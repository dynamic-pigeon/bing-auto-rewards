use std::{mem::ManuallyDrop, path::PathBuf, thread::sleep, time::Duration};

use headless_chrome::{Browser, LaunchOptionsBuilder, Tab};
use log::{debug, info, warn};
use rand::seq::SliceRandom;
use serde_json::Value;

use crate::bing::{
    BING_URL, BingBot, GAP_RANGE, HEADLESS, SLEEP_RANGE, close_tab, get_today_rewards,
    retry::Retryable, shot_when_faild,
};

use anyhow::{Result, anyhow};

impl BingBot {
    pub(crate) fn new_mobile_browser(
        _store_local: bool,
        account: &str,
        browser_path: &Option<String>,
    ) -> Self {
        // 为浏览器实例创建临时目录，避免多个账号互相污染
        std::fs::create_dir_all("./tmp").unwrap();
        let temp_dir = None;

        // mobile 的 cookie 不生效，为什么？
        // 总之先不存储了
        let store_local = false;

        // 根据是否持久化选择用户目录
        let user_dir = if store_local {
            std::fs::create_dir_all("./user-data").unwrap();
            let dir_name = format!("./user-data/mobile_{}", account);
            Some(std::path::PathBuf::from(dir_name))
        } else {
            let dir = tempfile::TempDir::new_in("./tmp").unwrap();
            Some(dir.path().to_path_buf())
        };

        let options = default_options_builder()
            .path(browser_path.clone().map(PathBuf::from))
            .user_data_dir(user_dir)
            .build()
            .unwrap();
        let browser = Browser::new(options).unwrap();
        BingBot {
            browser: ManuallyDrop::new(browser),
            temp_dir,
        }
    }
}

pub(crate) fn process_account(email: &str, password: &str, browser: &mut Browser) -> Result<()> {
    let tab = browser.new_tab()?;
    tab.set_default_timeout(Duration::from_secs(25));
    info!("开始处理移动端账号: {}", email);
    // 闭包封装登录流程，方便统一重试
    let ensure_logged_in = || -> Result<()> {
        if check_login_status(&tab)? {
            info!("账号 {} 已登录，无需重复登录", email);
            return Ok(());
        }

        login_bing_mobile(email, password, &tab)?;
        sleep(Duration::from_secs(5));

        if !check_login_status(&tab)? {
            return Err(anyhow!("登录后检查状态发现未登录"));
        }

        Ok(())
    };

    ensure_logged_in.retry(3).inspect_err(|_| {
        shot_when_faild(&tab, "mobile_login", email);
    })?;

    sleep(Duration::from_secs(5));

    info!("账号 {} 登录成功，开始搜索任务", email);
    search(&tab, browser, email).inspect_err(|_| {
        shot_when_faild(&tab, "mobile_search", email);
    })?;
    Ok(())
}

