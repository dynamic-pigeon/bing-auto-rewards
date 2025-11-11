use std::{
    mem::ManuallyDrop,
    path::Path,
    str::FromStr,
    sync::Arc,
    thread::{sleep, spawn},
    time::{self, Duration},
};

use anyhow::Result;
use chrono::Local;
use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};
use log::{error, info, warn};

use crate::bing::retry::Retryable;

mod mobile;
mod pc;
mod retry;

const HEADLESS: bool = true;
const BING_URL: &str = "https://www.bing.com/";
const REWARDS_URL: &str = "https://rewards.bing.com/";
const SLEEP_RANGE: std::ops::Range<u64> = 30..70;
const GAP_RANGE: std::ops::Range<u64> = 600..1000;

/// 需要保证 temp_dir 的生命周期长于 browser
pub(crate) struct BingBot {
    pub(crate) browser: ManuallyDrop<Browser>,
    temp_dir: Option<tempfile::TempDir>,
}

impl Drop for BingBot {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.browser);
            if let Some(dir) = self.temp_dir.take() {
                sleep(Duration::from_secs(3));
                drop(dir);
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct Config {
    accounts: Vec<Account>,
    max_threads: Option<usize>,
    store_local: Option<bool>,
    browser_path: Option<String>,
    schedule: Option<String>,
}

#[derive(serde::Deserialize, Clone)]
struct Account {
    email: String,
    password: String,
    proxy: Option<String>,
}

pub(crate) fn process<P: AsRef<Path>>(config_file: P) -> Result<()> {
    let config_file = std::fs::File::open(config_file)?;

    let config: Config = serde_json::from_reader(config_file)?;

    if let Some(schedule) = config.schedule.as_deref() {
        let schedule = croner::Cron::from_str(schedule)
            .inspect_err(|e| error!("定时任务格式串解析有误：{}", e))?;

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
            if let Err(e) = process_once(&config) {
                error!("定时任务执行失败：{}", e);
            }
        }
    } else {
        process_once(&config)?;
    }

    Ok(())
}

fn process_once(config: &Config) -> Result<()> {
    let (tx, rx) = crossbeam::channel::unbounded();

    let max_threads = config.max_threads.unwrap_or(1);
    let store_local = config.store_local.unwrap_or(false);

    let handlers = (1..=max_threads)
        .map(|i| {
            let rx: crossbeam::channel::Receiver<Account> = rx.clone();
            let browser_path = config.browser_path.clone();
            spawn(move || {
                info!("==== 第 {} 个线程启动 ====", i);
                for account in rx {
                    info!("==== 第 {} 个线程处理账号 {} ====", i, account.email);
                    process_account(account, &browser_path, store_local);
                }

                info!("==== 第 {} 个线程结束 ====", i);
            })
        })
        .collect::<Vec<_>>();

    drop(rx);

    for account in &config.accounts {
        tx.send(account.clone())?;
    }

    drop(tx);

    for handler in handlers {
        let _ = handler.join();
    }
    Ok(())
}

fn process_account(account: Account, browser_path: &Option<String>, store_local: bool) {
    let _ = (|| {
        let mut bot =
            BingBot::new_pc_browser(store_local, &account.email, &browser_path, &account.proxy);

        pc::process_account(&account.email, &account.password, &mut bot)?;
        sleep(time::Duration::from_secs(3));
        Ok(())
    })
    .retry(2)
    .inspect_err(|e| error!("处理账号 {} 失败: {}", account.email, e));

    let _ = (|| {
        let mut bot =
            BingBot::new_mobile_browser(store_local, &account.email, &browser_path, &account.proxy);

        mobile::process_account(&account.email, &account.password, &mut bot)?;
        sleep(time::Duration::from_secs(3));
        Ok(())
    })
    .retry(2)
    .inspect_err(|e| error!("处理移动端账号 {} 失败: {}", account.email, e));
}

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
        let file_name = format!("{}_failure_{}.png", prefix, account);
        if let Err(e) = std::fs::write(Path::new("failed").join(&file_name), &png) {
            warn!("保存失败截图 failed/{} 失败: {}", file_name, e);
        } else {
            info!("失败截图已保存为 {}", file_name);
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

fn default_options_builder() -> LaunchOptionsBuilder<'static> {
    let mut options = LaunchOptionsBuilder::default();
    options
        .headless(HEADLESS)
        .enable_gpu(false)
        .idle_browser_timeout(Duration::from_mins(15))
        .sandbox(false)
        .args(
            [
                "--disable-dev-shm-usage",
                "--disable-extensions",
                "--disable-blink-features=AutomationControlled",
                "--allow-running-insecure-content",
                "--disable-plugins",
                "--incognito",
                "--disable-images",
                "--disable-web-security",
            ]
            .into_iter()
            .map(std::ffi::OsStr::new)
            .collect(),
        );
    options
}
