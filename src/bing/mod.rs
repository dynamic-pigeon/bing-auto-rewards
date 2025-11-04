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
    _temp_dir: ManuallyDrop<tempfile::TempDir>,
}

impl Drop for BingBot {
    fn drop(&mut self) {
        unsafe {
            ManuallyDrop::drop(&mut self.browser);
            sleep(Duration::from_secs(5));
            ManuallyDrop::drop(&mut self._temp_dir);
        }
    }
}

#[derive(serde::Deserialize)]
struct Config {
    groups: Vec<Group>,
}

#[derive(serde::Deserialize)]
struct Group {
    accounts: Vec<Account>,
}

#[derive(serde::Deserialize)]
struct Account {
    email: String,
    password: String,
}

pub(crate) fn process<P: AsRef<Path>>(config_file: P) -> Result<()> {
    let config_file = std::fs::File::open(config_file)?;

    let config: Config = serde_json::from_reader(config_file)?;

    let handlers = config
        .groups
        .into_iter()
        .enumerate()
        .map(|(i, group)| {
            spawn(move || {
                let mut bot = BingBot::new_pc_browser();
                info!("==== 开始处理第 {} 组账号 ====", i + 1);
                for account in &group.accounts {
                    if let Err(e) =
                        pc::process_account(&account.email, &account.password, &mut bot.browser)
                    {
                        error!("处理账号 {} 失败: {}", account.email, e);
                    }
                    sleep(time::Duration::from_secs(3));
                }

                let mut bot = BingBot::new_mobile();
                for account in group.accounts {
                    if let Err(e) =
                        mobile::process_account(&account.email, &account.password, &mut bot.browser)
                    {
                        error!("处理账号 {} 失败: {}", account.email, e);
                    }
                    sleep(time::Duration::from_secs(3));
                }
                info!("==== 第 {} 组账号处理完成 ====", i + 1);
            })
        })
        .collect::<Vec<_>>();

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

    Ok(ele.get_inner_text()?)
}

fn shot_with_faild(tab: &Tab, prefix: &str, account: &str) {
    if let Ok(png) = tab.capture_screenshot(
        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
        None,
        None,
        true,
    ) {
        let file_name = format!("{}_failure_{}.png", prefix, account);
        if let Err(e) = std::fs::write(&file_name, &png) {
            warn!("保存失败截图 {} 失败: {}", file_name, e);
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
