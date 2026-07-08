use std::{
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

#[derive(serde::Deserialize, Default)]
struct Config {
    accounts: Vec<Account>,
    #[serde(default = "default_max_threads")]
    max_threads: usize,
    #[serde(default)]
    store_local: bool,
    browser_path: Option<String>,
    schedule: Option<String>,
    #[serde(default)]
    user_data_cleanup_days: Option<u64>,
}

fn default_max_threads() -> usize {
    1
}

#[derive(serde::Deserialize, Clone)]
struct Account {
    email: String,
    password: String,
    proxy: Option<String>,
}

pub(crate) fn process<P: AsRef<Path>>(config_file: P) -> Result<()> {
    let config_file = std::fs::File::open(config_file)?;

    let config: Arc<Config> = Arc::new(serde_json::from_reader(config_file)?);

    let pool = Arc::new(BrowserPool::new(config.max_threads));

    let run_with_cleanup = |config: Arc<Config>, pool: Arc<BrowserPool>| {
        if let Some(days) = config.user_data_cleanup_days {
            cleanup_stale_user_data(days)
                .inspect_err(|e| warn!("清理 user-data 失败：{}", e))
                .ok();
        }
        process_once(config, pool)
    };

    if let Some(schedule) = config.schedule.as_deref() {
        let schedule = croner::Cron::from_str(schedule)
            .inspect_err(|e| error!("定时任务格式串解析有误：{}", e))?;

        info!("第一次执行无视定时任务");
        if let Err(e) = run_with_cleanup(Arc::clone(&config), Arc::clone(&pool)) {
            error!("定时任务执行失败：{}", e);
        }

        for time in schedule.iter_after(Local::now()) {
            let now = Local::now();
            let duration = time.signed_duration_since(now);
            let duration = duration.to_std().unwrap_or_else(|_| Duration::from_secs(0));
            info!(
                "下次任务将在 {} 执行，等待 {} 秒",
                time.format("%Y-%m-%d %H:%M:%S"),
                duration.as_secs()
            );
            sleep(duration);
            if let Err(e) = run_with_cleanup(Arc::clone(&config), Arc::clone(&pool)) {
                error!("定时任务执行失败：{}", e);
            }
        }
    } else {
        run_with_cleanup(config, pool)?;
    }

    Ok(())
}

fn process_once(config: Arc<Config>, pool: Arc<BrowserPool>) -> Result<()> {
    // 启动时先获取一次热搜，顺便检测一下网络是否畅通
    hot_searches::fetch_hot_words_blocking().inspect_err(|e| warn!("获取热搜失败: {}", e))?;

    let mut handles = vec![];
    for account in &config.accounts {
        let account = account.clone();
        let config = Arc::clone(&config);
        let pool = Arc::clone(&pool);
        let email = account.email.clone();
        let handle = Builder::new()
            .name(email.clone())
            .spawn(move || {
                process_account(account, config.as_ref(), Arc::clone(&pool));
            })
            .map_err(|e| anyhow!("创建账号 {} 的处理线程失败: {}", email, e))?;
        handles.push(handle);
    }

    let mut errors = 0;
    for handle in handles {
        if let Err(e) = handle.join() {
            errors += 1;
            let err = e
                .downcast_ref::<String>()
                .map(String::as_str)
                .unwrap_or("未知错误");
            error!("处理账号的线程发生错误：{}", err);
        }
    }

    if errors > 0 {
        anyhow::bail!("{} 个账号处理线程出现异常", errors);
    }

    Ok(())
}

const USER_DATA_DIR: &str = "./user-data";
const LAST_CLEANUP_MARKER: &str = ".last_cleanup";

fn cleanup_stale_user_data(retention_days: u64) -> Result<()> {
    if retention_days == 0 {
        warn!("user_data_cleanup_days=0，跳过清理");
        return Ok(());
    }

    let root = Path::new(USER_DATA_DIR);
    if !root.exists() {
        return Ok(());
    }

    let ttl = Duration::from_secs(retention_days.saturating_mul(24 * 60 * 60));
    let now = SystemTime::now();

    let last_cleanup = read_last_cleanup_time(root)?;
    let elapsed = now
        .duration_since(last_cleanup)
        .unwrap_or_else(|_| Duration::from_secs(0));

    if elapsed < ttl {
        info!(
            "距离上次清理不足 {} 天，跳过本次清理（已过 {} 秒）",
            retention_days,
            elapsed.as_secs()
        );
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        fs::remove_dir_all(&path)?;
        info!("已清理目录 {}（周期清理）", path.display());
    }

    write_last_cleanup_time(root, now)?;
    info!("本次周期清理完成，下次将在至少 {} 天后触发", retention_days);

    Ok(())
}

fn read_last_cleanup_time(root: &Path) -> Result<SystemTime> {
    let marker = root.join(LAST_CLEANUP_MARKER);
    if marker.is_file() {
        let content = fs::read_to_string(&marker)?;
        match content.trim().parse::<u64>() {
            Ok(ts) => return Ok(UNIX_EPOCH + Duration::from_secs(ts)),
            Err(e) => warn!("清理时间戳解析失败（{}），将视为首次清理", e),
        }
    }

    Ok(UNIX_EPOCH)
}

fn write_last_cleanup_time(root: &Path, now: SystemTime) -> Result<()> {
    let ts = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs();
    fs::write(root.join(LAST_CLEANUP_MARKER), ts.to_string())?;
    Ok(())
}

fn process_account(
    account: Account,
    &Config {
        store_local,
        ref browser_path,
        ..
    }: &Config,
    pool: Arc<BrowserPool>,
) {
    let _ = (|| {
        let mut bot = pool.get_bot();
        bot.new_pc_browser(store_local, &account.email, browser_path, &account.proxy)?;
        info!("开始处理 PC 端账号 {}", account.email);
        pc::process_account(&account.email, &account.password, &mut bot)?;
        Ok(())
    })
    .retry(2)
    .inspect_err(|e| error!("处理账号 {} 失败: {}", account.email, e));
}

fn default_browser_config(
    args: Vec<String>,
    browser_path: &Option<String>,
    user_dir: Option<std::path::PathBuf>,
    proxy: &Option<String>,
) -> Result<BrowserConfig> {
    let mut config = BrowserConfig::builder();
    if !HEADLESS {
        config = config.with_head();
    }
    config = config.no_sandbox();
    config = config.window_size(1920, 1080);
    if let Some(path) = browser_path {
        config = config.chrome_executable(std::path::PathBuf::from(path));
    }
    if let Some(dir) = user_dir {
        config = config.user_data_dir(dir);
    }

    let mut chrome_args = vec![
        "--disable-dev-shm-usage".to_string(),
        "--disable-extensions".to_string(),
        "--disable-blink-features=AutomationControlled".to_string(),
        "--allow-running-insecure-content".to_string(),
        "--disable-plugins".to_string(),
        "--disable-images".to_string(),
        "--disable-web-security".to_string(),
        "--mute-audio".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ];
    chrome_args.extend(args);

    if let Some(proxy) = proxy {
        chrome_args.push("--proxy-server".to_string());
        chrome_args.push(proxy.clone());
    }

    config = config.args(chrome_args);

    config
        .build()
        .map_err(|e| anyhow!("构建浏览器启动选项失败：{}", e))
}

fn get_one_page(browser: &Browser, runtime: &tokio::runtime::Runtime) -> Result<Page> {
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
    runtime: &tokio::runtime::Runtime,
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

fn shot_when_failed(
    page: &Page,
    runtime: &tokio::runtime::Runtime,
    prefix: &str,
    account: &str,
) {
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
