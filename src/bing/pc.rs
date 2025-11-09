use std::{mem::ManuallyDrop, path::PathBuf, thread::sleep, time::Duration};

use headless_chrome::{Browser, Tab};
use log::{debug, info, warn};
use rand::seq::SliceRandom;

use crate::bing::{
    BING_URL, BingBot, GAP_RANGE, REWARDS_URL, SLEEP_RANGE, close_tab, default_options_builder,
    get_today_rewards, retry::Retryable, shot_when_faild,
};

use anyhow::{Result, anyhow};

impl BingBot {
    pub(crate) fn new_pc_browser(
        store_local: bool,
        account: &str,
        browser_path: &Option<String>,
        proxy: &Option<String>,
    ) -> BingBot {
        std::fs::create_dir_all("./tmp").unwrap();
        let temp_dir = None;

        let user_dir = if store_local {
            std::fs::create_dir_all("./user-data").unwrap();
            Some(std::path::PathBuf::from(format!(
                "./user-data/pc_{}",
                account
            )))
        } else {
            let dir = tempfile::TempDir::new_in("./tmp").unwrap();
            Some(dir.path().to_path_buf())
        };
        let options = default_options_builder()
            .path(browser_path.clone().map(PathBuf::from))
            .user_data_dir(user_dir)
            .window_size(Some((1920, 1080)))
            .proxy_server(proxy.as_deref())
            .build()
            .unwrap();
        let browser = Browser::new(options).unwrap();
        BingBot {
            browser: ManuallyDrop::new(browser),
            temp_dir,
        }
    }
}

/// 为什么是 &mut BingBot 而不是 &BingBot
///
/// 其实是借用了 rust 单一所有权的特性，保证同一时间只有一个可变引用在使用 browser
pub(crate) fn process_account(email: &str, password: &str, browser: &mut BingBot) -> Result<()> {
    info!("开始登录Bing账号: {}", email);
    let browser = &mut browser.browser;
    let tab = browser.new_tab()?;
    tab.set_default_timeout(Duration::from_secs(25));
    (|| {
        if !check_login_status(&tab)? {
            login_bing(email, password, &tab)?;
            sleep(Duration::from_secs(5));
            if !check_login_status(&tab)? {
                return Err(anyhow!("登录后检查状态仍然未登录"));
            }
        } else {
            info!("账号 {} 已登录，无需重复登录", email);
        }

        Ok(())
    })
    .retry(3)
    .inspect_err(|_| {
        shot_when_faild(&tab, "login", email);
    })?;

    sleep(Duration::from_secs(5));

    info!("开始尝试点击卡片");
    let _ = click_rewards(browser, &tab).inspect_err(|_| {
        shot_when_faild(&tab, "click_rewards", email);
    });

    sleep(Duration::from_secs(5));

    info!("开始进行搜索任务");
    search(browser, email, &tab).inspect_err(|_| {
        shot_when_faild(&tab, "search", email);
    })?;

    info!("{} 账号处理完成", email);
    Ok(())
}

fn search(browser: &mut Browser, email: &str, tab: &Tab) -> Result<()> {
    let search_words = get_search_words(tab)?;

    for (i, word) in search_words.into_iter().enumerate() {
        if i % 5 == 0 {
            match get_pc_search_process(tab) {
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
        }
        let sleep_time = if (i + 1) % 5 == 0 {
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
        const MAX_SLEEP_TIME: u64 = 60;
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
                    "//input[@name='q']|//*[@id='sb_form_q']",
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

            sleep(Duration::from_secs(rand::random_range(1..4)));

            let search_res = tab.wait_for_element("#b_results")?;

            let all_res = search_res.find_elements("li.b_algo")?;

            let ele = all_res
                .get(rand::random_range(0..all_res.len()))
                .ok_or(anyhow!("没有找到搜索结果"))?;

            ele.click()
                .map_err(|e| anyhow::Error::msg(format!("点击搜索结果失败：{}", e)))?;

            sleep(Duration::from_secs(rand::random_range(5..10)));

            close_tab(before_tabs, browser)?;

            info!("第 {} 次搜索完成", i + 1);

            Ok(())
        })
        .retry(3)?;

        match get_today_rewards(tab) {
            Ok(points) => {
                info!("账号 {} 今日搜索积分: {}", email, points);
            }
            Err(e) => {
                warn!("获取账号 {} 今日积分失败: {}", email, e);
            }
        }
    }

    Ok(())
}

