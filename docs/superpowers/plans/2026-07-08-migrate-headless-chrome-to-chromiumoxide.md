# Migrate headless_chrome to chromiumoxide Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `headless_chrome` dependency with `chromiumoxide` while preserving the existing synchronous, multi-threaded browser automation behavior for Bing Rewards.

**Architecture:** Keep the existing sync thread-per-account structure. Each `BingBot` owns a `tokio::Runtime`, a `chromiumoxide::Browser`, its polling `Handler` task, and the active `Page`. Every `chromiumoxide` async call is driven to completion with `runtime.block_on(...)`, so `src/bing/pc.rs` and `src/bing/mod.rs` keep their current control flow. No synthetic wrapper types are introduced; call sites use `chromiumoxide` APIs directly.

**Tech Stack:** Rust 2024, `chromiumoxide = "0.9"`, `tokio = { version = "1", features = ["full"] }`, `futures = "0.3"` (for `StreamExt` on the handler).

## Global Constraints

- `chromiumoxide` only supports the `tokio` runtime.
- All `chromiumoxide` page/browser operations are `async`; the existing codebase is synchronous, so each account thread needs its own `tokio::Runtime` and must `block_on` the futures.
- The `Handler` stream must be polled continuously while the browser is alive; spawn it with `tokio::spawn` and abort/await it during browser shutdown.
- Keep changes scoped to `src/bing/*`; `src/hot_searches/*`, `src/random.rs`, and `src/main.rs` do not use browser APIs.
- Do not change retry logic, sleep logic, search logic, or config parsing; only replace browser calls.
- Preserve the existing `BingBot` / `BrowserPool` ownership model: a bot is checked out, a browser is launched, the account is processed, and the browser is closed when the bot is returned.
- Verification must run `cargo check`, `cargo test`, and a real run against the account in `config.json` (test credentials already provided).

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `Cargo.toml` | Dependency manifest | Remove `headless_chrome`; add `chromiumoxide = "0.9"` and `futures = "0.3"`. |
| `src/bing/browser_pool.rs` | Browser pool and `BingBot` state | Store `Browser`, `Page`, `Runtime`, and handler task handle instead of `headless_chrome::Browser`. Update `Drop`. |
| `src/bing/mod.rs` | Orchestration, launch options, shared helpers | Replace `headless_chrome` imports and helpers (`default_options_builder`, `get_one_tab`, `close_tab`, `shot_when_failed`) with `chromiumoxide` equivalents. |
| `src/bing/pc.rs` | PC account processing | Replace `Tab`/`Browser`/`Element` calls with `Page`/`Browser`/`Element` calls from `chromiumoxide`; wrap async calls with the runtime stored in `BingBot`. |
| `src/bing/retry.rs` | Retry trait | No changes. |

---

## Task 1: Switch dependencies

**Files:**
- Modify: `Cargo.toml:8-9`

**Interfaces:**
- Consumes: none.
- Produces: `chromiumoxide` and `futures` available to the crate; `headless_chrome` removed.

- [ ] **Step 1: Update `Cargo.toml`**

```toml
[dependencies]
chromiumoxide = "0.9"
futures = "0.3"
tempfile = "3.3"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt", "env-filter"] }
tracing-appender = "0.2"
reqwest = { version = "0.13", features = ["json", "rustls"] }
anyhow = "1.0"
rand = "0.10"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
crossbeam = "0.8.4"
croner = "3.0.1"
chrono = "0.4"
linkme = "0.3"
const_format = "0.2.35"
parking_lot = { version = "0.12.5", features = ["arc_lock", "send_guard"] }
regex = "1.12"
tokio = { version = "1.0", features = ["full"] }
```

- [ ] **Step 2: Verify dependency resolution**

Run: `cargo check --message-format=short`
Expected: fails with compile errors in `src/bing/*` because `headless_chrome` types are gone, but dependency download succeeds.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "deps: replace headless_chrome with chromiumoxide 0.9"
```

---

## Task 2: Adapt `BingBot` to own a chromiumoxide runtime

**Files:**
- Modify: `src/bing/browser_pool.rs:1-78`

**Interfaces:**
- Consumes: `chromiumoxide::browser::Browser`, `tokio::runtime::Runtime`.
- Produces: `BingBot` exposes `browser: Option<Browser>`, `page: Option<Page>`, `runtime: Option<Arc<Runtime>>`, `handler_task: Option<JoinHandle<()>>`, plus existing `temp_dir`, `store_local`, `account`, `browser_path`, `proxy`. `get_browser()` returns `&mut Browser`. Add `get_page()` returning `&mut Page`.

- [ ] **Step 1: Update `src/bing/browser_pool.rs`**

```rust
use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
    thread::sleep,
    time::Duration,
};

use anyhow::{Result, anyhow};
use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use parking_lot::{Condvar, Mutex};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tracing::{debug, error};

