use std::{mem::ManuallyDrop, thread::sleep, time::Duration};

use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};
use log::{debug, info, warn};
use rand::seq::SliceRandom;

use crate::bing::{
    BING_URL, BingBot, GAP_RANGE, HEADLESS, SLEEP_RANGE, close_tab, get_today_rewards, retry,
    shot_with_faild,
};

use anyhow::{Result, anyhow};

impl BingBot {
    pub(crate) fn new_mobile() -> Self {
        std::fs::create_dir_all("./tmp").unwrap();
        let temp_dir = tempfile::tempdir_in("./tmp").unwrap();
        let options = default_options_builder()
            .user_data_dir(Some(temp_dir.path().to_path_buf()))
            .build()
            .unwrap();
        let browser = Browser::new(options).unwrap();
        BingBot {
            browser: ManuallyDrop::new(browser),
            _temp_dir: ManuallyDrop::new(temp_dir),
        }
    }
}

pub(crate) fn process_account(email: &str, password: &str, browser: &mut Browser) -> Result<()> {
    let tab = browser.new_tab()?;
    info!("开始处理移动端账号: {}", email);
    login_bing_mobile(email, password, &tab).map_err(|e| {
        let _ = shot_with_faild(&tab, "mobile_login", email);
        e
    })?;

    sleep(Duration::from_secs(5));

    info!("账号 {} 登录成功，开始搜索任务", email);
    search(&tab, browser, email).map_err(|e| {
        let _ = shot_with_faild(&tab, "mobile_search", email);
        e
    })?;
    Ok(())
}

fn search(tab: &Tab, browser: &mut Browser, email: &str) -> Result<()> {
    let search_words = get_search_words(tab)?;

    for (i, word) in search_words.into_iter().enumerate() {
        let sleep_time = if (i + 1) % 5 == 0 {
            rand::random_range(GAP_RANGE)
        } else {
            rand::random_range(SLEEP_RANGE)
        };

        info!("{} 秒后开始第 {} 次搜索：{}", sleep_time, i + 1, word);
        const MAX_SLEEP_TIME: u64 = 10;
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

        retry(
            || {
                use anyhow::Error;
                tab.activate()
                    .map_err(|e| Error::msg(format!("activate 失败：{}", e)))?;
                tab.navigate_to(BING_URL)
                    .map_err(|e| Error::msg(format!("前往 BING_URL失败：{}", e)))?;
                sleep(Duration::from_secs(2));
                tab.reload(false, None)
                    .map_err(|e| Error::msg(format!("重新加载失败：{}", e)))?;

                sleep(Duration::from_secs(1));

                let search_input = tab
                    .wait_for_xpath_with_custom_timeout(
                        "//input[@name='q']",
                        Duration::from_secs(10),
                    )
                    .map_err(|e| anyhow::Error::msg(format!("寻找输入框失败：{}", e)))?;

                search_input
                    .type_into(&word)
                    .map_err(|e| Error::msg(format!("输入失败：{}", e)))?;

                debug!("输入搜索词：{} 成功", &word);

                let before_tabs = browser.get_tabs().lock().unwrap().clone();

                let search_button = tab
                    .find_element_by_xpath("//label[@id='search_icon']")
                    .map_err(|e| Error::msg(format!("寻找搜索按钮失败：{}", e)))?;
                search_button
                    .click()
                    .map_err(|e| anyhow::Error::msg(format!("搜索按钮点击失败：{}", e)))?;

                sleep(Duration::from_secs(rand::random_range(5..10)));

                close_tab(before_tabs, browser)?;

                info!("第 {} 次搜索完成", i + 1);

                Ok(())
            },
            3,
        )?;

        match get_today_rewards(&tab) {
            Ok(points) => {
                info!("账号 {} 今日搜索积分: {}", email, points);
            }
            Err(e) => {
                warn!("获取账号 {} 今日积分失败: {}", email, e);
            }
        }

        if i % 5 == 0 {
            match get_mobile_search_process(&tab) {
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
                    warn!("获取账号 {} 积分详情失败: {}", email, e);
                }
            }
        }
    }
    Ok(())
}

