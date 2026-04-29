use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::Arc,
    thread::sleep,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use headless_chrome::{Browser, Tab};
use log::{debug, info, warn};
use rand::seq::IndexedRandom;

use crate::{
    bing::{
        BING_URL, BingBot, GAP_NUM, GAP_RANGE, REWARDS_URL, REWARDS_URL_DS, SLEEP_RANGE, close_tab,
        default_options_builder, get_one_tab, retry::Retryable, shot_when_faild,
    },
    random::ExpectedNTrigger,
};

static PC_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
const MAX_PC_SEARCH_TIMES: usize = 20;

impl BingBot {
    pub(crate) fn new_pc_browser(
        &mut self,
        store_local: bool,
        account: &str,
        browser_path: &Option<String>,
        proxy: &Option<String>,
    ) -> Result<()> {
        let temp_dir = None;

        let user_dir = if store_local {
            std::fs::create_dir_all("./user-data")?;
            let user_data_dir = std::path::PathBuf::from(format!("./user-data/pc_{}", account));
            std::fs::create_dir_all(&user_data_dir)?;
            mark_user_data_last_used(&user_data_dir)?;
            Some(user_data_dir)
        } else {
            std::fs::create_dir_all("./tmp")?;
            let dir = tempfile::TempDir::new_in("./tmp")?;
            Some(dir.path().to_path_buf())
        };
        let options = default_options_builder(vec![OsStr::new(const_format::formatcp!(
            "--user-agent='{}'",
            PC_USER_AGENT
        ))])
        .path(browser_path.as_deref().map(PathBuf::from))
        .user_data_dir(user_dir)
        .window_size(Some((1920, 1080)))
        .proxy_server(proxy.as_deref())
        .build()?;

        let browser = Browser::new(options)?;
        self.browser = Some(browser);
        self.temp_dir = temp_dir;
        self.store_local = store_local;
        self.account = account.to_string();
        self.browser_path = browser_path.clone();
        self.proxy = proxy.clone();
        Ok(())
    }

    pub(crate) fn restart_pc_browser(&mut self) -> Result<()> {
        self.browser.take();
        let user_dir = if self.store_local {
            std::fs::create_dir_all("./user-data")?;
            let user_data_dir =
                std::path::PathBuf::from(format!("./user-data/pc_{}", self.account));
            std::fs::create_dir_all(&user_data_dir)?;
            mark_user_data_last_used(&user_data_dir)?;
            Some(user_data_dir)
        } else {
            Some(self.temp_dir.as_ref().unwrap().path().to_path_buf())
        };
        let options = default_options_builder(vec![OsStr::new(const_format::formatcp!(
            "--user-agent='{}'",
            PC_USER_AGENT
        ))])
        .path(self.browser_path.as_deref().map(PathBuf::from))
        .user_data_dir(user_dir)
        .window_size(Some((1920, 1080)))
        .proxy_server(self.proxy.as_deref())
        .build()?;
        let browser = Browser::new(options)?;

        self.browser = Some(browser);
        debug!("浏览器重启完成");
        Ok(())
    }
}

fn mark_user_data_last_used(user_data_dir: &Path) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs();
    std::fs::write(user_data_dir.join(".last_used"), now.to_string())?;
    Ok(())
}

/// 为什么是 &mut BingBot 而不是 &BingBot
///
/// 其实是借用了 rust 单一所有权的特性，保证同一时间只有一个可变引用在使用 browser
pub(crate) fn process_account(
    email: &str,
    password: &str,
    browser_bot: &mut BingBot,
) -> Result<()> {
    info!("开始登录Bing账号: {}", email);
    let browser = browser_bot.get_browser()?;
    let mut tab = get_one_tab(browser)?;
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
    search(browser_bot, email, &mut tab).inspect_err(|_| {
        shot_when_faild(&tab, "search", email);
    })?;

    info!("{} 账号处理完成", email);
    Ok(())
}

