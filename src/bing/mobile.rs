use std::{ffi::OsStr, mem::ManuallyDrop, path::PathBuf, sync::Arc, thread::sleep, time::Duration};

use headless_chrome::{Browser, Tab};
use log::{debug, info, warn};

use crate::{
    bing::{
        Account, BING_URL, BingBot, GAP_NUM, GAP_RANGE, SLEEP_RANGE, close_tab,
        default_options_builder, retry::Retryable, shot_when_faild,
    },
    random::ExpectedNTrigger,
};

use anyhow::{Result, anyhow};

impl BingBot {
    pub(crate) fn new_mobile_browser(
        store_local: bool,
        account: &str,
        browser_path: &Option<String>,
        proxy: &Option<String>,
    ) -> Self {
        let temp_dir = None;

        let user_dir = if store_local {
            std::fs::create_dir_all("./user-data").unwrap();
            Some(std::path::PathBuf::from(format!(
                "./user-data/mobile_{}",
                account
            )))
        } else {
            std::fs::create_dir_all("./tmp").unwrap();
            let dir = tempfile::TempDir::new_in("./tmp").unwrap();
            Some(dir.path().to_path_buf())
        };

        let options = default_options_builder()
            .path(browser_path.clone().map(PathBuf::from))
            .user_data_dir(user_dir)
            .proxy_server(proxy.as_deref())
            .window_size(Some((770, 1600)))
            .args(vec![OsStr::new("--user-agent='Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Mobile Safari/537.36'")])
            .build()
            .unwrap();
        let browser = Browser::new(options).unwrap();
        BingBot {
            browser: ManuallyDrop::new(browser),
            temp_dir,
            store_local,
            account: account.to_string(),
            browser_path: browser_path.clone(),
            proxy: proxy.clone(),
        }
    }

    pub(crate) fn restart_mobile_browser(&mut self) -> Result<()> {
        unsafe {
            ManuallyDrop::drop(&mut self.browser);
        }
        let user_dir = if self.store_local {
            std::fs::create_dir_all("./user-data").unwrap();
            Some(std::path::PathBuf::from(format!(
                "./user-data/pc_{}",
                self.account
            )))
        } else {
            Some(self.temp_dir.as_ref().unwrap().path().to_path_buf())
        };
        let options = default_options_builder()
            .path(self.browser_path.clone().map(PathBuf::from))
            .user_data_dir(user_dir)
            .proxy_server(self.proxy.as_deref())
            .window_size(Some((770, 1600)))
            .args(vec![OsStr::new("--user-agent='Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Mobile Safari/537.36'")])
            .build()
            .unwrap();
        let browser = Browser::new(options).unwrap();

        self.browser = ManuallyDrop::new(browser);
        debug!("浏览器重启完成");
        Ok(())
    }
}