/// 需要保证 temp_dir 的生命周期长于 browser
#[derive(Default)]
pub(crate) struct BingBot {
    pub(crate) browser: Option<Browser>,
    pub(crate) page: Option<Page>,
    pub(crate) temp_dir: Option<tempfile::TempDir>,
    pub(crate) store_local: bool,
    pub(crate) account: String,
    pub(crate) browser_path: Option<String>,
    pub(crate) proxy: Option<String>,
    pub(crate) runtime: Option<Arc<Runtime>>,
    pub(crate) handler_task: Option<JoinHandle<()>>,
}

impl BingBot {
    pub(crate) fn get_browser(&mut self) -> anyhow::Result<&mut Browser> {
        self.browser.as_mut().ok_or(anyhow!("浏览器未启动"))
    }

    pub(crate) fn get_page(&mut self) -> anyhow::Result<&mut Page> {
        self.page.as_mut().ok_or(anyhow!("页面未打开"))
    }

    pub(crate) fn get_runtime(&self) -> anyhow::Result<Arc<Runtime>> {
        self.runtime.clone().ok_or(anyhow!("运行时未初始化"))
    }
}

impl Drop for BingBot {
    fn drop(&mut self) {
        if let (Some(browser), Some(runtime), Some(task)) =
            (self.browser.take(), self.runtime.take(), self.handler_task.take())
        {
            let _ = runtime.block_on(async {
                let _ = browser.close().await;
            });
            task.abort();
        }
        if let Some(dir) = self.temp_dir.take() {
            sleep(Duration::from_secs(3));
            drop(dir);
        }
    }
}

pub(crate) struct BrowserPool {
    cond: Condvar,
    pool: Mutex<Vec<BingBot>>,
}

pub(crate) struct PoolWrapper<'a> {
    pool: &'a BrowserPool,
    bot: Option<BingBot>,
}

impl<'a> Deref for PoolWrapper<'a> {
    type Target = BingBot;
    fn deref(&self) -> &Self::Target {
        self.bot.as_ref().expect("浏览器实例丢失")
    }
}

impl<'a> DerefMut for PoolWrapper<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.bot.as_mut().expect("浏览器实例丢失")
    }
}

impl<'a> Drop for PoolWrapper<'a> {
    fn drop(&mut self) {
        let Some(mut bot) = self.bot.take() else {
            error!("浏览器池中的浏览器实例丢失！");
            self.pool.cond.notify_one();
            return;
        };

        debug!("归还浏览器实例到浏览器池：{}", bot.account);
        if let (Some(browser), Some(runtime), Some(task)) =
            (bot.browser.take(), bot.runtime.take(), bot.handler_task.take())
        {
            let _ = runtime.block_on(async {
                let _ = browser.close().await;
            });
            task.abort();
        }
        if let Some(t) = bot.temp_dir.take() {
            sleep(Duration::from_secs(3));
            drop(t);
        }
        self.pool.pool.lock().push(bot);
        self.pool.cond.notify_one();
    }
}

impl BrowserPool {
    pub(crate) fn new(cnt: usize) -> Self {
        Self {
            cond: Condvar::new(),
            pool: Mutex::new({
                let mut v = Vec::with_capacity(cnt);
                for _ in 0..cnt {
                    v.push(BingBot::default());
                }
                v
            }),
        }
    }

