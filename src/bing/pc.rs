use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use chromiumoxide::browser::Browser;
use chromiumoxide::element::Element;
use chromiumoxide::page::Page;
use chromiumoxide_cdp::cdp::browser_protocol::page::ReloadParams;
use futures::StreamExt;
use rand::seq::IndexedRandom;
use tracing::{debug, info, warn};

use crate::{
    bing::{
        BING_URL, BingBot, GAP_NUM, GAP_RANGE, REWARDS_URL, REWARDS_URL_DS, SLEEP_RANGE, close_tab,
        default_browser_config, get_one_page, shot_when_failed,
    },
    random::ExpectedNTrigger,
};

static PC_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
const MAX_PC_SEARCH_TIMES: usize = 20;

impl BingBot {
    pub(crate) async fn new_pc_browser(
        &mut self,
        store_local: bool,
        account: &str,
        browser_path: &Option<String>,
        proxy: &Option<String>,
    ) -> Result<()> {
        self.close_browser().await;

        let (temp_dir, user_dir) = if store_local {
            (None, Some(prepare_local_user_data_dir(account)?))
        } else if let Some(dir) = self.temp_dir.take() {
            // 重启时复用同一个临时 profile，保留 cookie
            let path = dir.path().to_path_buf();
            (Some(dir), Some(path))
        } else {
            std::fs::create_dir_all("./tmp")?;
            let dir = tempfile::TempDir::new_in("./tmp")?;
            let path = dir.path().to_path_buf();
            (Some(dir), Some(path))
        };

        let config = build_pc_config(browser_path, proxy, user_dir)?;

        let (browser, mut handler) = Browser::launch(config)
            .await
            .map_err(|e| anyhow!("启动浏览器失败：{}", e))?;

        let handler_task = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        let page = get_one_page(&browser).await?;
        page.enable_stealth_mode_with_agent(PC_USER_AGENT)
            .await
            .map_err(|e| anyhow!("启用反检测模式失败：{}", e))?;

        self.browser = Some(browser);
        self.page = Some(page);
        self.temp_dir = temp_dir;
        self.store_local = store_local;
        self.account = account.to_string();
        self.browser_path = browser_path.clone();
        self.proxy = proxy.clone();
        self.handler_task = Some(handler_task);
        Ok(())
    }

    pub(crate) async fn restart_pc_browser(&mut self) -> Result<()> {
        let store_local = self.store_local;
        let account = self.account.clone();
        let browser_path = self.browser_path.clone();
        let proxy = self.proxy.clone();
        self.close_browser().await;
        self.new_pc_browser(store_local, &account, &browser_path, &proxy)
            .await
    }
}

fn build_pc_config(
    browser_path: &Option<String>,
    proxy: &Option<String>,
    user_dir: Option<PathBuf>,
) -> Result<chromiumoxide::browser::BrowserConfig> {
    default_browser_config(vec![], browser_path, user_dir, proxy)
}