fn search(browser_bot: &mut BingBot, email: &str, tab: &mut Arc<Tab>) -> Result<()> {
    let search_words = crate::hot_searches::get_hot_words(MAX_PC_SEARCH_TIMES);

    let mut trigger = ExpectedNTrigger::new(GAP_NUM);
    for (i, word) in search_words.into_iter().enumerate() {
        let sleep_time = if trigger.next() {
            match get_pc_search_process(tab) {
                Ok((cur_points, max_points)) => {
                    info!(
                        "账号 {email} 当前搜索积分: {cur_points}，今日最大搜索积分: {max_points}"
                    );
                    if cur_points >= max_points {
                        info!("账号 {email} 今日搜索积分已达上限，结束搜索任务");
                        break;
                    }
                }
                Err(e) => {
                    shot_when_faild(tab, "rewards_get_failed", email);
                    warn!("获取账号 {email} 积分详情失败: {e}");
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
            perform_search_and_click(browser_bot.get_browser()?, tab, &word).inspect_err(|_| {
                if let Ok(_) = browser_bot.restart_pc_browser()
                    && let Ok(new_tab) = get_one_tab(browser_bot.get_browser().expect("怎么回事呢"))
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

fn perform_search_and_click(browser: &mut Browser, tab: &Tab, word: &str) -> Result<()> {
    let before_tabs = browser.get_tabs().lock().unwrap().clone();

    tab.navigate_to(&format!(
        "https://cn.bing.com/search?q={}&PC=U316&FORM=CHROMN",
        word
    ))?;

    sleep(Duration::from_secs(rand::random_range(1..4)));

    let search_res = tab.wait_for_element("#b_results")?;

    search_res
        .wait_for_element("li.b_algo")
        .map_err(|e| anyhow!(format!("没有找到搜索结果：{e}")))?;

    let all_res = search_res.find_elements("li.b_algo")?;

    let ele = all_res
        .choose(&mut rand::rng())
        .ok_or(anyhow!("没有找到搜索结果"))?;

    ele.click()
        .map_err(|e| anyhow!(format!("点击搜索结果失败：{e}")))?;

    sleep(Duration::from_secs(rand::random_range(5..10)));

    close_tab(before_tabs, browser)?;

    Ok(())
}

fn click_rewards(browser: &mut Browser, tab: &Tab) -> Result<()> {
    (|| click_daily_set(browser, tab))
        .retry(3)
        .inspect_err(|e| {
            warn!("点击奖励卡片失败: {e}");
        })?;
    (|| click_earn(browser, tab)).retry(3).inspect_err(|e| {
        warn!("点击奖励卡片失败: {e}");
    })?;

    info!("卡片点击完成");
    Ok(())
}

fn click_earn(browser: &mut Browser, tab: &Tab) -> Result<()> {
    tab.navigate_to(REWARDS_URL)?;
    tab.wait_until_navigated()?;

    let ele = tab.wait_for_element("#moreactivities > div > div:nth-of-type(2)")?;
    let ele = ele.wait_for_elements("a")?;
    info!("找到 {} 个奖励卡片，准备点击", ele.len());

    for card in ele {
        let text = card.get_inner_text().unwrap_or_default();
        if !text.contains("+") {
            continue;
        }

        let before_tabs = browser.get_tabs().lock().unwrap().clone();
        match card.call_js_fn("function() { this.click(); }", vec![], false) {
            Ok(_) => info!("通过 JS 点击奖励卡片成功"),
            Err(e) => warn!("通过 JS 点击奖励卡片失败：{}", e),
        }
        sleep(Duration::from_secs(5));
        let _ = close_tab(before_tabs, browser);
        sleep(Duration::from_secs(1));
    }

    Ok(())
}

fn click_daily_set(browser: &mut Browser, tab: &Tab) -> Result<()> {
    tab.navigate_to(REWARDS_URL_DS)?;
    tab.wait_until_navigated()?;
    let ele = tab.wait_for_element("#dailyset > div > div:nth-of-type(2)")?;
    let ele = ele.wait_for_elements("a")?;
    info!("找到 {} 个每日任务卡片，准备点击", ele.len());

    for card in ele {
        let before_tabs = browser.get_tabs().lock().unwrap().clone();
        match card.call_js_fn("function() { this.click(); }", vec![], false) {
            Ok(_) => info!("通过 JS 点击每日任务卡片成功"),
            Err(e) => warn!("通过 JS 点击每日任务卡片失败：{}", e),
        }
        sleep(Duration::from_secs(5));
        let _ = close_tab(before_tabs, browser);
        sleep(Duration::from_secs(1));
    }

    Ok(())
}

pub(super) fn login_bing(email: &str, password: &str, tab: &Tab) -> Result<()> {
    tab.activate()?;
    tab.navigate_to(BING_URL)?;
    tab.wait_until_navigated()?;
    tab.reload(true, None)?;
    tab.wait_until_navigated()?;

    if let Err(e) = (|| {
        tab.reload(false, None)?;
        tab.wait_until_navigated()?;
        sleep(Duration::from_secs(2));
        click_login_button(tab)
    })
    .retry(3)
    {
        debug!("当前页面：{}", tab.get_url());
        warn!("点击登录按钮失败: {}", e);
        return Err(e);
    }

    info!("登录按钮点击成功，准备输入账号密码");
    let email_input = tab
        .wait_for_xpath_with_custom_timeout(
            concat!(
                "//input[@type='email' or @name='loginfmt']",
                "|//input[@id='usernameEntry']",
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

pub(super) fn check_login_status(tab: &Tab) -> Result<bool> {
    tab.navigate_to(BING_URL)?;
    tab.wait_until_navigated()?;
    tab.reload(true, None)?;
    sleep(Duration::from_secs(2));

    match tab.wait_for_element_with_custom_timeout("#id_s", Duration::from_secs(25)) {
        Ok(ele) => {
            let status = ele.get_attribute_value("aria-hidden")?;
            match status.as_deref() {
                Some("true") => Ok(true),
                Some("false") => Ok(false),
                None => Err(anyhow!("没有找到")),
                _ => Err(anyhow!("未知状态")),
            }
        }
        Err(_) => {
            anyhow::bail!("没有找到登录状态元素");
        }
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
    tab.navigate_to("https://rewards.bing.com/earn")?;
    let _ = tab.wait_until_navigated();
    let ele = tab.wait_for_element("#shell > div.grow > div > main > div")?;
    let button = ele.wait_for_element("button")?;
    button.click()?;
    sleep(Duration::from_secs(3));
    let ele =
        tab.wait_for_xpath("/html/body/div[3]/div/section/div/div[2]/div/div[1]/div[2]/div[4]")?;
    let text = ele.get_inner_text()?;

    let (cur_points, max_points) = parse_point(&text)?;
    Ok((cur_points, max_points))
}

fn parse_point(text: &str) -> Result<(u32, u32)> {
    let re = regex::Regex::new(r"(?P<cur>\d+)\D*/\D*(?P<tot>\d+)").unwrap();
    if let Some(caps) = re.captures(text)
        && let Some(cur) = caps.name("cur")
        && let Some(tot) = caps.name("tot")
    {
        let cur: u32 = cur.as_str().parse()?;
        let tot: u32 = tot.as_str().parse()?;
        return Ok((cur, tot));
    }
    Err(anyhow!("无法解析积分"))
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse_point() {
        let text = "15 个积分/共 60 个(金牌)";
        let (cur, max) = parse_point(text).unwrap();
        assert_eq!(cur, 15);
        assert_eq!(max, 60);
        let text = "15/60";
        let (cur, max) = parse_point(text).unwrap();
        assert_eq!(cur, 15);
        assert_eq!(max, 60);
    }
}