    pub(crate) fn get_bot<'a>(&'a self) -> PoolWrapper<'a> {
        let mut pool = self.pool.lock();
        loop {
            if let Some(bot) = pool.pop() {
                return PoolWrapper {
                    pool: self,
                    bot: Some(bot),
                };
            }
            self.cond.wait(&mut pool);
        }
    }
}
```

- [ ] **Step 2: Run cargo check**

Run: `cargo check --message-format=short`
Expected: compiles `browser_pool.rs` with errors only in `mod.rs` / `pc.rs`.

- [ ] **Step 3: Commit**

```bash
git add src/bing/browser_pool.rs
git commit -m "refactor: adapt BingBot to own chromiumoxide runtime and page"
```

---

## Task 3: Rewrite browser launch and shared helpers in `src/bing/mod.rs`

**Files:**
- Modify: `src/bing/mod.rs:1-322`

**Interfaces:**
- Consumes: `chromiumoxide::browser::{Browser, BrowserConfig}`; `chromiumoxide::page::Page`; `tokio::runtime::Runtime`; `futures::StreamExt`.
- Produces: `default_browser_config(args, browser_path, user_dir, proxy) -> Result<BrowserConfig>`; `get_one_page(browser, runtime) -> Result<Page>`; `close_tab(before_pages, browser, runtime) -> Result<()>`; `shot_when_failed(page, runtime, prefix, account)`; helper `block_on_with_runtime` not needed because callers have `runtime`.

- [ ] **Step 1: Replace imports and constants at the top of `src/bing/mod.rs`**

```rust
use std::{
    ffi::OsStr,
    fs,
    path::Path,
    str::FromStr,
    sync::Arc,
    thread::{Builder, sleep},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use chrono::Local;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::{Page, ScreenshotParams};
use chromiumoxide_cdp::cdp::browser_protocol::page::CaptureScreenshotFormat;
use futures::StreamExt;
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

use crate::{
    bing::{
        browser_pool::{BingBot, BrowserPool},
        retry::Retryable,
    },
    hot_searches,
};

mod browser_pool;
mod pc;
mod retry;

#[cfg(feature = "debug")]
const HEADLESS: bool = false;
#[cfg(not(feature = "debug"))]
const HEADLESS: bool = true;
const BING_URL: &str = "https://cn.bing.com/";
const REWARDS_URL: &str = "https://rewards.bing.com/earn";
const REWARDS_URL_DS: &str = "https://rewards.bing.com/dashboard";
const SLEEP_RANGE: std::ops::Range<u64> = 30..80;
const GAP_RANGE: std::ops::Range<u64> = 400..1000;
const GAP_NUM: u32 = 4;
```

- [ ] **Step 2: Replace `default_options_builder` and `get_one_tab` / `close_tab` / `shot_when_failed`**

Remove the old `get_today_rewards`, `shot_when_failed`, `close_tab`, `get_one_tab`, and `default_options_builder` functions and replace with:

```rust
fn default_browser_config<'a>(
    args: Vec<&'a OsStr>,
    browser_path: &'a Option<String>,
    user_dir: Option<std::path::PathBuf>,
    proxy: &'a Option<String>,
) -> Result<BrowserConfig> {
    let mut config = BrowserConfig::builder();
    config = config.headless(HEADLESS);
    config = config.no_sandbox();
    config = config.window_size(1920, 1080);
    if let Some(path) = browser_path {
        config = config.chrome_executable(std::path::PathBuf::from(path));
    }
    if let Some(dir) = user_dir {
        config = config.user_data_dir(dir);
    }

    let mut chrome_args = vec![
        OsStr::new("--disable-dev-shm-usage"),
        OsStr::new("--disable-extensions"),
        OsStr::new("--disable-blink-features=AutomationControlled"),
        OsStr::new("--allow-running-insecure-content"),
        OsStr::new("--disable-plugins"),
        OsStr::new("--disable-images"),
        OsStr::new("--disable-web-security"),
        OsStr::new("--mute-audio"),
        OsStr::new("--no-first-run"),
        OsStr::new("--no-default-browser-check"),
    ];
    chrome_args.extend(args);

    if let Some(proxy) = proxy {
        chrome_args.push(OsStr::new("--proxy-server"));
        chrome_args.push(OsStr::new(proxy));
    }

    config = config.args(chrome_args);

    config
        .build()
        .map_err(|e| anyhow!("构建浏览器启动选项失败：{}", e))
}

fn get_one_page(browser: &Browser, runtime: &Runtime) -> Result<Page> {
    let pages = runtime.block_on(browser.pages())?;
    if let Some(page) = pages.into_iter().next() {
        Ok(page)
    } else {
        runtime
            .block_on(browser.new_page("about:blank"))
            .map_err(|e| anyhow!("创建新页面失败：{}", e))
    }
}

fn close_tab(
    before_pages: Vec<Page>,
    browser: &Browser,
    runtime: &Runtime,
) -> Result<()> {
    let after_pages = runtime.block_on(browser.pages())?;
    for page in after_pages {
        let target_id = page.target_id();
        if !before_pages
            .iter()
            .any(|p| p.target_id() == target_id)
        {
            info!("发现新打开的标签页，准备关闭");
            runtime.block_on(page.close())?;
            info!("标签页关闭成功");
        }
    }
    Ok(())
}

fn shot_when_failed(page: &Page, runtime: &Runtime, prefix: &str, account: &str) {
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .build();
    if let Ok(png) = runtime.block_on(page.screenshot(params)) {
        std::fs::create_dir_all("failed").ok();
        let file_name = format!("{prefix}_failure_{account}.png");
        if let Err(e) = std::fs::write(Path::new("failed").join(&file_name), &png) {
            warn!("保存失败截图 failed/{file_name} 失败: {e}");
        } else {
            info!("失败截图已保存为 {file_name}");
        }
    }
}
```

- [ ] **Step 3: Update `process_account` call site**

No change needed to the body of `process_account` in `mod.rs`; it already calls `bot.new_pc_browser(...)` and `pc::process_account(...)`. The helper signatures in `pc.rs` will change from `&Tab` to `&Page`.

- [ ] **Step 4: Run cargo check**

Run: `cargo check --message-format=short`
Expected: errors in `src/bing/pc.rs` because `headless_chrome` calls remain, but `mod.rs` compiles.

- [ ] **Step 5: Commit**

```bash
git add src/bing/mod.rs
git commit -m "refactor: rewrite shared browser helpers for chromiumoxide"
```

---

## Task 4: Launch browser in `src/bing/pc.rs`

**Files:**
- Modify: `src/bing/pc.rs:1-120`

**Interfaces:**
- Consumes: `chromiumoxide::browser::Browser`; `chromiumoxide::page::Page`; `BingBot` now stores `runtime`, `browser`, `page`, `handler_task`.
- Produces: `new_pc_browser` builds `BrowserConfig`, calls `Browser::launch`, spawns the handler task, and stores the first `Page` in the bot.

- [ ] **Step 1: Update imports and `BingBot` impl block**

```rust
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use futures::StreamExt;
use rand::seq::IndexedRandom;
use tracing::{debug, info, warn};

