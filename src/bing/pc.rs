use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use chromiumoxide::browser::Browser;
use chromiumoxide::element::Element;
use chromiumoxide::page::Page;
use chromiumoxide_cdp::cdp::browser_protocol::page::ReloadParams;
use futures::StreamExt;
use rand::seq::IndexedRandom;
use tracing::{debug, info, warn};

use crate::{
    bing::{
        BING_URL, BingBot, REWARDS_URL, REWARDS_URL_DS, close_tab, default_browser_config,
        get_one_page, shot_when_failed,
    },
    random::ExpectedNTrigger,
    user_agent::user_agent,
};

const MAX_PC_SEARCH_TIMES: usize = 20;
const SLEEP_RANGE: std::ops::Range<u64> = 30..80;
const GAP_RANGE: std::ops::Range<u64> = 400..1000;
const GAP_NUM: u32 = 4;
const LONG_ELEMENT_TIMEOUT: Duration = Duration::from_secs(40);
const ELEMENT_TIMEOUT: Duration = Duration::from_secs(20);
const PASSWORD_ELEMENT_TIMEOUT: Duration = Duration::from_secs(15);
const PAGE_SETTLE_DELAY: Duration = Duration::from_secs(3);
const ACTION_SETTLE_DELAY: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_secs(5);

impl BingBot {
    pub(crate) async fn new_pc_browser(
        &mut self,
        store_local: bool,
        account: &str,
        browser_path: &Option<String>,
        proxy: &Option<String>,
    ) -> Result<()> {
        self.close_browser().await;

        let (temp_dir, user_dir) = if store_local {
            (None, Some(prepare_local_user_data_dir(account)?))
        } else if let Some(dir) = self.temp_dir.take() {
            // 重启时复用同一个临时 profile，保留 cookie
            let path = dir.path().to_path_buf();
            (Some(dir), Some(path))
        } else {
            std::fs::create_dir_all("./tmp")?;
            let dir = tempfile::TempDir::new_in("./tmp")?;
            let path = dir.path().to_path_buf();
            (Some(dir), Some(path))
        };

        if let Some(dir) = user_dir.as_ref() {
            ensure_profile_unlocked(dir).await?;
        }

        let config = default_browser_config(browser_path, user_dir, proxy)?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| anyhow!("启动浏览器失败：{}", e))?;

        let handler_task = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        let page = get_one_page(&browser).await?;
        page.enable_stealth_mode_with_agent(user_agent())
            .await
            .map_err(|e| anyhow!("启用反检测模式失败：{}", e))?;

        self.browser = Some(browser);
        self.page = Some(page);
        self.temp_dir = temp_dir;
        self.store_local = store_local;
        self.account = account.to_string();
        self.browser_path = browser_path.clone();
        self.proxy = proxy.clone();
        self.handler_task = Some(handler_task);
        Ok(())
    }

    pub(crate) async fn restart_pc_browser(&mut self) -> Result<()> {
        let store_local = self.store_local;
        let account = self.account.clone();
        let browser_path = self.browser_path.clone();
        let proxy = self.proxy.clone();
        self.close_browser().await;
        self.new_pc_browser(store_local, &account, &browser_path, &proxy)
            .await
    }
}

fn prepare_local_user_data_dir(account: &str) -> Result<PathBuf> {
    std::fs::create_dir_all("./user-data")?;
    let user_data_dir = std::path::PathBuf::from(format!("./user-data/pc_{}", account));
    std::fs::create_dir_all(&user_data_dir)?;
    mark_user_data_last_used(&user_data_dir)?;
    Ok(user_data_dir)
}

fn mark_user_data_last_used(user_data_dir: &Path) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs();
    std::fs::write(user_data_dir.join(super::LAST_USED_MARKER), now.to_string())?;
    Ok(())
}

/// Chrome 用 `hostname-pid` 锁住 user-data-dir。进程异常退出后锁文件会残留，
/// 再次启动就会报 SingletonLock: File exists。
async fn ensure_profile_unlocked(user_data_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        unix_ensure_profile_unlocked(user_data_dir).await?;
    }
    #[cfg(not(unix))]
    {
        let _ = user_data_dir;
    }
    Ok(())
}

