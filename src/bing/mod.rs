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
use headless_chrome::{Browser, Tab};
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
    sechedule: Option<String>,
}

#[derive(serde::Deserialize, Clone)]
struct Account {
    email: String,
    password: String,
}

pub(crate) fn process<P: AsRef<Path>>(config_file: P) -> Result<()> {
    // 读取配置文件并反序列化，后续流程依赖该结构体
    let config_file = std::fs::File::open(config_file)?;
    let config: Config = serde_json::from_reader(config_file)?;

    if let Some(schedule) = config.sechedule.as_deref() {
        // 解析 Cron 表达式，若失败打印详细报错
        let schedule = croner::Cron::from_str(schedule)
            .inspect_err(|e| error!("定时任务格式串解析有误：{}", e))?;

        for time in schedule.iter_after(Local::now()) {
            let now = Local::now();
            // 计算下一次执行的等待时间
            let duration = time
                .signed_duration_since(now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(0));

            let formatted_time = time.format("%Y-%m-%d %H:%M:%S");
            info!(
                "下次任务将在 {} 执行，等待 {} 秒",
                formatted_time,
                duration.as_secs()
            );

            sleep(duration);

            if let Err(e) = process_once(&config) {
                error!("定时任务执行失败：{}", e);
            }
        }
    } else {
        // 未配置定时任务则直接执行一次
        process_once(&config)?;
    }

    Ok(())
}

fn process_once(config: &Config) -> Result<()> {
    // 生产者消费者模型：主线程生产账号，子线程消费账号
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

                    // PC 端处理任务，失败会触发截图
                    let handle_pc_account = || -> Result<()> {
                        let mut bot =
                            BingBot::new_pc_browser(store_local, &account.email, &browser_path);

                        pc::process_account(&account.email, &account.password, &mut bot.browser)?;
                        sleep(time::Duration::from_secs(3));
                        Ok(())
                    };

                    if let Err(e) = handle_pc_account.retry(2) {
                        error!("处理账号 {} 失败: {}", account.email, e);
                    }

                    // 移动端处理任务，失败同样会触发截图
                    let handle_mobile_account = || -> Result<()> {
                        let mut bot =
                            BingBot::new_mobile_browser(store_local, &account.email, &browser_path);

                        mobile::process_account(
                            &account.email,
                            &account.password,
                            &mut bot.browser,
                        )?;
                        sleep(time::Duration::from_secs(3));
                        Ok(())
                    };

                    if let Err(e) = handle_mobile_account.retry(2) {
                        error!("处理移动端账号 {} 失败: {}", account.email, e);
                    }
                }

                info!("==== 第 {} 个线程结束 ====", i);
            })
        })
        .collect::<Vec<_>>();

    drop(rx);

    // 将账号派发给工作线程
    for account in &config.accounts {
        tx.send(account.clone())?;
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

    let reward_counter_xpath =
        "//*[@id='dailypointToolTipDiv']/p/mee-rewards-counter-animation/span";
    let ele =
        tab.wait_for_xpath_with_custom_timeout(reward_counter_xpath, Duration::from_secs(5))?;

    ele.get_inner_text()
}

fn shot_when_faild(tab: &Tab, prefix: &str, account: &str) {
    // 捕获当前页面截图，用于排查失败原因
    let capture_result = tab.capture_screenshot(
        headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
        None,
        None,
        true,
    );

    let Ok(png) = capture_result else {
        return;
    };

    std::fs::create_dir_all("failed").ok();
    let file_name = format!("{}_failure_{}.png", prefix, account);
    let file_path = Path::new("failed").join(&file_name);

    if let Err(e) = std::fs::write(&file_path, &png) {
        warn!("保存失败截图 failed/{} 失败: {}", file_name, e);
        return;
    }

    info!("失败截图已保存为 {}", file_name);
}

fn close_tab(before_tabs: Vec<Arc<Tab>>, browser: &mut Browser) -> Result<()> {
    // 对比执行前后的标签页集合，只关闭新增的标签页，避免影响主标签页
    let after_tabs = browser.get_tabs().lock().unwrap().clone();

    for tab in after_tabs.iter() {
        let exists_in_before = before_tabs
            .iter()
            .any(|t| t.get_target_id() == tab.get_target_id());

        if exists_in_before {
            continue;
        }

        info!("发现新打开的标签页，准备关闭");
        tab.close(false)?;
        info!("标签页关闭成功");
    }

    Ok(())
}