use crate::{
    bing::{
        BING_URL, BingBot, GAP_NUM, GAP_RANGE, REWARDS_URL, REWARDS_URL_DS, SLEEP_RANGE, close_tab,
        default_browser_config, get_one_page, retry::Retryable, shot_when_failed,
    },
    random::ExpectedNTrigger,
};

static PC_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
const MAX_PC_SEARCH_TIMES: usize = 20;

impl BingBot {
    pub(crate) fn new_pc_browser(
        &mut self,
        store_local: bool,
        account: &str,
        browser_path: &Option<String>,
        proxy: &Option<String>,
    ) -> Result<()> {
        self.close_browser();

        let (temp_dir, user_dir) = if store_local {
            (None, Some(prepare_local_user_data_dir(account)?))
        } else {
            std::fs::create_dir_all("./tmp")?;
            let dir = tempfile::TempDir::new_in("./tmp")?;
            let path = dir.path().to_path_buf();
            (Some(dir), Some(path))
        };

        let config = build_pc_config(browser_path, proxy, user_dir)?;

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| anyhow!("创建 tokio 运行时失败：{}", e))?;
        let runtime = Arc::new(runtime);

        let (browser, mut handler) = runtime
            .block_on(Browser::launch(config))
            .map_err(|e| anyhow!("启动浏览器失败：{}", e))?;

        let handler_task = runtime.spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        let page = get_one_page(&browser, &runtime)?;