pub(crate) fn process_account(
    Account {
        email,
        password,
        proxy,
    }: &Account,
    browser_path: &Option<String>,
    store_local: bool,
) -> Result<()> {
    let mut browser_bot = BingBot::new_mobile_browser(store_local, email, browser_path, proxy);
    info!("开始处理移动端账号: {}", email);

    let browser = &mut browser_bot.browser;
    let mut tab = get_one_tab(browser)?;
    tab.set_default_timeout(Duration::from_secs(25));
    (|| {
        let logged_in = check_login_status(&tab)?;
        if !logged_in {
            login_bing_mobile(email, password, &tab)?;
        } else {
            info!("账号 {} 已登录，跳过登录步骤", email);
        }
        Ok(())
    })
    .retry(3)
    .inspect_err(|e| {
        shot_when_faild(&tab, "mobile_login_failed", email);
        warn!("账号 {} 登录失败: {}", email, e);
    })?;

    info!("账号 {} 登录成功，开始搜索任务", email);

    sleep(Duration::from_secs(5));

    search(&mut tab, &mut browser_bot, email).inspect_err(|_| {
        shot_when_faild(&tab, "mobile_search", email);
    })?;
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

fn search(tab: &mut Arc<Tab>, browser_bot: &mut BingBot, email: &str) -> Result<()> {
    let search_words = crate::hot_searches::get_hot_words(50);

    let mut tigger = ExpectedNTrigger::new(GAP_NUM);
    for (i, word) in search_words.into_iter().enumerate() {
        let sleep_time = if tigger.next() {
            match get_mobile_search_process(tab) {
                Ok((cur_points, max_points)) => {
                    info!(
                        "账号 {} 当前搜索积分: {}，今日最大搜索积分: {}",
                        email, cur_points, max_points
                    );
                    if cur_points >= max_points {
                        info!("账号 {} 今日搜索积分已达上限，结束搜索任务", email);
                        break;
                    }
                }
                Err(e) => {
                    shot_when_faild(tab, "mobile_rewards_get_failed", email);
                    warn!("获取账号 {} 积分详情失败: {}", email, e);
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
                let _ = tab.reload(false, None);
                slept += sleep_chunk;
            }
        }

        (|| {
            perform_search_and_click(&mut browser_bot.browser, tab, &word).inspect_err(|_| {
                if let Ok(_) = browser_bot.restart_mobile_browser()
                    && let Ok(new_tab) = get_one_tab(&mut browser_bot.browser)
                {
                    *tab = new_tab;
                }
            })?;
            info!("第 {} 次搜索完成", i + 1);
            Ok(())
        })
        .retry(3)?;
    }
    Ok(())
}

// 虽然和 pc 的一模一样，但是考虑到以后可能会有差异，还是分开写了
fn perform_search_and_click(browser: &mut Browser, tab: &Tab, word: &str) -> Result<()> {
    tab.activate()
        .map_err(|e| anyhow!(format!("activate 失败：{}", e)))?;
    tab.navigate_to(BING_URL)
        .map_err(|e| anyhow!(format!("前往 BING_URL失败：{}", e)))?;
    sleep(Duration::from_secs(2));
    tab.reload(false, None)
        .map_err(|e| anyhow!(format!("重新加载失败：{}", e)))?;

    sleep(Duration::from_secs(1));

    let search_input = tab
        .wait_for_xpath_with_custom_timeout(
            "//input[@name='q']|//*[@id='sb_form_q']",
            Duration::from_secs(10),
        )
        .map_err(|e| anyhow!(format!("寻找输入框失败：{}", e)))?;

    search_input
        .type_into(word)
        .map_err(|e| anyhow!(format!("输入失败：{}", e)))?;

    debug!("输入搜索词：{} 成功", word);

    let before_tabs = browser.get_tabs().lock().unwrap().clone();

    let search_button = tab
        .find_element_by_xpath("//label[@id='search_icon']|//*[@id='sb_form']/label/svg")
        .map_err(|e| anyhow!(format!("寻找搜索按钮失败：{}", e)))?;
    search_button
        .click()
        .map_err(|e| anyhow!(format!("搜索按钮点击失败：{}", e)))?;

    sleep(Duration::from_secs(rand::random_range(1..4)));

    let search_res = tab.wait_for_element("#b_results")?;

    let all_res = search_res.find_elements("li.b_algo")?;

    let ele = all_res
        .get(rand::random_range(0..all_res.len()))
        .ok_or(anyhow!("没有找到搜索结果"))?;

    ele.click()
        .map_err(|e| anyhow!(format!("点击搜索结果失败：{}", e)))?;

    sleep(Duration::from_secs(rand::random_range(5..10)));

    close_tab(before_tabs, browser)?;

    Ok(())
}

fn get_mobile_search_process(tab: &Tab) -> Result<(u32, u32)> {
    tab.navigate_to("https://rewards.bing.com/status/pointsbreakdown")?;
    tab.wait_until_navigated()?;

    let ele = tab
        .wait_for_element_with_custom_timeout(
            "#meeGradientBanner > div > div > div > p",
            Duration::from_secs(40),
        )
        .map_err(|e| anyhow!(format!("没有找到等级：{}", e)))?;

    let text = ele.get_inner_text()?;
    let text = text.trim();
    if matches!(text, "一级" | "Level 1" | "1级" | "Level One" | "1 级") {
        warn!("当前账号处于一级会员，没有移动端搜索积分");
        return Ok((0, 0));
    }

    let ele = tab.wait_for_element_with_custom_timeout(
        "#userPointsBreakdown > div > div:nth-child(2) > div > div:nth-child(2) > div > div.pointsDetail > mee-rewards-user-points-details > div > div > div > div > p.pointsDetail.c-subheading-3.ng-binding",
        Duration::from_secs(40),
    ).map_err(|e| anyhow!(format!("没有找到移动搜索积分：{}", e)))?;

    let text = ele.get_inner_text()?;

    let mut parts = text.split('/');

    let cur_points = parts
        .next()
        .ok_or(anyhow!("没有找到今日分数"))?
        .trim()
        .parse()?;
    let max_points = parts
        .next()
        .ok_or(anyhow!("没有找到今日最大分数"))?
        .trim()
        .parse()?;

    Ok((cur_points, max_points))
}

fn login_bing_mobile(email: &str, password: &str, tab: &Tab) -> Result<()> {
    tab.navigate_to(BING_URL)?;
    tab.wait_until_navigated()?;

    if let Err(e) = (|| {
        tab.reload(false, None)?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(2));
        click_login_button(tab)
    })
    .retry(3)
    {
        warn!("点击登录按钮失败: {}", e);
        return Err(e);
    }

    info!("登录按钮点击成功，准备输入账号密码");
    let email_input = tab
        .wait_for_xpath_with_custom_timeout(
            concat!(
                "//input[@type='email' or @name='loginfmt']",
                "|input[@id='usernameEntry']",
            ),
            Duration::from_secs(10),
        )
        .map_err(|e| anyhow::Error::msg(format!("寻找账号输入位置有误：{}", e)))?;

    email_input.type_into(email)?;

    info!("账号输入成功，准备点击下一步");

    let next_button = tab.find_element_by_xpath(concat!(
        "//button[@type='submit']",
        "|//button[text()='下一步']",
    ))?;

    next_button.click()?;

    let password_input = loop {
        match tab.wait_for_xpath_with_custom_timeout(
            concat!(
                "//input[@type='password' or @name='passwd']",
                "|input[@id='passwordInput']",
            ),
            Duration::from_secs(5),
        ) {
            Ok(input) => break input,
            Err(_e) => {
                let _ = tab
                    .wait_for_xpath_with_custom_timeout(
                        "//*[text()='暂时跳过']",
                        Duration::from_secs(5),
                    )
                    .and_then(|button| button.click().map(|_| ()));

                let _ = tab
                    .wait_for_xpath_with_custom_timeout(concat!(
                        "//span[@role='button' and (text()='其他登录方法' or text()='Other ways to sign in')]",
                        "|//*[text()='其他登录方法']"
                    ), Duration::from_secs(5)).and_then(|button| {
                        button.click().map(|_| ())
                    });

                let button = tab.wait_for_xpath_with_custom_timeout(
                    concat!(
                        "//*[text()='使用密码']",
                        "|//*[text()='Use your password']",
                        "|//button[contains(text(), '使用密码')]",
                        "|//button[contains(text(), 'Use your password')]",
                        "|//a[contains(text(), '使用密码')]",
                        "|//a[contains(text(), 'Use your password')]",
                    ),
                    Duration::from_secs(5),
                )?;

                button.click()?;
            }
        }
    };

    password_input.type_into(password)?;

    info!("密码输入成功，准备点击登录");

    let sign_in_button = tab.find_element_by_xpath(concat!(
        "//button[@type='submit']",
        "|//button[text()='登录']",
        "|//button[text()='Sign in']",
        "|//button[text()='下一步']",
        "|//button[text()='Next']",
    ))?;

    sign_in_button.click()?;

    info!("登录按钮点击成功");

    if let Ok(_) = tab.wait_for_xpath_with_custom_timeout(
        "//*[contains(text(), '保持登录状态')]|//[*contains(text(), 'Stay signed in')]",
        Duration::from_secs(5),
    ) && let Ok(ok_button) =
        tab.find_element_by_xpath(concat!("//button[text()='是']", "|//button[text()='Yes']",))
    {
        let _ = ok_button.click();
    } else if let Ok(ok_button) =
        tab.find_element_by_xpath(concat!("//button[text()='是']", "|//button[text()='Yes']",))
    {
        let _ = ok_button.click();
    }

    info!("登录流程完成");
    Ok(())
}

fn check_login_status(tab: &Tab) -> Result<bool> {
    tab.navigate_to(BING_URL)?;
    tab.wait_until_navigated()?;
    tab.reload(false, None)?;

    let login_button =
        tab.wait_for_element_with_custom_timeout("#mHamburger", Duration::from_secs(5))?;

    login_button.click()?;

    match tab.wait_for_element_with_custom_timeout("#hb_n", Duration::from_secs(5)) {
        Ok(ele) => {
            let status = ele.get_attribute_value("style")?;
            match status.as_deref() {
                Some("display:none") => Ok(false),
                _ => Ok(true),
            }
        }
        Err(_) => Ok(false),
    }
}

fn click_login_button(tab: &Tab) -> Result<()> {
    let login_button =
        tab.wait_for_element_with_custom_timeout("#mHamburger", Duration::from_secs(5))?;

    login_button.click()?;

    let login_button =
        tab.wait_for_element_with_custom_timeout("#hb_a > img", Duration::from_secs(5))?;

    login_button.click()?;
    Ok(())
}
