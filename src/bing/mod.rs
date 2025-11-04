use std::{
    mem::ManuallyDrop,
    path::Path,
    sync::Arc,
    thread::{sleep, spawn},
    time::{self, Duration},
};

use anyhow::Result;
use headless_chrome::{Browser, Tab};
use log::{debug, error, info, warn};

mod mobile;
mod pc;

const HEADLESS: bool = true;
const BING_URL: &str = "https://www.bing.com/";
const REWARDS_URL: &str = "https://rewards.bing.com/";
const SLEEP_RANGE: std::ops::Range<u64> = 10..20;
const GAP_RANGE: std::ops::Range<u64> = 200..400;

/// 需要保证 temp_dir 的生命周期长于 browser
pub(crate) struct BingBot {
    pub(crate) browser: ManuallyDrop<Browser>,
    temp_dir: ManuallyDrop<tempfile::TempDir>,
}

impl Drop for BingBot {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.browser);
            // 等待一会儿，确保浏览器进程退出
            sleep(Duration::from_secs(5));
            ManuallyDrop::drop(&mut self.temp_dir);
        }
    }
}

#[derive(serde::Deserialize)]
struct Config {
    accounts: Vec<Account>,
    max_threads: Option<usize>,
}

#[derive(serde::Deserialize)]
struct Account {
    email: String,
    password: String,
}

pub(crate) fn process<P: AsRef<Path>>(config_file: P) -> Result<()> {
    let config_file = std::fs::File::open(config_file)?;

    let config: Config = serde_json::from_reader(config_file)?;

    let (tx, rx) = crossbeam::channel::unbounded();

    let max_threads = config.max_threads.unwrap_or(1);

    let handlers = (1..=max_threads)
        .map(|i| {
            let rx: crossbeam::channel::Receiver<Account> = rx.clone();
            spawn(move || {
                info!("==== 第 {} 个线程启动 ====", i);
                for account in rx {
                    info!("==== 第 {} 个线程处理账号 {} ====", i, account.email);

                    let mut bot = BingBot::new_pc_browser();
                    if let Err(e) =
                        pc::process_account(&account.email, &account.password, &mut bot.browser)
                    {
                        error!("处理账号 {} 失败: {}", account.email, e);
                    }
                    sleep(time::Duration::from_secs(3));

                    let mut bot = BingBot::new_mobile_browser();
                    if let Err(e) =
                        mobile::process_account(&account.email, &account.password, &mut bot.browser)
                    {
                        error!("处理账号 {} 失败: {}", account.email, e);
                    }
                    sleep(time::Duration::from_secs(3));
                }

                info!("==== 第 {} 个线程结束 ====", i);
            })
        })
        .collect::<Vec<_>>();

    drop(rx);

    for account in config.accounts {
        tx.send(account)?;
    }

    drop(tx);

    for handler in handlers {
        let _ = handler.join();
    }

    Ok(())
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

fn retry<T>(mut f: impl FnMut() -> Result<T>, times: usize) -> Result<T> {
    let mut ret = None;
    for i in 0..times {
        match f() {
            Ok(result) => return Ok(result),
            Err(e) => {
                debug!("尝试第 {} 次失败: {}", i + 1, e);
                sleep(Duration::from_secs(2));
                ret = Some(e);
            }
        }
    }
    unsafe { Err(ret.unwrap_unchecked()) }
}

fn close_tab(before_tabs: Vec<Arc<Tab>>, browser: &mut Browser) -> Result<()> {
    let after_tabs = browser.get_tabs().lock().unwrap().clone();
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