        self.browser = Some(browser);
        self.page = Some(page);
        self.temp_dir = temp_dir;
        self.store_local = store_local;
        self.account = account.to_string();
        self.browser_path = browser_path.clone();
        self.proxy = proxy.clone();
        self.runtime = Some(runtime);
        self.handler_task = Some(handler_task);
        Ok(())
    }

    pub(crate) fn restart_pc_browser(&mut self) -> Result<()> {
        self.close_browser();
        self.new_pc_browser(self.store_local, &self.account, &self.browser_path, &self.proxy)
    }

    fn close_browser(&mut self) {
        if let (Some(browser), Some(runtime), Some(task)) =
            (self.browser.take(), self.runtime.take(), self.handler_task.take())
        {
            let _ = runtime.block_on(async {
                let _ = browser.close().await;
            });
            task.abort();
        }
        self.page.take();
    }
}
```

- [ ] **Step 2: Replace `build_pc_options` with chromiumoxide config builder**

```rust
fn build_pc_config(
    browser_path: &Option<String>,
    proxy: &Option<String>,
    user_dir: Option<PathBuf>,
) -> Result<chromiumoxide::browser::BrowserConfig> {
    default_browser_config(
        vec![OsStr::new(const_format::formatcp!("--user-agent='{}'", PC_USER_AGENT))],
        browser_path,
        user_dir,
        proxy,
    )
}
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check --message-format=short`
Expected: compiles the top of `pc.rs`; remaining errors are in `process_account` and helper functions below.

- [ ] **Step 4: Commit**

```bash
git add src/bing/pc.rs
git commit -m "refactor: launch chromiumoxide browser per BingBot"
```

---

## Task 5: Migrate account processing and search loop

**Files:**
- Modify: `src/bing/pc.rs:121-232`

**Interfaces:**
- Consumes: `Page` methods `goto`, `wait_for_navigation`, `find_element`, `find_xpath`, `screenshot`, `reload`, `evaluate_function`, `close`, `url`.
- Produces: `process_account(email, password, browser_bot: &mut BingBot) -> Result<()>` keeps the same signature but operates on `bot.get_page()`.

- [ ] **Step 1: Update `process_account` signature and body**

```rust
/// 为什么是 &mut BingBot 而不是 &BingBot
///
/// 其实是借用了 rust 单一所有权的特性，保证同一时间只有一个可变引用在使用 browser
pub(crate) fn process_account(
    email: &str,
    password: &str,
    browser_bot: &mut BingBot,
) -> Result<()> {
    info!("开始登录Bing账号: {}", email);
    let runtime = browser_bot.get_runtime()?;
    let page = browser_bot.get_page()?;
    (|| {
        if !check_login_status(page, &runtime)? {
            login_bing(email, password, page, &runtime)?;
            sleep(Duration::from_secs(5));
            if !check_login_status(page, &runtime)? {
                return Err(anyhow!("登录后检查状态仍然未登录"));
            }
        } else {
            info!("账号 {} 已登录，无需重复登录", email);
        }

        Ok(())
    })
    .retry(3)
    .inspect_err(|_| {
        shot_when_failed(page, &runtime, "login", email);
    })?;

    sleep(Duration::from_secs(5));

    info!("开始尝试点击卡片");
    let browser = browser_bot.get_browser()?;
    let _ = click_rewards(browser, page, &runtime).inspect_err(|_| {
        shot_when_failed(page, &runtime, "click_rewards", email);
    });

    sleep(Duration::from_secs(5));

    info!("开始进行搜索任务");
    search(browser_bot, email).inspect_err(|_| {
        let page = browser_bot.get_page().expect("页面已丢失");
        let runtime = browser_bot.get_runtime().expect("运行时已丢失");
        shot_when_failed(page, &runtime, "search", email);
    })?;

    info!("{} 账号处理完成", email);
    Ok(())
}
```

- [ ] **Step 2: Update `search` to use `BingBot` for page/runtime access**

```rust
fn search(browser_bot: &mut BingBot, email: &str) -> Result<()> {
    let search_words = crate::hot_searches::get_hot_words(MAX_PC_SEARCH_TIMES);
    let runtime = browser_bot.get_runtime()?;

    let mut trigger = ExpectedNTrigger::new(GAP_NUM);
    for (i, word) in search_words.into_iter().enumerate() {
        let page = browser_bot.get_page()?;
        let sleep_time = if trigger.next() {
            match get_pc_search_process(page, &runtime) {
                Ok((cur_points, max_points)) => {
                    info!(
                        "账号 {email} 当前搜索积分: {cur_points}，今日最大搜索积分: {max_points}"
                    );
                    if cur_points >= max_points {
                        info!("账号 {email} 今日搜索积分已达上限，结束搜索任务");
                        break;
                    }
                }
                Err(e) => {
                    shot_when_failed(page, &runtime, "rewards_get_failed", email);
                    warn!("获取账号 {email} 积分详情失败: {e}");
                }
            }
            rand::random_range(GAP_RANGE)
        } else {
            rand::random_range(SLEEP_RANGE)
        };

        info!(
            "{} {} 秒后开始第 {} 次搜索：{}",
            email,
            sleep_time,
            i + 1,
            word
        );
        const MAX_SLEEP_TIME: u64 = 30;
        if sleep_time < MAX_SLEEP_TIME {
            sleep(Duration::from_secs(sleep_time));
        } else {
            let mut slept = 0;
            while slept < sleep_time {
                let sleep_chunk = std::cmp::min(MAX_SLEEP_TIME, sleep_time - slept);
                sleep(Duration::from_secs(sleep_chunk));
                // 空转防止 timeout
                let _ = runtime.block_on(page.reload());
                slept += sleep_chunk;
            }
        }

        (|| {
            let page = browser_bot.get_page()?;
            let browser = browser_bot.get_browser()?;
            perform_search_and_click(browser, page, &runtime, &word).inspect_err(|_| {
                let _ = (|| -> Result<()> {
                    browser_bot.restart_pc_browser()?;
                    Ok(())
                })();
            })?;
            info!("第 {} 次搜索完成", i + 1);
            Ok(())
        })
        .retry(3)?;
    }

    Ok(())
}
```

- [ ] **Step 3: Run cargo check**

Run: `cargo check --message-format=short`
Expected: errors in `perform_search_and_click` and remaining helpers.

- [ ] **Step 4: Commit**

```bash
git add src/bing/pc.rs
git commit -m "refactor: migrate account processing and search loop to chromiumoxide"
```

---

## Task 6: Migrate search click, rewards click, and login helpers

**Files:**
- Modify: `src/bing/pc.rs:233-466`

**Interfaces:**
- Consumes: `Page::goto`, `Page::wait_for_navigation`, `Page::find_element`, `Page::find_xpath`, `Page::evaluate_function`, `Element::click`, `Element::type_str`, `Element::inner_text`, `Element::attribute`, `Browser::pages`, `Page::close`, `Page::url`.
- Produces: all helper functions keep the same logical behavior with chromiumoxide calls.

- [ ] **Step 1: Rewrite `perform_search_and_click`, `click_rewards`, `click_earn`, `click_daily_set`**

```rust
fn perform_search_and_click(
    browser: &Browser,
    page: &Page,
    runtime: &Runtime,
    word: &str,
) -> Result<()> {
    let before_pages = runtime.block_on(browser.pages())?;

    let search_url = reqwest::Url::parse_with_params(
        "https://cn.bing.com/search",
        [("q", word), ("PC", "U316"), ("FORM", "CHROMN")],
    )?;
    runtime.block_on(page.goto(search_url.as_str()))?;
    runtime.block_on(page.wait_for_navigation())?;

    sleep(Duration::from_secs(rand::random_range(1..4)));

    let search_res = runtime.block_on(page.find_element("#b_results"))?;

    runtime
        .block_on(search_res.find_element("li.b_algo"))
        .map_err(|e| anyhow!(format!("没有找到搜索结果：{e}")))?;

    let all_res = runtime.block_on(search_res.find_elements("li.b_algo"))?;

    let ele = all_res
        .choose(&mut rand::rng())
        .ok_or(anyhow!("没有找到搜索结果"))?;

    runtime
        .block_on(ele.click())
        .map_err(|e| anyhow!(format!("点击搜索结果失败：{e}")))?;

    sleep(Duration::from_secs(rand::random_range(5..10)));

    close_tab(before_pages, browser, runtime)?;

    Ok(())
}