fn search(tab: &Tab, browser: &mut Browser, email: &str) -> Result<()> {
    // 搜索词列表来源于多个热搜渠道
    let search_words = get_search_words(tab)?;

    for (i, word) in search_words.into_iter().enumerate() {
        // 每隔 5 次查询一次积分进度，避免频繁访问积分页面
        let need_progress_check = i % 5 == 0;
        if need_progress_check {
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
        }

        // 随机睡眠一段时间，模拟真实用户行为
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
            // 时间较长时分段睡眠并刷新页面，避免 tab 超时
            let mut slept = 0;
            while slept < sleep_time {
                let sleep_chunk = std::cmp::min(MAX_SLEEP_TIME, sleep_time - slept);
                sleep(Duration::from_secs(sleep_chunk));
                // 空转防止 timeout
                let _ = tab.reload(false, None);
                slept += sleep_chunk;
            }
        }

        // 单次搜索流程封装成闭包，便于重试
        let run_single_search = || -> Result<()> {
            use anyhow::Error;
            tab.activate()
                .map_err(|e| Error::msg(format!("activate 失败：{}", e)))?;
            tab.navigate_to(BING_URL)
                .map_err(|e| Error::msg(format!("前往 BING_URL失败：{}", e)))?;
            sleep(Duration::from_secs(2));
            tab.reload(false, None)
                .map_err(|e| Error::msg(format!("重新加载失败：{}", e)))?;

            sleep(Duration::from_secs(1));

            let input_xpath = "//input[@name='q']|//*[@id='sb_form_q']";
            let search_input = tab
                .wait_for_xpath_with_custom_timeout(input_xpath, Duration::from_secs(10))
                .map_err(|e| anyhow::Error::msg(format!("寻找输入框失败：{}", e)))?;

            search_input
                .type_into(&word)
                .map_err(|e| Error::msg(format!("输入失败：{}", e)))?;

            debug!("输入搜索词：{} 成功", &word);

            // 记录点击前的标签页列表，方便后续关闭新增标签页
            let before_tabs = browser.get_tabs().lock().unwrap().clone();
            let search_button_xpath = "//label[@id='search_icon']";
            let search_button = tab
                .find_element_by_xpath(search_button_xpath)
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
        };

        run_single_search.retry(3)?;

        match get_today_rewards(tab) {
            Ok(points) => info!("账号 {} 今日搜索积分: {}", email, points),
            Err(e) => warn!("获取账号 {} 今日积分失败: {}", email, e),
        }
    }
    Ok(())
}

