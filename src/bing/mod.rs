use std::{
    ffi::OsStr,
    path::Path,
    str::FromStr,
    sync::Arc,
    thread::{sleep, spawn},
    time::Duration,
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
const BING_URL: &str = "https://www.bing.com/";
const REWARDS_URL: &str = "https://rewards.bing.com/";
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

    if let Some(schedule) = config.schedule.as_deref() {
        let schedule = croner::Cron::from_str(schedule)
            .inspect_err(|e| error!("定时任务格式串解析有误：{}", e))?;

        info!("第一次执行无视定时任务");
        if let Err(e) = process_once(Arc::clone(&config), Arc::clone(&pool)) {
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
            if let Err(e) = process_once(Arc::clone(&config), Arc::clone(&pool)) {
                error!("定时任务执行失败：{}", e);
            }
        }
    } else {
        process_once(config, pool)?;
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