fn click_rewards(browser: &Browser, page: &Page, runtime: &Runtime) -> Result<()> {
    (|| click_daily_set(browser, page, runtime))
        .retry(3)
        .inspect_err(|e| {
            warn!("点击奖励卡片失败: {e}");
        })?;
    (|| click_earn(browser, page, runtime)).retry(3).inspect_err(|e| {
        warn!("点击奖励卡片失败: {e}");
    })?;

    info!("卡片点击完成");
    Ok(())
}

fn click_earn(browser: &Browser, page: &Page, runtime: &Runtime) -> Result<()> {
    runtime.block_on(page.goto(REWARDS_URL))?;
    runtime.block_on(page.wait_for_navigation())?;

    let ele = runtime.block_on(page.find_element("#moreactivities > div > div:nth-of-type(2)"))?;
    let ele = runtime.block_on(ele.find_elements("a"))?;
    info!("找到 {} 个奖励卡片，准备点击", ele.len());

    for card in ele {
        let text = runtime
            .block_on(card.inner_text())?
            .unwrap_or_default();
        if !text.contains("+") {
            continue;
        }

        let before_pages = runtime.block_on(browser.pages())?;
        match runtime.block_on(card.evaluate_function("function() { this.click(); }")) {
            Ok(_) => info!("通过 JS 点击奖励卡片成功"),
            Err(e) => warn!("通过 JS 点击奖励卡片失败：{}", e),
        }
        sleep(Duration::from_secs(5));
        let _ = close_tab(before_pages, browser, runtime);
        sleep(Duration::from_secs(1));
    }

    Ok(())
}

