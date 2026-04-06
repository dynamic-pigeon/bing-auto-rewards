use std::{
    ffi::OsStr,
    fs,
    path::Path,
    str::FromStr,
    sync::Arc,
    thread::{sleep, spawn},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use chrono::Local;
use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};
use log::{error, info, warn};

use crate::bing::{
    browser_pool::{BingBot, BrowserPool},
    retry::Retryable,
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
    let mut handles = vec![];
    for account in &config.accounts {
        let account = account.clone();
        let config = Arc::clone(&config);
        let pool = Arc::clone(&pool);
        let handle = spawn(move || {
            process_account(account, config.as_ref(), Arc::clone(&pool));
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
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
    if marker.exists() {
        let content = fs::read_to_string(&marker)?;
        if let Ok(ts) = content.trim().parse::<u64>() {
            return Ok(UNIX_EPOCH + Duration::from_secs(ts));
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

#[allow(dead_code)]
fn get_today_rewards(tab: &Tab) -> Result<String> {
    tab.navigate_to(REWARDS_URL)?;
    tab.wait_until_navigated()?;

    let ele = tab.wait_for_xpath_with_custom_timeout(
        "//*[@id='dailypointToolTipDiv']/p/mee-rewards-counter-animation/span",
        Duration::from_secs(5),
    )?;

    ele.get_inner_text()
}

fn shot_when_faild(tab: &Tab, prefix: &str, account: &str) {
    if let Ok(png) = tab.capture_screenshot(
        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
        None,
        None,
        true,
    ) {
        std::fs::create_dir_all("failed").ok();
        let file_name = format!("{prefix}_failure_{account}.png");
        if let Err(e) = std::fs::write(Path::new("failed").join(&file_name), &png) {
            warn!("保存失败截图 failed/{file_name} 失败: {e}");
        } else {
            info!("失败截图已保存为 {file_name}");
        }
    }
}

fn close_tab(before_tabs: Vec<Arc<Tab>>, browser: &mut Browser) -> Result<()> {
    let after_tabs = browser.get_tabs().lock().unwrap().clone();
    // 就两三个 tab，直接遍历关闭
    for tab in after_tabs.iter() {
        if !before_tabs
            .iter()
            .any(|t| t.get_target_id() == tab.get_target_id())
        {
            info!("发现新打开的标签页，准备关闭");
            tab.close(false)?;
            info!("标签页关闭成功");
        }
    }
    Ok(())
}

fn get_one_tab(browser: &mut Browser) -> Result<Arc<Tab>> {
    let tabs = browser.get_tabs().lock().unwrap();
    if !tabs.is_empty() {
        tabs[0].set_default_timeout(Duration::from_secs(25));
        Ok(tabs[0].clone())
    } else {
        drop(tabs);
        let tab = browser.new_tab()?;
        tab.set_default_timeout(Duration::from_secs(25));
        Ok(tab)
    }
}

fn default_options_builder<'a>(args: Vec<&'a OsStr>) -> LaunchOptionsBuilder<'a> {
    let mut options = LaunchOptionsBuilder::default();
    options
        .headless(HEADLESS)
        .enable_gpu(false)
        .idle_browser_timeout(Duration::from_mins(30))
        .sandbox(false)
        .args(
            [
                "--disable-dev-shm-usage",
                "--disable-extensions",
                "--disable-blink-features=AutomationControlled",
                "--allow-running-insecure-content",
                "--disable-plugins",
                "--disable-images",
                "--disable-web-security",
                "--mute-audio",
                "--no-first-run",
                "--no-default-browser-check",
            ]
            .into_iter()
            .map(std::ffi::OsStr::new)
            .chain(args)
            .collect(),
        );
    options
}