fn get_mobile_search_process(tab: &Tab) -> Result<(u32, u32)> {
    tab.navigate_to("https://rewards.bing.com/status/pointsbreakdown")?;
    tab.wait_until_navigated()?;

    let ele = tab.wait_for_element("#meeGradientBanner > div > div > div > p")?;

    let text = ele.get_inner_text()?;
    let text = text.trim();
    if text == "1级" || text == "一级" {
        warn!("当前账号处于一级会员，无法获取搜索积分详情");
        return Ok((0, 0));
    }

    let ele = tab.wait_for_element_with_custom_timeout(
        "#userPointsBreakdown > div > div:nth-child(2) > div > div:nth-child(2) > div > div.pointsDetail > mee-rewards-user-points-details > div > div > div > div > p.pointsDetail.c-subheading-3.ng-binding",
        Duration::from_secs(15),
    )?;

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

fn get_search_words(tab: &Tab) -> Result<Vec<String>> {
    if let Ok(hot) = (|| {
        tab.navigate_to("https://ranks.hao.360.com/")?;
        tab.wait_until_navigated()?;
        tab.wait_for_element("#main > div > div.center-section.svelte-1xaaya4 > ul > li:nth-child(1) > a > div.text.svelte-10xd19r > div.title.svelte-10xd19r")?;
        let html = tab.get_content()?;
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse(" #main > div > div.center-section.svelte-1xaaya4 > ul > li > a > div.text.svelte-10xd19r > div.title.svelte-10xd19r ").unwrap();
        let mut hot_words = document
            .select(&selector)
            .into_iter()
            .take(80)
            .map(|ele| ele.text().collect::<String>())
            .collect::<Vec<_>>();

        hot_words.shuffle(&mut rand::rng());

        let hot_words = hot_words.into_iter().take(40).collect::<Vec<_>>();
        if !hot_words.is_empty() {
            info!("成功获取到搜索热词");
            Ok(hot_words)
        } else {
            Err(anyhow!("没有找到热词"))
        }
    })() {
        return Ok(hot);
    }

    info!("返回默认热词");
    return Ok([
        "python",
        "bing",
        "ai",
        "chatgpt",
        "微软",
        "天气",
        "NBA",
        "世界杯",
        "科技新闻",
        "人工智能",
        "股票",
        "电影",
        "电视剧",
        "旅游",
        "健康",
        "教育",
        "汽车",
        "手机",
        "数码",
        "美食",
        "历史",
        "地理",
        "音乐",
        "游戏",
        "动漫",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect());
}

fn login_bing_mobile(email: &str, password: &str, tab: &Tab) -> Result<()> {
    // Implement login logic here
    tab.navigate_to(BING_URL).unwrap();
    sleep(Duration::from_secs(2));

    if let Err(e) = retry(
        || {
            tab.reload(false, None).unwrap();
            tab.wait_until_navigated().unwrap();
            sleep(Duration::from_secs(2));
            click_login_button(&tab)
        },
        3,
    ) {
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
        .map_err(|e| anyhow::Error::msg(format!("寻找账号输入位置有误：{}", e.to_string())))?;

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
                    .wait_for_xpath_with_custom_timeout(concat!(
                        "//span[@role='button' and (text()='其他登录方法' or text()='Use another way to sign in')]",
                        "|//*[text()='其他登录方法']"
                    ), Duration::from_secs(5)).and_then(|button| {
                        button.click().map(|_| ())
                    });

                let button = tab.wait_for_xpath_with_custom_timeout(
                    concat!(
                        "//*[text()='使用密码']",
                        "|//*[text()='Use password']",
                        "|//button[contains(text(), '使用密码')]",
                        "|//button[contains(text(), 'Use password')]",
                        "|//a[contains(text(), '使用密码')]",
                        "|//a[contains(text(), 'Use password')]",
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

fn click_login_button(tab: &Tab) -> Result<()> {
    let login_button =
        tab.wait_for_element_with_custom_timeout("#mHamburger", Duration::from_secs(5))?;

    login_button.click()?;

    let login_button =
        tab.wait_for_element_with_custom_timeout("#hb_a > img", Duration::from_secs(5))?;

    login_button.click()?;
    Ok(())
}

fn default_options_builder() -> LaunchOptionsBuilder<'static> {
    let mut options = LaunchOptionsBuilder::default();
    options
        .headless(HEADLESS)
        .enable_gpu(false)
        .window_size(Some((770, 1600)))
        .args(
            [
                "--incognito",
                "--disable-dev-shm-usage",
                "--disable-extensions",
                "--disable-blink-features=AutomationControlled",
                "--no-sandbox",
                "--allow-running-insecure-content",
                "--user-agent=Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Mobile Safari/537.36"
            ]
            .into_iter()
            .map(std::ffi::OsStr::new)
            .collect(),
        );
    options
}