fn click_daily_set(browser: &Browser, page: &Page, runtime: &Runtime) -> Result<()> {
    runtime.block_on(page.goto(REWARDS_URL_DS))?;
    runtime.block_on(page.wait_for_navigation())?;
    let ele = runtime.block_on(page.find_element("#dailyset > div > div:nth-of-type(2)"))?;
    let ele = runtime.block_on(ele.find_elements("a"))?;
    info!("找到 {} 个每日任务卡片，准备点击", ele.len());

    for card in ele {
        let before_pages = runtime.block_on(browser.pages())?;
        match runtime.block_on(card.evaluate_function("function() { this.click(); }")) {
            Ok(_) => info!("通过 JS 点击每日任务卡片成功"),
            Err(e) => warn!("通过 JS 点击每日任务卡片失败：{}", e),
        }
        sleep(Duration::from_secs(5));
        let _ = close_tab(before_pages, browser, runtime);
        sleep(Duration::from_secs(1));
    }

    Ok(())
}
```

- [ ] **Step 2: Rewrite `login_bing`**

```rust
pub(super) fn login_bing(
    email: &str,
    password: &str,
    page: &Page,
    runtime: &Runtime,
) -> Result<()> {
    runtime.block_on(page.activate())?;
    runtime.block_on(page.goto(BING_URL))?;
    runtime.block_on(page.wait_for_navigation())?;
    runtime.block_on(page.reload())?;
    runtime.block_on(page.wait_for_navigation())?;

    if let Err(e) = (|| {
        runtime.block_on(page.reload())?;
        runtime.block_on(page.wait_for_navigation())?;
        sleep(Duration::from_secs(2));
        click_login_button(page, runtime)
    })
    .retry(3)
    {
        let url = runtime
            .block_on(page.url())?
            .unwrap_or_default();
        debug!("当前页面：{}", url);
        warn!("点击登录按钮失败: {}", e);
        return Err(e);
    }

    info!("登录按钮点击成功，准备输入账号密码");
    let email_input = runtime
        .block_on(page.find_xpath(concat!(
            "//input[@type='email' or @name='loginfmt']",
            "|//input[@id='usernameEntry']",
        )))
        .map_err(|e| anyhow::Error::msg(format!("寻找账号输入位置有误：{}", e)))?;

    runtime.block_on(email_input.type_str(email))?;

    info!("账号输入成功，准备点击下一步");

    let next_button = runtime.block_on(page.find_xpath(concat!(
        "//button[@type='submit']",
        "|//button[text()='下一步']",
    )))?;

    runtime.block_on(next_button.click())?;

    let password_input = loop {
        match runtime.block_on(page.find_xpath(concat!(
            "//input[@type='password' or @name='passwd']",
            "|//input[@id='passwordInput']",
        ))) {
            Ok(input) => break input,
            Err(_e) => {
                let _ = runtime
                    .block_on(page.find_xpath("//*[text()='暂时跳过']"))
                    .and_then(|button| runtime.block_on(button.click()).map(|_| ()));

                let _ = runtime
                    .block_on(page.find_xpath(concat!(
                        "//span[@role='button' and (text()='其他登录方法' or text()='Other ways to sign in')]",
                        "|//*[text()='其他登录方法']"
                    )))
                    .and_then(|button| runtime.block_on(button.click()).map(|_| ()));

                let button = runtime.block_on(page.find_xpath(concat!(
                    "//*[text()='使用密码']",
                    "|//*[text()='Use your password']",
                    "|//button[contains(text(), '使用密码')]",
                    "|//button[contains(text(), 'Use your password')]",
                    "|//a[contains(text(), '使用密码')]",
                    "|//a[contains(text(), 'Use your password')]",
                )))?;

                runtime.block_on(button.click())?;
            }
        }
    };

    runtime.block_on(password_input.type_str(password))?;

    info!("密码输入成功，准备点击登录");

    let sign_in_button = runtime.block_on(page.find_xpath(concat!(
        "//button[@type='submit']",
        "|//button[text()='登录']",
        "|//button[text()='Sign in']",
        "|//button[text()='下一步']",
        "|//button[text()='Next']",
    )))?;

    runtime.block_on(sign_in_button.click())?;

    info!("登录按钮点击成功");

    if let Ok(Some(_)) = runtime.block_on(page.find_xpath(
        "//*[contains(text(), '保持登录状态')]|//*[contains(text(), 'Stay signed in')]",
    )) {
        if let Ok(ok_button) = runtime.block_on(page.find_xpath(concat!(
            "//button[text()='是']",
            "|//button[text()='Yes']",
        ))) {
            let _ = runtime.block_on(ok_button.click());
        }
    } else if let Ok(ok_button) = runtime.block_on(page.find_xpath(concat!(
        "//button[text()='是']",
        "|//button[text()='Yes']",
    ))) {
        let _ = runtime.block_on(ok_button.click());
    }

    info!("登录流程完成");
    Ok(())
}
```

- [ ] **Step 3: Rewrite `check_login_status`, `click_login_button`, `get_pc_search_process`**

```rust
pub(super) fn check_login_status(page: &Page, runtime: &Runtime) -> Result<bool> {
    runtime.block_on(page.goto(BING_URL))?;
    runtime.block_on(page.wait_for_navigation())?;
    runtime.block_on(page.reload())?;
    sleep(Duration::from_secs(2));

    match runtime.block_on(tokio::time::timeout(
        Duration::from_secs(25),
        page.find_element("#id_s"),
    )) {
        Ok(Ok(ele)) => {
            let status = runtime.block_on(ele.attribute("aria-hidden"))?;
            match status.as_deref() {
                Some("true") => Ok(true),
                Some("false") => Ok(false),
                None => Err(anyhow!("没有找到")),
                _ => Err(anyhow!("未知状态")),
            }
        }
        _ => {
            anyhow::bail!("没有找到登录状态元素")
        }
    }
}

fn click_login_button(page: &Page, runtime: &Runtime) -> Result<()> {
    let login_button = runtime.block_on(tokio::time::timeout(
        Duration::from_secs(10),
        page.find_xpath(concat!(
            "//span[@id='id_s']",
            "|//*[@id='id_a']",
            "|//a[@id='id_l']",
            "|//a[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//a[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//a[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
            "|//button[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//button[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//button[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
        )),
    ))??;

    info!("找到登录按钮，准备点击");
    sleep(Duration::from_secs(2));
    runtime.block_on(login_button.click())?;
    runtime.block_on(page.wait_for_navigation())?;
    Ok(())
}

fn get_pc_search_process(page: &Page, runtime: &Runtime) -> Result<(u32, u32)> {
    runtime.block_on(page.goto("https://rewards.bing.com/earn"))?;
    let _ = runtime.block_on(page.wait_for_navigation());
    let ele = runtime.block_on(page.find_element("#shell > div.grow > div > main > div"))?;
    let button = runtime.block_on(ele.find_element("button"))?;
    runtime.block_on(button.click())?;
    sleep(Duration::from_secs(3));
    let ele = runtime.block_on(page.find_xpath(
        "/html/body/div[3]/div/section/div/div[2]/div/div[1]/div[2]/div[4]",
    ))?;
    let text = runtime.block_on(ele.inner_text())?.unwrap_or_default();

    let (cur_points, max_points) = parse_point(&text)?;
    Ok((cur_points, max_points))
}
```

- [ ] **Step 4: Validate login entry point against current Bing DOM**

Playwright check result (already done): the current `https://cn.bing.com/` homepage does **not** expose the legacy login elements (`#id_s`, `#id_a`, `#id_l`) even after reload. The login link is available from the search results page header instead. If `click_login_button` fails during real-account verification, replace the `BING_URL` entry point in `login_bing` with a search results URL such as `https://cn.bing.com/search?q=bing`, and update `click_login_button` to click the visible header login link (`//a[contains(text(),'登录')]` inside the search results header) before the Microsoft account pages appear.