fn parse_chrome_singleton_lock(target: &str) -> Option<(String, u32)> {
    let (host, pid) = target.rsplit_once('-')?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), pid.parse().ok()?))
}

#[cfg(unix)]
const CHROME_SINGLETON_FILES: [&str; 3] = ["SingletonLock", "SingletonCookie", "SingletonSocket"];

#[cfg(unix)]
async fn unix_ensure_profile_unlocked(user_data_dir: &Path) -> Result<()> {
    if let Some((host, pid)) = read_chrome_singleton_lock(user_data_dir) {
        let same_host = current_hostname().is_some_and(|h| h.eq_ignore_ascii_case(&host));
        if same_host && is_pid_alive(pid) {
            warn!(
                "检测到浏览器仍占用 {}（pid={}），准备结束残留进程",
                user_data_dir.display(),
                pid
            );
            terminate_pid(pid, false);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
            while is_pid_alive(pid) && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if is_pid_alive(pid) {
                terminate_pid(pid, true);
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            if is_pid_alive(pid) {
                return Err(anyhow!(
                    "无法释放浏览器目录 {}，进程 {} 仍在运行",
                    user_data_dir.display(),
                    pid
                ));
            }
        }
    }
    remove_chrome_singleton_files(user_data_dir);
    Ok(())
}

#[cfg(unix)]
fn read_chrome_singleton_lock(user_data_dir: &Path) -> Option<(String, u32)> {
    let path = user_data_dir.join("SingletonLock");
    let target = if let Ok(link) = std::fs::read_link(&path) {
        link.to_string_lossy().into_owned()
    } else {
        std::fs::read_to_string(&path).ok()?
    };
    parse_chrome_singleton_lock(target.trim())
}

#[cfg(unix)]
fn current_hostname() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let host = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if host.is_empty() { None } else { Some(host) }
}

#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn terminate_pid(pid: u32, force: bool) {
    let signal = if force { "-KILL" } else { "-TERM" };
    let _ = std::process::Command::new("kill")
        .args([signal, &pid.to_string()])
        .status();
}

#[cfg(unix)]
fn remove_chrome_singleton_files(user_data_dir: &Path) {
    for name in CHROME_SINGLETON_FILES {
        let path = user_data_dir.join(name);
        if path.symlink_metadata().is_ok()
            && let Err(e) = std::fs::remove_file(&path)
        {
            warn!("删除 {} 失败: {}", path.display(), e);
        }
    }
}