fn get_mobile_search_process(tab: &Tab) -> Result<(u32, u32)> {
    // Rewards 页面 UI 变化较多，此处仅抓取必要信息
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

fn get_search_words(tab: &Tab) -> Result<Vec<String>> {
    info!("开始获取360热搜");
    if let Ok(hot) = (|| {
        tab.navigate_to("https://ranks.hao.360.com/")?;
        tab.wait_until_navigated()?;
        tab.wait_for_element_with_custom_timeout("#main > div > div.center-section.svelte-1xaaya4 > ul > li:nth-child(1) > a > div.text.svelte-10xd19r > div.title.svelte-10xd19r", Duration::from_secs(15))?;
        let html = tab.get_content()?;
        let document = scraper::Html::parse_document(&html);
        let selector = scraper::Selector::parse(" #main > div > div.center-section.svelte-1xaaya4 > ul > li > a > div.text.svelte-10xd19r > div.title.svelte-10xd19r ").unwrap();
        let mut hot_words = document
            .select(&selector)
            .take(80)
            .map(|ele| ele.text().collect::<String>())
            .collect::<Vec<_>>();

        hot_words.shuffle(&mut rand::rng());

        if !hot_words.is_empty() {
            info!("成功获取到搜索热词");
            Ok(hot_words)
        } else {
            Err(anyhow!("没有找到热词"))
        }
    })() {
        return Ok(hot);
    }
    warn!("获取360热搜失败");

    info!("开始获取zhihu热搜");
    if let Ok(hot) = (|| {
        let json: Value =
            reqwest::blocking::get("https://uapis.cn/api/v1/misc/hotboard?type=zhihu")?.json()?;
        let mut hot_words = json["list"]
            .as_array()
            .ok_or(anyhow!(""))?
            .iter()
            .map(|e| e["title"].as_str().unwrap().to_string())
            .take(80)
            .collect::<Vec<_>>();

        hot_words.shuffle(&mut rand::rng());

        if !hot_words.is_empty() {
            info!("成功获取到知乎热搜");
            Ok(hot_words)
        } else {
            Err(anyhow!("没有找到热词"))
        }
    })() {
        return Ok(hot);
    }
    warn!("获取zhihu热搜失败");

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

fn login_bing_mobile(email: &str, password: &str, tab: &Tab) -> Result<()> {
    tab.navigate_to(BING_URL)?;
    tab.wait_until_navigated()?;

    let click_login = || -> Result<()> {
        tab.reload(false, None)?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(2));
        click_login_button(tab)
    };

    // 登录入口经常更新，重试三次提升成功率
    if let Err(e) = click_login.retry(3) {
        warn!("点击登录按钮失败: {}", e);
        return Err(e);
    }

    info!("登录按钮点击成功，准备输入账号密码");
    let email_input_xpath = concat!(
        "//input[@type='email' or @name='loginfmt']",
        "|input[@id='usernameEntry']",
    );
    let email_input = tab
        .wait_for_xpath_with_custom_timeout(email_input_xpath, Duration::from_secs(10))
        .map_err(|e| anyhow::Error::msg(format!("寻找账号输入位置有误：{}", e)))?;

    email_input.type_into(email)?;

    info!("账号输入成功，准备点击下一步");

    let next_button_xpath = concat!("//button[@type='submit']", "|//button[text()='下一步']");
    let next_button = tab.find_element_by_xpath(next_button_xpath)?;
    next_button.click()?;

    // 不同登录页面密码框不一致，通过循环等待兼容多种布局
    let password_input = loop {
        let password_input_xpath = concat!(
            "//input[@type='password' or @name='passwd']",
            "|input[@id='passwordInput']",
        );

        match tab.wait_for_xpath_with_custom_timeout(password_input_xpath, Duration::from_secs(5)) {
            Ok(input) => break input,
            Err(_e) => {
                let other_way_xpath = concat!(
                    "//span[@role='button' and (text()='其他登录方法' or text()='Other ways to sign in')]",
                    "|//*[text()='其他登录方法']",
                );
                let _ = tab
                    .wait_for_xpath_with_custom_timeout(other_way_xpath, Duration::from_secs(5))
                    .and_then(|button| button.click().map(|_| ()));

                let use_password_xpath = concat!(
                    "//*[text()='使用密码']",
                    "|//*[text()='Use your password']",
                    "|//button[contains(text(), '使用密码')]",
                    "|//button[contains(text(), 'Use your password')]",
                    "|//a[contains(text(), '使用密码')]",
                    "|//a[contains(text(), 'Use your password')]",
                );
                let button = tab.wait_for_xpath_with_custom_timeout(
                    use_password_xpath,
                    Duration::from_secs(5),
                )?;

                button.click()?;
            }
        }
    };

    password_input.type_into(password)?;

    info!("密码输入成功，准备点击登录");

    let sign_in_button_xpath = concat!(
        "//button[@type='submit']",
        "|//button[text()='登录']",
        "|//button[text()='Sign in']",
        "|//button[text()='下一步']",
        "|//button[text()='Next']",
    );
    let sign_in_button = tab.find_element_by_xpath(sign_in_button_xpath)?;
    sign_in_button.click()?;

    info!("登录按钮点击成功");

    let stay_signed_in_xpath =
        "//*[contains(text(), '保持登录状态')]|//[*contains(text(), 'Stay signed in')]";
    let confirm_button_xpath = concat!("//button[text()='是']", "|//button[text()='Yes']",);

    if let Ok(_) =
        tab.wait_for_xpath_with_custom_timeout(stay_signed_in_xpath, Duration::from_secs(5))
    {
        if let Ok(ok_button) = tab.find_element_by_xpath(confirm_button_xpath) {
            let _ = ok_button.click();
        }
    } else if let Ok(ok_button) = tab.find_element_by_xpath(confirm_button_xpath) {
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
    let hamburger_button =
        tab.wait_for_element_with_custom_timeout("#mHamburger", Duration::from_secs(5))?;
    hamburger_button.click()?;

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
        .idle_browser_timeout(Duration::from_secs(120))
        .args(
            [
                "--disable-dev-shm-usage",
                "--disable-extensions",
                "--disable-blink-features=AutomationControlled",
                "--no-sandbox",
                "--allow-running-insecure-content",
                "--user-agent=Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Mobile Safari/537.36",
            ]
            .into_iter()
            .map(std::ffi::OsStr::new)
            .collect(),
        );
    options
}