Run: `cargo check --message-format=short`
Expected: compiles.

- [ ] **Step 5: Run cargo check**

Run: `cargo check --message-format=short`
Expected: zero errors or only minor type mismatches that must be fixed before proceeding.

- [ ] **Step 6: Commit**

```bash
git add src/bing/pc.rs
git commit -m "refactor: migrate search, login and rewards helpers to chromiumoxide"
```

---

## Task 7: Compile, test, and fix type mismatches

**Files:**
- Modify: any files with remaining compile errors.

**Interfaces:**
- Consumes: full crate.
- Produces: passing `cargo check` and `cargo test`.

- [ ] **Step 1: Run full check**

Run: `cargo check --all-targets --message-format=short`
Expected: no errors.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib`
Expected: existing tests pass (`parse_point`, `ExpectedNTrigger`, hot search unit tests). Browser-based tests are not added in this plan.

- [ ] **Step 3: Fix any remaining issues**

Common expected fixes:
- If `Page::activate()` does not exist, replace with `page.execute(chromiumoxide_cdp::cdp::browser_protocol::page::BringToFrontParams::default())`.
- If `Page::reload()` does not exist or does not ignore cache, replace with `page.execute(chromiumoxide_cdp::cdp::browser_protocol::page::ReloadParams::builder().ignore_cache(true).build())`.
- If `Element::evaluate_function` does not exist for JS clicks, replace with `page.evaluate_function("(el) => el.click()", vec![element.remote_object_id_or_node_id...])` or use `element.click()` directly.
- If `Element::inner_text()` returns `String` instead of `Option<String>`, remove `.unwrap_or_default()`.
- If `Page::screenshot` expects `ScreenshotParams` from `chromiumoxide_cdp`, import from `chromiumoxide_cdp::cdp::browser_protocol::page`.
- If `Page::url()` returns `String` instead of `Option<String>`, adjust accordingly.
- If `Page::target_id()` does not exist, compare pages by object identity or store target IDs via `page.execute(GetTargetInfoParams::default())`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "fix: resolve chromiumoxide type mismatches"
```

---

## Task 8: Real-world verification with `config.json`

**Files:**
- Uses: `config.json` (already present, contains test account).

**Interfaces:**
- Consumes: compiled binary.
- Produces: confirmed successful login/search/rewards behavior; screenshots in `failed/` only if something breaks.

- [ ] **Step 1: Build release binary**

Run: `cargo build --release`
Expected: succeeds.

- [ ] **Step 2: Run against the test account**

Run: `./target/release/bing-auto-reward`
Expected: account logs in, searches run, rewards cards are clicked, and process completes without panic. Pay special attention to the first login step: if it fails to find the login button on the Bing homepage, apply the selector update described in Task 6 Step 4 (use the search results page header login link). If the run fails, inspect `failed/` screenshots and logs in `log/`, fix the offending call, and rerun.

- [ ] **Step 3: Commit any verification fixes**

```bash
git add -A
git commit -m "fix: adjust behavior from real account verification"
```

---

## Self-Review

**1. Spec coverage:**
- Remove `headless_chrome` dependency: covered in Task 1.
- Keep existing multi-thread sync structure: covered by per-bot `Runtime` in Tasks 2 and 4.
- Preserve `BingBot` / `BrowserPool` lifecycle: covered in Task 2.
- Preserve login, search, rewards logic: covered in Tasks 5 and 6; login entry point selector validated via Playwright and fallback described in Task 6 Step 4.
- Preserve screenshot on failure: covered in Task 3.
- Preserve launch options (headless, args, proxy, user data dir, window size): covered in Task 3.
- Verify with real account: covered in Task 8.

**2. Placeholder scan:**
- No "TBD", "TODO", or "implement later".
- Every step includes concrete code.
- Every command includes expected output.
- Type mismatches are enumerated explicitly in Task 7.

**3. Type consistency:**
- `BingBot` fields are consistent across `browser_pool.rs`, `mod.rs`, and `pc.rs`.
- `Page`, `Browser`, `Runtime`, and `Element` come from `chromiumoxide` / `tokio` consistently.
- Helper signatures in `mod.rs` (`close_tab`, `get_one_page`, `shot_when_failed`) accept `Runtime` and `Page`/`Browser` as used in `pc.rs`.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-08-migrate-headless-chrome-to-chromiumoxide.md`.

Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