fn prepare_local_user_data_dir(account: &str) -> Result<PathBuf> {
    std::fs::create_dir_all("./user-data")?;
    let user_data_dir = std::path::PathBuf::from(format!("./user-data/pc_{}", account));
    std::fs::create_dir_all(&user_data_dir)?;
    mark_user_data_last_used(&user_data_dir)?;
    Ok(user_data_dir)
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
pub(crate) async fn process_account(
    email: &str,
    password: &str,
    browser_bot: &mut BingBot,
) -> Result<()> {
    info!("开始登录Bing账号: {}", email);
    let page = browser_bot.get_page()?;
    let mut login_last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match async {
            if !check_login_status(page).await? {
                login_bing(email, password, page).await?;
                tokio::time::sleep(Duration::from_secs(5)).await;
                if !check_login_status(page).await? {
                    return Err(anyhow!("登录后检查状态仍然未登录"));
                }
            } else {
                info!("账号 {} 已登录，无需重复登录", email);
            }
            Ok(())
        }
        .await
        {
            Ok(()) => {
                login_last_err = None;
                break;
            }
            Err(e) => {
                debug!("账号 {} 第 {} 次登录失败: {}", email, i + 1, e);
                login_last_err = Some(e);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    if let Some(e) = login_last_err {
        let page = browser_bot.get_page().expect("页面已丢失");
        shot_when_failed(page, "login", email).await;
        return Err(anyhow!("登录失败: {}", e));
    }

    tokio::time::sleep(Duration::from_secs(5)).await;

    info!("开始尝试点击卡片");
    let browser = browser_bot.get_browser()?;
    let page = browser_bot.get_page()?;
    if let Err(e) = click_rewards(browser, page).await {
        warn!("点击奖励卡片失败: {}", e);
        let page = browser_bot.get_page().expect("页面已丢失");
        shot_when_failed(page, "click_rewards", email).await;
    }

    tokio::time::sleep(Duration::from_secs(5)).await;

    info!("开始进行搜索任务");
    if let Err(e) = search(browser_bot, email).await {
        let page = browser_bot.get_page().expect("页面已丢失");
        shot_when_failed(page, "search", email).await;
        return Err(anyhow!("搜索任务失败: {}", e));
    }

    info!("{} 账号处理完成", email);
    Ok(())
}

async fn search(browser_bot: &mut BingBot, email: &str) -> Result<()> {
    let search_words = crate::hot_searches::get_hot_words(MAX_PC_SEARCH_TIMES);

    let mut trigger = ExpectedNTrigger::new(GAP_NUM);
    for (i, word) in search_words.into_iter().enumerate() {
        let page = browser_bot.get_page()?;
        let sleep_time = if trigger.next() {
            match get_pc_search_process(page).await {
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
                    shot_when_failed(page, "rewards_get_failed", email).await;
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
            tokio::time::sleep(Duration::from_secs(sleep_time)).await;
        } else {
            let mut slept = 0;
            while slept < sleep_time {
                let sleep_chunk = std::cmp::min(MAX_SLEEP_TIME, sleep_time - slept);
                tokio::time::sleep(Duration::from_secs(sleep_chunk)).await;
                // 空转防止 timeout
                let _ = page.reload().await;
                slept += sleep_chunk;
            }
        }

        let mut search_last_err: Option<anyhow::Error> = None;
        for _j in 0..3 {
            let page = browser_bot.get_page()?;
            let browser = browser_bot.get_browser()?;
            match perform_search_and_click(browser, page, &word).await {
                Ok(()) => {
                    search_last_err = None;
                    break;
                }
                Err(e) => {
                    warn!("搜索点击失败，尝试重启浏览器: {}", e);
                    search_last_err = Some(e);
                    browser_bot.restart_pc_browser().await?;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
        if let Some(e) = search_last_err {
            return Err(e);
        }
        info!("第 {} 次搜索完成", i + 1);
    }

    Ok(())
}

async fn perform_search_and_click(browser: &Browser, page: &Page, word: &str) -> Result<()> {
    let before_pages = browser.pages().await?;

    let search_url = reqwest::Url::parse_with_params(
        "https://cn.bing.com/search",
        [("q", word), ("PC", "U316"), ("FORM", "CHROMN")],
    )?;
    page.goto(search_url.as_str()).await?;
    page.wait_for_navigation().await?;

    tokio::time::sleep(Duration::from_secs(rand::random_range(1..4))).await;

    wait_for_element(page, "#b_results li.b_algo", Duration::from_secs(25))
        .await
        .map_err(|e| anyhow!("等待搜索结果超时：{e}"))?;

    let all_res = wait_for_elements(page, "#b_results li.b_algo", Duration::from_secs(25)).await?;

    let ele = all_res
        .choose(&mut rand::rng())
        .ok_or(anyhow!("没有找到搜索结果"))?;

    ele.click()
        .await
        .map_err(|e| anyhow!(format!("点击搜索结果失败：{e}")))?;

    tokio::time::sleep(Duration::from_secs(rand::random_range(5..10))).await;

    close_tab(before_pages, browser).await?;

    Ok(())
}

async fn click_rewards(browser: &Browser, page: &Page) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match click_daily_set(browser, page).await {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                debug!("点击每日任务卡片第 {} 次失败: {}", i + 1, e);
                last_err = Some(e);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    if let Some(e) = last_err {
        warn!("点击每日任务卡片失败: {e}");
        return Err(e);
    }

    let mut last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match click_earn(browser, page).await {
            Ok(()) => {
                last_err = None;
                break;
            }
            Err(e) => {
                debug!("点击奖励卡片第 {} 次失败: {}", i + 1, e);
                last_err = Some(e);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    if let Some(e) = last_err {
        warn!("点击奖励卡片失败: {e}");
        return Err(e);
    }

    info!("卡片点击完成");
    Ok(())
}

/// 强制刷新页面（忽略缓存），恢复 headless_chrome 中 `tab.reload(true, None)` 的语义。
async fn reload_hard(page: &Page) -> Result<()> {
    let params = ReloadParams::builder().ignore_cache(true).build();
    page.execute(params).await?;
    page.wait_for_navigation().await?;
    Ok(())
}

/// 轮询等待页面中匹配 CSS 选择器的单个元素出现。
///
/// `chromiumoxide` 的 `find_element` 只执行一次查询；原 `headless_chrome` 的
/// `wait_for_element` 会轮询 DOM 直到超时。本函数恢复原有语义。
async fn wait_for_element(page: &Page, selector: &str, timeout: Duration) -> Result<Element> {
    let start = std::time::Instant::now();
    loop {
        match page.find_element(selector).await {
            Ok(ele) => return Ok(ele),
            _ => {
                if start.elapsed() >= timeout {
                    anyhow::bail!("等待选择器 {} 的元素超时", selector);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// 轮询等待页面中匹配 XPath 的单个元素出现。
async fn wait_for_xpath(page: &Page, xpath: &str, timeout: Duration) -> Result<Element> {
    let start = std::time::Instant::now();
    loop {
        match page.find_xpath(xpath).await {
            Ok(ele) => return Ok(ele),
            _ => {
                if start.elapsed() >= timeout {
                    anyhow::bail!("等待 XPath {} 的元素超时", xpath);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

/// 轮询等待页面中匹配选择器的元素出现（至少一个）。
///
/// 页面动态渲染时，容器可能在 DOMContentLoaded 后就存在，但子元素会延迟填充。
/// chromiumoxide 的 `find_elements` 只执行一次查询，不会主动等待，因此需要手动轮询。
async fn wait_for_elements(page: &Page, selector: &str, timeout: Duration) -> Result<Vec<Element>> {
    let start = std::time::Instant::now();
    loop {
        match page.find_elements(selector).await {
            Ok(elements) if !elements.is_empty() => return Ok(elements),
            _ => {
                if start.elapsed() >= timeout {
                    anyhow::bail!("等待选择器 {} 的元素超时", selector);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn click_earn(browser: &Browser, page: &Page) -> Result<()> {
    page.goto(REWARDS_URL).await?;
    page.wait_for_navigation().await?;

    let cards = wait_for_elements(
        page,
        "#moreactivities > div > div:nth-of-type(2) a",
        Duration::from_secs(25),
    )
    .await?;
    info!("找到 {} 个奖励卡片，准备点击", cards.len());

    for card in cards {
        let text = card.inner_text().await?.unwrap_or_default();
        if !text.contains("+") {
            continue;
        }

        let before_pages = browser.pages().await?;
        match card.call_js_fn("function() { this.click(); }", false).await {
            Ok(_) => info!("通过 JS 点击奖励卡片成功"),
            Err(e) => warn!("通过 JS 点击奖励卡片失败：{}", e),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = close_tab(before_pages, browser).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

async fn click_daily_set(browser: &Browser, page: &Page) -> Result<()> {
    page.goto(REWARDS_URL_DS).await?;
    page.wait_for_navigation().await?;

    let cards = wait_for_elements(
        page,
        "#dailyset > div > div:nth-of-type(2) a",
        Duration::from_secs(25),
    )
    .await?;
    info!("找到 {} 个每日任务卡片，准备点击", cards.len());

    for card in cards {
        let before_pages = browser.pages().await?;
        match card.call_js_fn("function() { this.click(); }", false).await {
            Ok(_) => info!("通过 JS 点击每日任务卡片成功"),
            Err(e) => warn!("通过 JS 点击每日任务卡片失败：{}", e),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = close_tab(before_pages, browser).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

pub(super) async fn login_bing(email: &str, password: &str, page: &Page) -> Result<()> {
    page.activate().await?;
    page.goto(BING_URL).await?;
    page.wait_for_navigation().await?;
    reload_hard(page).await?;

    let mut click_last_err: Option<anyhow::Error> = None;
    for i in 0..3 {
        match async {
            page.reload().await?;
            page.wait_for_navigation().await?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            click_login_button(page).await
        }
        .await
        {
            Ok(()) => {
                click_last_err = None;
                break;
            }
            Err(e) => {
                debug!("点击登录按钮第 {} 次失败: {}", i + 1, e);
                click_last_err = Some(e);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    if let Some(e) = click_last_err {
        let url = page.url().await?.unwrap_or_default();
        debug!("当前页面：{}", url);
        warn!("点击登录按钮失败: {}", e);
        return Err(e);
    }

    info!("登录按钮点击成功，准备输入账号密码");
    let email_input = wait_for_xpath(
        page,
        concat!(
            "//input[@type='email' or @name='loginfmt']",
            "|//input[@id='usernameEntry']",
        ),
        Duration::from_secs(10),
    )
    .await
    .map_err(|e| anyhow!("寻找账号输入位置超时：{e}"))?;

    email_input.type_str(email).await?;

    info!("账号输入成功，准备点击下一步");

    let next_button = wait_for_xpath(
        page,
        concat!(
            "//button[@type='submit']",
            "|//button[text()='下一步']",
        ),
        Duration::from_secs(10),
    )
    .await?;

    next_button.click().await?;

    let password_input = loop {
        match wait_for_xpath(
            page,
            concat!(
                "//input[@type='password' or @name='passwd']",
                "|//input[@id='passwordInput']",
            ),
            Duration::from_secs(5),
        )
        .await
        {
            Ok(input) => break input,
            Err(_) => {
                if let Ok(button) = wait_for_xpath(
                    page,
                    "//*[text()='暂时跳过']",
                    Duration::from_secs(5),
                )
                .await
                {
                    let _ = button.click().await;
                }

                if let Ok(button) = wait_for_xpath(
                    page,
                    concat!(
                        "//span[@role='button' and (text()='其他登录方法' or text()='Other ways to sign in')]",
                        "|//*[text()='其他登录方法']"
                    ),
                    Duration::from_secs(5),
                )
                .await
                {
                    let _ = button.click().await;
                }

                let button = wait_for_xpath(
                    page,
                    concat!(
                        "//*[text()='使用密码']",
                        "|//*[text()='Use your password']",
                        "|//button[contains(text(), '使用密码')]",
                        "|//button[contains(text(), 'Use your password')]",
                        "|//a[contains(text(), '使用密码')]",
                        "|//a[contains(text(), 'Use your password')]",
                    ),
                    Duration::from_secs(5),
                )
                .await
                .map_err(|e| anyhow!("等待使用密码按钮超时：{e}"))?;

                button.click().await?;
            }
        }
    };

    password_input.type_str(password).await?;

    info!("密码输入成功，准备点击登录");

    let sign_in_button = wait_for_xpath(
        page,
        concat!(
            "//button[@type='submit']",
            "|//button[text()='登录']",
            "|//button[text()='Sign in']",
            "|//button[text()='下一步']",
            "|//button[text()='Next']",
        ),
        Duration::from_secs(10),
    )
    .await?;

    sign_in_button.click().await?;

    info!("登录按钮点击成功");

    if wait_for_xpath(
        page,
        "//*[contains(text(), '保持登录状态')]|//*[contains(text(), 'Stay signed in')]",
        Duration::from_secs(5),
    )
    .await
    .is_ok()
    {
        if let Ok(ok_button) = wait_for_xpath(
            page,
            concat!("//button[text()='是']", "|//button[text()='Yes']"),
            Duration::from_secs(3),
        )
        .await
        {
            let _ = ok_button.click().await;
        }
    } else if let Ok(ok_button) = wait_for_xpath(
        page,
        concat!("//button[text()='是']", "|//button[text()='Yes']"),
        Duration::from_secs(3),
    )
    .await
    {
        let _ = ok_button.click().await;
    }

    info!("登录流程完成");
    Ok(())
}

pub(super) async fn check_login_status(page: &Page) -> Result<bool> {
    page.goto(BING_URL).await?;
    page.wait_for_navigation().await?;
    reload_hard(page).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    match wait_for_element(page, "#id_s", Duration::from_secs(25)).await {
        Ok(ele) => {
            let status = ele.attribute("aria-hidden").await?;
            match status.as_deref() {
                Some("true") => Ok(true),
                Some("false") => Ok(false),
                None => Err(anyhow!("没有找到")),
                _ => Err(anyhow!("未知状态")),
            }
        }
        Err(_) => {
            anyhow::bail!("没有找到登录状态元素")
        }
    }
}

async fn click_login_button(page: &Page) -> Result<()> {
    // 新版 Bing 首页可能不显示登录入口，优先在搜索结果页头部寻找
    let login_button = wait_for_xpath(
        page,
        concat!(
            "//span[@id='id_s']",
            "|//*[@id='id_a']",
            "|//a[@id='id_l']",
            "|//a[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//a[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//a[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
            "|//button[span[text()='登录'] or span[text()='Sign in'] or span[text()='登入']]",
            "|//button[contains(@aria-label, '登录') or contains(@aria-label, 'Sign in') or contains(@aria-label, '登入')]",
            "|//button[contains(text(), '登录') or contains(text(), 'Sign in') or contains(text(), '登入')]",
        ),
        Duration::from_secs(10),
    )
    .await;

    let login_button = match login_button {
        Ok(btn) => btn,
        Err(_) => {
            info!("首页未找到登录按钮，尝试从搜索结果页登录");
            page.goto("https://cn.bing.com/search?q=bing").await?;
            page.wait_for_navigation().await?;
            wait_for_xpath(
                page,
                "//header//a[contains(text(), '登录') or contains(text(), 'Sign in')]",
                Duration::from_secs(10),
            )
            .await
            .map_err(|e| anyhow!("等待登录按钮超时：{e}"))?
        }
    };

    info!("找到登录按钮，准备点击");
    tokio::time::sleep(Duration::from_secs(2)).await;
    login_button.click().await?;
    page.wait_for_navigation().await?;
    Ok(())
}

async fn get_pc_search_process(page: &Page) -> Result<(u32, u32)> {
    page.goto("https://rewards.bing.com/earn").await?;
    let _ = page.wait_for_navigation().await;
    tokio::time::sleep(Duration::from_secs(3)).await;

    // 新版 Rewards 页面直接在卡片里展示进度，例如中文 "搜索: 0/1" 或英文 "Search: 0/1"。
    let progress: Option<String> = page
        .evaluate(
            r#"(() => {
                const cards = Array.from(document.querySelectorAll('main *'));
                const card = cards.find(el => {
                    const t = el.textContent;
                    return t.includes('必应搜索连续打卡') ||
                           /PC\s*search|Bing\s*search|search\s*streak/i.test(t);
                });
                if (!card) return null;
                let node = card;
                for (let i = 0; i < 8 && node; i++) {
                    const m = node.textContent.match(/(?:搜索|Search)[:：]?\s*(\d+)\s*\/\s*(\d+)/i);
                    if (m) return `${m[1]}/${m[2]}`;
                    node = node.parentElement;
                }
                return null;
            })()"#,
        )
        .await?
        .into_value()?;

    let text = progress.ok_or(anyhow!("未找到 PC 搜索进度"))?;
    parse_point(&text)
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