/// 为什么是 &mut BingBot 而不是 &BingBot
///
/// 其实是借用了 rust 单一所有权的特性，保证同一时间只有一个可变引用在使用 browser
pub(crate) async fn process_account(
    email: &str,
    password: &str,
    browser_bot: &mut BingBot,
) -> Result<()> {
    info!("开始登录Bing账号: {}", email);
    let page = browser_bot.get_page()?;
    let mut login_last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match async {
            if !check_login_status(page).await? {
                login_bing(email, password, page).await?;
                tokio::time::sleep(Duration::from_secs(5)).await;
                if !check_login_status(page).await? {
                    return Err(anyhow!("登录后检查状态仍然未登录"));
                }
            } else {
                info!("账号 {} 已登录，无需重复登录", email);
            }
            Ok(())
        }
        .await
        {
            Ok(()) => {
                login_last_err = None;
                break;
            }
            Err(e) => {
                debug!("账号 {} 第 {} 次登录失败: {}", email, i + 1, e);
                login_last_err = Some(e);
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
    if let Some(e) = login_last_err {
        if let Ok(page) = browser_bot.get_page() {
            shot_when_failed(page, "login", email).await;
        }
        return Err(anyhow!("登录失败: {}", e));
    }

    tokio::time::sleep(Duration::from_secs(5)).await;

    info!("开始尝试点击卡片");
    let browser = browser_bot.get_browser()?;
    let page = browser_bot.get_page()?;
    if let Err(e) = click_rewards(browser, page, email, password).await {
        warn!("点击奖励卡片失败: {}", e);
        if let Ok(page) = browser_bot.get_page() {
            shot_when_failed(page, "click_rewards", email).await;
        }
    }

    tokio::time::sleep(Duration::from_secs(5)).await;

    info!("开始进行搜索任务");
    if let Err(e) = search(browser_bot, email).await {
        if let Ok(page) = browser_bot.get_page() {
            shot_when_failed(page, "search", email).await;
        }
        return Err(anyhow!("搜索任务失败: {}", e));
    }

    info!("{} 账号处理完成", email);
    Ok(())
}

async fn search(browser_bot: &mut BingBot, email: &str) -> Result<()> {
    let search_words = crate::hot_searches::get_hot_words(MAX_PC_SEARCH_TIMES);
    if search_words.is_empty() {
        return Err(anyhow!("热搜词为空，无法执行搜索任务"));
    }

    let mut trigger = ExpectedNTrigger::new(GAP_NUM);
    for (i, word) in search_words.into_iter().enumerate() {
        let page = browser_bot.get_page()?;
        let sleep_time = if trigger.next() {
            match get_pc_search_process(page).await {
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
                    shot_when_failed(page, "rewards_get_failed", email).await;
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
            tokio::time::sleep(Duration::from_secs(sleep_time)).await;
        } else {
            let mut slept = 0;
            while slept < sleep_time {
                let sleep_chunk = std::cmp::min(MAX_SLEEP_TIME, sleep_time - slept);
                tokio::time::sleep(Duration::from_secs(sleep_chunk)).await;
                // 空转防止 timeout
                let _ = page.reload().await;
                slept += sleep_chunk;
            }
        }

        let mut search_last_err: Option<anyhow::Error> = None;
        for _j in 0..3 {
            let page = browser_bot.get_page()?;
            let browser = browser_bot.get_browser()?;
            match perform_search_and_click(browser, page, &word).await {
                Ok(()) => {
                    search_last_err = None;
                    break;
                }
                Err(e) => {
                    warn!("搜索点击失败，尝试重启浏览器: {}", e);
                    search_last_err = Some(e);
                    browser_bot.restart_pc_browser().await?;
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
        if let Some(e) = search_last_err {
            return Err(e);
        }
        info!("第 {} 次搜索完成", i + 1);
    }

    Ok(())
}

async fn perform_search_and_click(browser: &Browser, page: &Page, word: &str) -> Result<()> {
    let before_pages = browser.pages().await?;

    let search_url = reqwest::Url::parse_with_params(
        "https://cn.bing.com/search",
        [("q", word), ("PC", "U316"), ("FORM", "CHROMN")],
    )?;
    page.goto(search_url.as_str()).await?;
    page.wait_for_navigation().await?;

    tokio::time::sleep(Duration::from_secs(rand::random_range(1..4))).await;

    wait_for_element(page, "#b_results li.b_algo", LONG_ELEMENT_TIMEOUT)
        .await
        .map_err(|e| anyhow!("等待搜索结果超时：{e}"))?;

    let all_res = wait_for_elements(page, "#b_results li.b_algo", LONG_ELEMENT_TIMEOUT).await?;

    let ele = all_res
        .choose(&mut rand::rng())
        .ok_or(anyhow!("没有找到搜索结果"))?;

    ele.click()
        .await
        .map_err(|e| anyhow!(format!("点击搜索结果失败：{e}")))?;

    tokio::time::sleep(Duration::from_secs(rand::random_range(5..10))).await;

    close_tab(before_pages, browser).await?;

    Ok(())
}

async fn click_rewards(browser: &Browser, page: &Page, email: &str, password: &str) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match click_daily_set(browser, page, email, password).await {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                debug!("点击每日任务卡片第 {} 次失败: {}", i + 1, e);
                last_err = Some(e);
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
    if let Some(e) = last_err {
        warn!("点击每日任务卡片失败: {e}");
        return Err(e);
    }
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;

    let mut last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match click_earn(browser, page, email, password).await {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                debug!("点击奖励卡片第 {} 次失败: {}", i + 1, e);
                last_err = Some(e);
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
    if let Some(e) = last_err {
        warn!("点击奖励卡片失败: {e}");
        return Err(e);
    }

    info!("卡片点击完成");
    Ok(())
}

/// 强制刷新页面（忽略缓存），恢复 headless_chrome 中 `tab.reload(true, None)` 的语义。
async fn reload_hard(page: &Page) -> Result<()> {
    let params = ReloadParams::builder().ignore_cache(true).build();
    page.execute(params).await?;
    page.wait_for_navigation().await?;
    Ok(())
}

/// 轮询等待页面中匹配 CSS 选择器的单个元素出现。
///
/// `chromiumoxide` 的 `find_element` 只执行一次查询；原 `headless_chrome` 的
/// `wait_for_element` 会轮询 DOM 直到超时。本函数恢复原有语义。
async fn wait_for_element(page: &Page, selector: &str, timeout: Duration) -> Result<Element> {
    let start = std::time::Instant::now();
    loop {
        match page.find_element(selector).await {
            Ok(ele) => return Ok(ele),
            _ => {
                if start.elapsed() >= timeout {
                    anyhow::bail!("等待选择器 {} 的元素超时", selector);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// 轮询等待页面中匹配 XPath 的单个元素出现。
async fn wait_for_xpath(page: &Page, xpath: &str, timeout: Duration) -> Result<Element> {
    let start = std::time::Instant::now();
    loop {
        match page.find_xpath(xpath).await {
            Ok(ele) => return Ok(ele),
            _ => {
                if start.elapsed() >= timeout {
                    anyhow::bail!("等待 XPath {} 的元素超时", xpath);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// 轮询等待页面中匹配选择器的元素出现（至少一个）。
///
/// 页面动态渲染时，容器可能在 DOMContentLoaded 后就存在，但子元素会延迟填充。
/// chromiumoxide 的 `find_elements` 只执行一次查询，不会主动等待，因此需要手动轮询。
async fn wait_for_elements(page: &Page, selector: &str, timeout: Duration) -> Result<Vec<Element>> {
    let start = std::time::Instant::now();
    loop {
        match page.find_elements(selector).await {
            Ok(elements) if !elements.is_empty() => return Ok(elements),
            _ => {
                if start.elapsed() >= timeout {
                    anyhow::bail!("等待选择器 {} 的元素超时", selector);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn click_earn(browser: &Browser, page: &Page, email: &str, password: &str) -> Result<()> {
    open_rewards_page(page, REWARDS_URL, email, password).await?;

    // 新版 Earn 页面将奖励卡片直接放在 #moreactivities 下的 grid 中，
    // 不再使用旧的 #moreactivities > div > div:nth-of-type(2) 嵌套结构。
    let cards = wait_for_elements(page, "#moreactivities a", LONG_ELEMENT_TIMEOUT).await?;
    info!("找到 {} 个奖励卡片，准备点击", cards.len());

    for card in cards {
        let text = card.inner_text().await?.unwrap_or_default();
        if !text.contains("+") {
            continue;
        }

        let before_pages = browser.pages().await?;
        match card.call_js_fn("function() { this.click(); }", false).await {
            Ok(_) => info!("通过 JS 点击奖励卡片成功"),
            Err(e) => warn!("通过 JS 点击奖励卡片失败：{}", e),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = close_tab(before_pages, browser).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

async fn click_daily_set(
    browser: &Browser,
    page: &Page,
    email: &str,
    password: &str,
) -> Result<()> {
    open_rewards_page(page, REWARDS_URL_DS, email, password).await?;
    expand_daily_set(page).await?;

    let cards = wait_for_elements(page, "#dailyset [role='group'] a", LONG_ELEMENT_TIMEOUT).await?;
    info!("找到 {} 个每日任务卡片，准备点击", cards.len());

    let mut clicked = 0;
    for card in cards {
        let text = card.inner_text().await?.unwrap_or_default();
        if !text.contains("+") {
            continue;
        }

        let before_pages = browser.pages().await?;
        if let Err(e) = card.click().await {
            warn!("点击每日任务卡片失败，尝试通过 JS 点击：{}", e);
            card.call_js_fn("function() { this.click(); }", false)
                .await
                .map_err(|js_err| anyhow!("点击每日任务卡片失败：{e}；JS 回退失败：{js_err}"))?;
        }
        clicked += 1;
        info!("每日任务卡片点击成功：{}", text.replace('\n', " "));
        tokio::time::sleep(Duration::from_secs(5)).await;
        close_tab(before_pages, browser).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // 卡片状态不会在当前 DOM 中实时更新，刷新后确认后端已经记账。
    page.reload().await?;
    page.wait_for_navigation().await?;
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;
    expand_daily_set(page).await?;
    let cards = wait_for_elements(page, "#dailyset [role='group'] a", LONG_ELEMENT_TIMEOUT).await?;
    let mut pending = 0;
    for card in cards {
        let text = card.inner_text().await?.unwrap_or_default();
        if text.contains("+") {
            pending += 1;
        }
    }
    if pending > 0 {
        anyhow::bail!("仍有 {pending} 个每日任务卡片未完成");
    }

    info!("每日任务卡片处理完成，本次点击 {} 个", clicked);
    Ok(())
}

async fn open_rewards_page(
    page: &Page,
    rewards_url: &str,
    email: &str,
    password: &str,
) -> Result<()> {
    page.goto(rewards_url).await?;
    page.wait_for_navigation().await?;
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;

    if !is_login_page(page).await? {
        return Ok(());
    }

    let current_url = page.url().await?.unwrap_or_default();
    if !is_microsoft_login_url(&current_url) {
        anyhow::bail!("账号 {email} 进入了不支持自动填写的账户验证页面");
    }

    warn!("进入 Rewards 页面时跳转到了登录界面，准备在当前页面重新登录账号 {email}");
    submit_login_form(email, password, page).await?;
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;

    page.goto(rewards_url).await?;
    page.wait_for_navigation().await?;
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;

    if is_login_page(page).await? {
        anyhow::bail!("账号 {email} 重新登录后仍然进入登录界面");
    }

    info!("账号 {email} 重新登录成功，继续处理 Rewards 卡片");
    Ok(())
}

async fn is_login_page(page: &Page) -> Result<bool> {
    let current_url = page.url().await?.unwrap_or_default();
    if is_microsoft_login_url(&current_url) || is_microsoft_account_verification_url(&current_url) {
        return Ok(true);
    }

    Ok(page
        .find_element(
            "input[type='email'], input[name='loginfmt'], input#usernameEntry, \
             input[type='password'], input[name='passwd'], input#passwordInput",
        )
        .await
        .is_ok())
}

fn is_microsoft_login_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };

    host == "login.live.com"
        || host.ends_with(".login.live.com")
        || host == "login.microsoft.com"
        || (host.starts_with("login.")
            && (host.ends_with(".microsoftonline.com")
                || host.ends_with(".microsoftonline.cn")
                || host.ends_with(".microsoftonline.us")
                || host.ends_with(".windows.net")))
}

fn is_microsoft_account_verification_url(url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };

    url.host_str() == Some("account.live.com") && url.path().starts_with("/identity/")
}

#[cfg(test)]
mod tests {
    use super::{is_microsoft_account_verification_url, is_microsoft_login_url};

    #[test]
    fn recognizes_microsoft_login_urls() {
        assert!(is_microsoft_login_url(
            "https://login.live.com/login.srf?wa=wsignin1.0"
        ));
        assert!(is_microsoft_login_url(
            "https://login.microsoftonline.com/common/oauth2/authorize"
        ));
        assert!(is_microsoft_login_url(
            "https://login.partner.microsoftonline.cn/common/oauth2/authorize"
        ));
    }

    #[test]
    fn rejects_non_login_urls() {
        assert!(!is_microsoft_login_url(
            "https://rewards.bing.com/dashboard"
        ));
        assert!(!is_microsoft_login_url(
            "https://account.live.com/identity/confirm"
        ));
        assert!(!is_microsoft_login_url(
            "https://login.live.com.evil.example/login"
        ));
        assert!(!is_microsoft_login_url("not a url"));
    }

    #[test]
    fn recognizes_account_verification_urls_without_treating_them_as_login_forms() {
        let url = "https://account.live.com/identity/confirm?mkt=ZH-CN";
        assert!(is_microsoft_account_verification_url(url));
        assert!(!is_microsoft_login_url(url));
        assert!(!is_microsoft_account_verification_url(
            "https://account.live.com/"
        ));
    }
}

async fn expand_daily_set(page: &Page) -> Result<()> {
    // #dailyset 中还有一个带 aria-expanded 的“关于”按钮；真正的折叠开关
    // 由 React Aria 标记为 slot=trigger。
    let toggle = wait_for_element(
        page,
        "#dailyset button[slot='trigger'][aria-expanded]",
        ELEMENT_TIMEOUT,
    )
    .await?;
    let expanded = toggle.attribute("aria-expanded").await?.unwrap_or_default();
    if expanded != "true" {
        toggle.click().await?;
        tokio::time::sleep(ACTION_SETTLE_DELAY).await;
    }
    Ok(())
}

pub(super) async fn login_bing(email: &str, password: &str, page: &Page) -> Result<()> {
    page.activate().await?;
    page.goto(BING_URL).await?;
    page.wait_for_navigation().await?;
    reload_hard(page).await?;
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;

    let mut click_last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match async {
            page.reload().await?;
            page.wait_for_navigation().await?;
            tokio::time::sleep(PAGE_SETTLE_DELAY).await;
            click_login_button(page).await
        }
        .await
        {
            Ok(()) => {
                click_last_err = None;
                break;
            }
            Err(e) => {
                debug!("点击登录按钮第 {} 次失败: {}", i + 1, e);
                click_last_err = Some(e);
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
    if let Some(e) = click_last_err {
        let url = page.url().await?.unwrap_or_default();
        debug!("当前页面：{}", url);
        warn!("点击登录按钮失败: {}", e);
        return Err(e);
    }

    info!("登录按钮点击成功，准备输入账号密码");
    submit_login_form(email, password, page).await
}

async fn submit_login_form(email: &str, password: &str, page: &Page) -> Result<()> {
    page.activate().await?;
    tokio::time::sleep(ACTION_SETTLE_DELAY).await;
    let email_input = wait_for_xpath(
        page,
        concat!(
            "//input[@type='email' or @name='loginfmt']",
            "|//input[@id='usernameEntry']",
        ),
        ELEMENT_TIMEOUT,
    )
    .await
    .map_err(|e| anyhow!("寻找账号输入位置超时：{e}"))?;

    tokio::time::sleep(ACTION_SETTLE_DELAY).await;

    email_input.type_str(email).await?;
    tokio::time::sleep(ACTION_SETTLE_DELAY).await;

    info!("账号输入成功，准备点击下一步");

    let next_button = wait_for_xpath(
        page,
        concat!("//button[@type='submit']", "|//button[text()='下一步']",),
        ELEMENT_TIMEOUT,
    )
    .await?;

    next_button.click().await?;
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;

    let password_input = loop {
        match wait_for_xpath(
            page,
            concat!(
                "//input[@type='password' or @name='passwd']",
                "|//input[@id='passwordInput']",
            ),
            PASSWORD_ELEMENT_TIMEOUT,
        )
        .await
        {
            Ok(input) => break input,
            Err(_) => {
                if let Ok(button) =
                    wait_for_xpath(page, "//*[text()='暂时跳过']", Duration::from_secs(5)).await
                {
                    let _ = button.click().await;
                    tokio::time::sleep(ACTION_SETTLE_DELAY).await;
                }

                if let Ok(button) = wait_for_xpath(
                    page,
                    concat!(
                        "//span[@role='button' and (text()='其他登录方法' or text()='Other ways to sign in')]",
                        "|//*[text()='其他登录方法']"
                    ),
                    Duration::from_secs(5),
                )
                .await
                {
                    let _ = button.click().await;
                    tokio::time::sleep(ACTION_SETTLE_DELAY).await;
                }

                let button = wait_for_xpath(
                    page,
                    concat!(
                        "//*[text()='使用密码']",
                        "|//*[text()='Use your password']",
                        "|//button[contains(text(), '使用密码')]",
                        "|//button[contains(text(), 'Use your password')]",
                        "|//a[contains(text(), '使用密码')]",
                        "|//a[contains(text(), 'Use your password')]",
                    ),
                    PASSWORD_ELEMENT_TIMEOUT,
                )
                .await
                .map_err(|e| anyhow!("等待使用密码按钮超时：{e}"))?;

                button.click().await?;
                tokio::time::sleep(PAGE_SETTLE_DELAY).await;
            }
        }
    };

    tokio::time::sleep(ACTION_SETTLE_DELAY).await;

    password_input.type_str(password).await?;
    tokio::time::sleep(ACTION_SETTLE_DELAY).await;

    info!("密码输入成功，准备点击登录");

    let sign_in_button = wait_for_xpath(
        page,
        concat!(
            "//button[@type='submit']",
            "|//button[text()='登录']",
            "|//button[text()='Sign in']",
            "|//button[text()='下一步']",
            "|//button[text()='Next']",
        ),
        ELEMENT_TIMEOUT,
    )
    .await?;

    sign_in_button.click().await?;
    tokio::time::sleep(PAGE_SETTLE_DELAY).await;

    info!("登录按钮点击成功");

    if wait_for_xpath(
        page,
        "//*[contains(text(), '保持登录状态')]|//*[contains(text(), 'Stay signed in')]",
        Duration::from_secs(5),
    )
    .await
    .is_ok()
    {
        if let Ok(ok_button) = wait_for_xpath(
            page,
            concat!("//button[text()='是']", "|//button[text()='Yes']"),
            Duration::from_secs(3),
        )
        .await
        {
            let _ = ok_button.click().await;
            tokio::time::sleep(ACTION_SETTLE_DELAY).await;
        }
    } else if let Ok(ok_button) = wait_for_xpath(
        page,
        concat!("//button[text()='是']", "|//button[text()='Yes']"),
        Duration::from_secs(3),
    )
    .await
    {
        let _ = ok_button.click().await;
        tokio::time::sleep(ACTION_SETTLE_DELAY).await;
    }

    info!("登录流程完成");
    Ok(())
}

pub(super) async fn check_login_status(page: &Page) -> Result<bool> {
    page.goto(BING_URL).await?;
    page.wait_for_navigation().await?;
    reload_hard(page).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    match wait_for_element(page, "#id_s", LONG_ELEMENT_TIMEOUT).await {
        Ok(ele) => {
            let status = ele.attribute("aria-hidden").await?;
            match status.as_deref() {
                Some("true") => Ok(true),
                Some("false") => Ok(false),
                None => Err(anyhow!("没有找到")),
                _ => Err(anyhow!("未知状态")),
            }
        }
        Err(_) => {
            anyhow::bail!("没有找到登录状态元素")
        }
    }
}

async fn click_login_button(page: &Page) -> Result<()> {
    // 新版 Bing 首页可能不显示登录入口，优先在搜索结果页头部寻找
    let login_button = wait_for_xpath(
        page,
        concat!(
            "//span[@id='id_s']",
            "|//*[@id='id_a']",
            "|//a[@id='id_l']",
            "|//a[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//a[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//a[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
            "|//button[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//button[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//button[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
        ),
        ELEMENT_TIMEOUT,
    )
    .await;

    let login_button = match login_button {
        Ok(btn) => btn,
        Err(_) => {
            info!("首页未找到登录按钮，尝试从搜索结果页登录");
            page.goto("https://cn.bing.com/search?q=bing").await?;
            page.wait_for_navigation().await?;
            tokio::time::sleep(PAGE_SETTLE_DELAY).await;
            wait_for_xpath(
                page,
                "//header//a[contains(text(), '登录') or contains(text(), 'Sign in')]",
                ELEMENT_TIMEOUT,
            )
            .await
            .map_err(|e| anyhow!("等待登录按钮超时：{e}"))?
        }
    };

    info!("找到登录按钮，准备点击");
    tokio::time::sleep(ACTION_SETTLE_DELAY).await;
    login_button.click().await?;
    page.wait_for_navigation().await?;
    Ok(())
}

async fn wait_for_dialog_search_progress(page: &Page, timeout: Duration) -> Result<Option<String>> {
    let start = std::time::Instant::now();
    loop {
        let result: serde_json::Value = page
            .evaluate(
                r#"(() => {
                    const labelPattern = /^(必应搜索|Bing search|PC search)$/i;
                    const label = Array.from(document.querySelectorAll('p, span, div'))
                        .find(e => e.children.length === 0 &&
                                   labelPattern.test((e.textContent || '').trim()));
                    if (!label) return { value: null };
                    const text = label.parentElement?.nextElementSibling?.textContent || '';
                    const m = text.match(/(\d+)\s*\/\s*(\d+)/);
                    return { value: m ? `${m[1]}/${m[2]}` : null };
                })()"#,
            )
            .await?
            .into_value()?;
        if let Some(value) = result.get("value").and_then(|v| v.as_str()) {
            return Ok(Some(value.to_string()));
        }
        if start.elapsed() >= timeout {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn get_pc_search_process(page: &Page) -> Result<(u32, u32)> {
    page.goto("https://rewards.bing.com/earn").await?;
    page.wait_for_navigation().await?;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Cookie 提示会覆盖页面控件；拒绝可选 Cookie 后再打开积分侧栏。
    if let Ok(button) = page
        .find_xpath(
            "//button[normalize-space()='拒绝' or normalize-space()='全部拒绝' or normalize-space()='Reject' or normalize-space()='Reject all']",
        )
        .await
    {
        let _ = button.click().await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // 点击顶部"积分明细"按钮，弹出真正的积分进度对话框。
    // 卡片上显示的"搜索: 1/1"只是连续打卡进度，积分明细里的"必应搜索 x/y"才是当日搜索积分。
    let detail_button = wait_for_xpath(
        page,
        concat!(
            "//button[.//*[contains(text(), '积分明细') or contains(text(), 'Points breakdown')]]",
            "|//button[contains(., '积分明细') or contains(., 'Points breakdown')]",
        ),
        ELEMENT_TIMEOUT,
    )
    .await
    .map_err(|e| anyhow!("等待积分明细按钮超时：{e}"))?;
    detail_button.click().await?;
    tokio::time::sleep(ACTION_SETTLE_DELAY).await;

    // 新版积分明细是无 dialog role 的侧栏，内容仍然异步加载。
    let progress = wait_for_dialog_search_progress(page, ELEMENT_TIMEOUT).await?;

    // 关闭对话框，避免影响后续操作
    let _ = page
        .evaluate(
            r#"(() => {
                const heading = Array.from(document.querySelectorAll('h1, h2, h3, [role="heading"]'))
                    .find(e => /^(积分明细|Points breakdown)$/i.test((e.textContent || '').trim()));
                if (!heading) return;
                const close = heading.parentElement?.querySelector(
                    'button[aria-label="关闭"], button[aria-label="Close"]'
                );
                if (close) close.click();
            })()"#,
        )
        .await;

    let text = progress.ok_or(anyhow!("未找到 PC 搜索积分进度"))?;
    parse_point(&text)
}

fn parse_point(text: &str) -> Result<(u32, u32)> {
    let re = regex::Regex::new(r"(?P<cur>\d+)\D*/\D*(?P<tot>\d+)").unwrap();
    if let Some(caps) = re.captures(text)
        && let Some(cur) = caps.name("cur")
        && let Some(tot) = caps.name("tot")
    {
        let cur: u32 = cur.as_str().parse()?;
        let tot: u32 = tot.as_str().parse()?;
        return Ok((cur, tot));
    }
    Err(anyhow!("无法解析积分"))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_point() {
        let text = "15 个积分/共 60 个(金牌)";
        let (cur, max) = parse_point(text).unwrap();
        assert_eq!(cur, 15);
        assert_eq!(max, 60);
        let text = "15/60";
        let (cur, max) = parse_point(text).unwrap();
        assert_eq!(cur, 15);
        assert_eq!(max, 60);
    }

    #[test]
    fn parse_chrome_singleton_lock_reads_hostname_and_pid() {
        assert_eq!(
            parse_chrome_singleton_lock("MTGQ93YQPH-85812"),
            Some(("MTGQ93YQPH".to_string(), 85812))
        );
        assert_eq!(parse_chrome_singleton_lock("no-pid"), None);
        assert_eq!(parse_chrome_singleton_lock("-123"), None);
        assert_eq!(parse_chrome_singleton_lock("onlyhost"), None);
    }

    #[cfg(unix)]
    #[test]
    fn stale_chrome_singleton_files_are_removed() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock = dir.path().join("SingletonLock");
        std::os::unix::fs::symlink("HOST-99999", &lock).unwrap();
        std::os::unix::fs::symlink("cookie", dir.path().join("SingletonCookie")).unwrap();
        std::os::unix::fs::symlink("socket", dir.path().join("SingletonSocket")).unwrap();

        assert_eq!(
            read_chrome_singleton_lock(dir.path()),
            Some(("HOST".to_string(), 99999))
        );
        assert!(!is_pid_alive(99999));
        remove_chrome_singleton_files(dir.path());
        assert!(!dir.path().join("SingletonLock").exists());
        assert!(!dir.path().join("SingletonCookie").exists());
        assert!(!dir.path().join("SingletonSocket").exists());
    }
}