fn click_rewards(browser: &mut Browser, tab: &Tab) -> Result<()> {
    tab.navigate_to(REWARDS_URL)?;
    tab.wait_until_navigated()?;
    tab.reload(false, None)?;

    sleep(Duration::from_secs(2));
    info!("开始寻找可点击卡片");

    tab.wait_for_element_with_custom_timeout(".c-card-content a", Duration::from_secs(30))?;

    let cards = tab
        .find_elements(".c-card-content a")?
        .into_iter()
        .filter(|ele| ele.find_element(".mee-icon-AddMedium").is_ok())
        .collect::<Vec<_>>();

    info!("找到 {} 个可点击卡片", cards.len());

    for card in cards {
        let before_tabs = browser.get_tabs().lock().unwrap().clone();

        // 有些页面元素可能被遮挡，直接调用 JS 的 click() 更稳健
        match card.call_js_fn("function() { this.click(); }", vec![], false) {
            Ok(_) => info!("通过 JS 点击卡片成功"),
            Err(e) => warn!("通过 JS 点击卡片失败：{}", e),
        }

        sleep(Duration::from_secs(5));
        close_tab(before_tabs, browser)?;
    }

    info!("卡片点击完成");
    Ok(())
}

fn login_bing(email: &str, password: &str, tab: &Tab) -> Result<()> {
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
    sleep(Duration::from_secs(2));

    match tab.wait_for_element_with_custom_timeout("#id_s", Duration::from_secs(3)) {
        Ok(ele) => {
            let status = ele.get_attribute_value("aria-hidden")?;
            match status.as_deref() {
                Some("true") => Ok(true),
                Some("false") => Ok(false),
                None => Err(anyhow!("没有找到")),
                _ => Err(anyhow!("未知状态")),
            }
        }
        Err(_) => Ok(false),
    }
}

fn click_login_button(tab: &Tab) -> Result<()> {
    let login_button =  tab.wait_for_xpath_with_custom_timeout(concat!(
            "//span[@id='id_s']",
            "|//*[@id='id_a']",
            "|//a[@id='id_l']",
            "|//a[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//a[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//a[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
            "|//button[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//button[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//button[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
        ), Duration::from_secs(10))?;

    info!("找到登录按钮，准备点击");
    sleep(Duration::from_secs(2));
    login_button.click()?;
    tab.wait_until_navigated()?;
    Ok(())
}

fn get_pc_search_process(tab: &Tab) -> Result<(u32, u32)> {
    tab.navigate_to("https://rewards.bing.com/pointsbreakdown")?;
    tab.wait_until_navigated()?;

    let ele = tab.wait_for_element_with_custom_timeout(
        "#userPointsBreakdown > div > div:nth-child(2) > div > div:nth-child(1) > div > div.pointsDetail > mee-rewards-user-points-details > div > div > div > div > p.pointsDetail.c-subheading-3.ng-binding",
        Duration::from_secs(40),
    ).map_err(|e| anyhow!(format!("没有找到电脑搜索积分：{}", e)))?;

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
    info!("开始获取百度搜索热词");
    if let Ok(hot_words) = (|| {
        tab.navigate_to("https://top.baidu.com/board?tab=realtime")?;
        tab.wait_for_element_with_custom_timeout(
            ".c-single-text-ellipsis",
            Duration::from_secs(3),
        )?;

        let html = tab.get_content()?;
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse(".c-single-text-ellipsis").unwrap();

        let mut hot_words = document
            .select(&selector)
            .take(80)
            .map(|ele| ele.text().collect::<String>())
            .collect::<Vec<_>>();

        hot_words.shuffle(&mut rand::rng());

        if !hot_words.is_empty() {
            info!("成功获取到百度搜索热词");
            Ok(hot_words)
        } else {
            Err(anyhow!("没有找到热词"))
        }
    })() {
        return Ok(hot_words);
    }
    warn!("获取百度搜索热词失败");

    info!("开始获取微博热搜");
    if let Ok(hot_words) = (|| {
        tab.navigate_to("https://s.weibo.com/top/summary")?;
        tab.wait_for_element_with_custom_timeout(
            "#pl_top_realtimehot > table > tbody > tr:nth-child(2) > td.td-02 > a",
            Duration::from_secs(10),
        )?;

        let html = tab.get_content()?;
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse(
            "#pl_top_realtimehot > table > tbody > tr:nth-child(n) > td.td-02 > a",
        )
        .unwrap();
        let mut hot_words = document
            .select(&selector)
            .take(80)
            .map(|ele| ele.text().collect::<String>())
            .collect::<Vec<_>>();

        hot_words.shuffle(&mut rand::rng());

        if !hot_words.is_empty() {
            info!("成功获取到微博热搜");
            Ok(hot_words)
        } else {
            Err(anyhow!("没有找到热词"))
        }
    })() {
        return Ok(hot_words);
    }
    warn!("获取微博热搜失败");

    info!("返回默认热词");

    Ok([
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
    .collect())
}
