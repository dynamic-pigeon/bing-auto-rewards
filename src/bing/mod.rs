use std::{
    fs,
    path::Path,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::{Page, ScreenshotParams};
use chromiumoxide_cdp::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chrono::Local;
use tracing::{Instrument, debug, error, info, warn};

use crate::{
    bing::browser_pool::{BingBot, BrowserPool},
    hot_searches,
};

mod browser_pool;
mod pc;

#[cfg(feature = "debug")]
const HEADLESS: bool = false;
#[cfg(not(feature = "debug"))]
const HEADLESS: bool = true;
const BING_URL: &str = "https://cn.bing.com/";
const REWARDS_URL: &str = "https://rewards.bing.com/earn";
const REWARDS_URL_DS: &str = "https://rewards.bing.com/dashboard";
const SLEEP_RANGE: std::ops::Range<u64> = 30..80;
const GAP_RANGE: std::ops::Range<u64> = 400..1000;
const GAP_NUM: u32 = 4;

#[derive(serde::Deserialize, Default)]
struct Config {
    accounts: Vec<Account>,
    #[serde(default = "default_max_threads")]
    max_threads: usize,
    #[serde(default)]
    store_local: bool,
    browser_path: Option<String>,
    schedule: Option<String>,
    #[serde(default)]
    user_data_cleanup_days: Option<u64>,
    /// 自定义 User-Agent，留空则使用内置默认值
    user_agent: Option<String>,
}

fn default_max_threads() -> usize {
    1
}

#[derive(serde::Deserialize, Clone)]
struct Account {
    email: String,
    password: String,
    proxy: Option<String>,
}

const INSTANCE_LOCK_PATH: &str = "bing-auto-reward.lock";

/// 进程退出后操作系统会自动释放文件锁；锁文件本身故意不删，
/// 避免 unlink 后另一个进程创建同名新文件、两边各自加锁。
#[derive(Debug)]
struct InstanceLock {
    _file: fs::File,
}

fn acquire_instance_lock() -> Result<InstanceLock> {
    acquire_instance_lock_at(Path::new(INSTANCE_LOCK_PATH))
}

fn acquire_instance_lock_at(path: &Path) -> Result<InstanceLock> {
    use std::io::{Seek, SeekFrom, Write};

    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| anyhow!("打开实例锁 {} 失败：{}", path.display(), e))?;

    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            write!(file, "{}", std::process::id())?;
            file.flush()?;
            Ok(InstanceLock { _file: file })
        }
        Err(fs::TryLockError::WouldBlock) => Err(already_running_error(path)),
        Err(fs::TryLockError::Error(e)) => {
            Err(anyhow!("获取实例锁 {} 失败：{}", path.display(), e))
        }
    }
}

fn already_running_error(path: &Path) -> anyhow::Error {
    if let Some(pid) = fs::read_to_string(path)
        .ok()
        .and_then(|content| content.trim().parse::<u32>().ok())
    {
        anyhow!("已有实例在运行（pid={pid}），请先结束该进程后再启动：kill {pid}")
    } else {
        anyhow!(
            "已有实例在运行（锁文件 {} 被占用），请先结束旧进程后再启动",
            path.display()
        )
    }
}

pub(crate) async fn process<P: AsRef<Path>>(config_file: P) -> Result<()> {
    let _instance_lock = acquire_instance_lock()?;
    let config_file = std::fs::File::open(config_file)?;

    let config: Arc<Config> = Arc::new(serde_json::from_reader(config_file)?);

    if let Some(user_agent) = config.user_agent.as_deref() {
        crate::user_agent::set_user_agent(user_agent);
    }

    let pool = Arc::new(BrowserPool::new(config.max_threads));

    let run_with_cleanup = |config: Arc<Config>, pool: Arc<BrowserPool>| async move {
        if let Some(days) = config.user_data_cleanup_days {
            cleanup_stale_user_data(days)
                .inspect_err(|e| warn!("清理 user-data 失败：{}", e))
                .ok();
        }
        process_once(config, pool).await
    };

    if let Some(schedule) = config.schedule.as_deref() {
        let schedule = croner::Cron::from_str(schedule)
            .inspect_err(|e| error!("定时任务格式串解析有误：{}", e))?;

        info!("第一次执行无视定时任务");
        if let Err(e) = run_with_cleanup(Arc::clone(&config), Arc::clone(&pool)).await {
            error!("定时任务执行失败：{}", e);
        }

        // 每次跑完再取“现在之后”的下一次，避免任务耗时超过间隔时立刻补跑过期档期。
        loop {
            let now = Local::now();
            let Some(time) = schedule.iter_after(now).next() else {
                info!("定时任务没有后续执行时间，退出");
                break;
            };
            let duration = time
                .signed_duration_since(now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_secs(1));
            info!(
                "下次任务将在 {} 执行，等待 {} 秒",
                time.format("%Y-%m-%d %H:%M:%S"),
                duration.as_secs()
            );
            tokio::time::sleep(duration).await;
            if let Err(e) = run_with_cleanup(Arc::clone(&config), Arc::clone(&pool)).await {
                error!("定时任务执行失败：{}", e);
            }
        }
    } else {
        run_with_cleanup(config, pool).await?;
    }

    Ok(())
}

async fn process_once(config: Arc<Config>, pool: Arc<BrowserPool>) -> Result<()> {
    // 启动时先获取一次热搜，顺便检测一下网络是否畅通
    hot_searches::fetch_hot_words()
        .await
        .inspect_err(|e| warn!("获取热搜失败: {}", e))?;

    let mut handles = vec![];
    for account in &config.accounts {
        let account = account.clone();
        let config = Arc::clone(&config);
        let pool = Arc::clone(&pool);
        let account_email = account.email.clone();
        let account_span = tracing::info_span!("account", email = %account_email);
        let handle = tokio::spawn(
            async move { process_account(account, config.as_ref(), pool).await }
                .instrument(account_span),
        );
        handles.push((account_email, handle));
    }

    let mut errors = 0;
    for (account_email, handle) in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                errors += 1;
                error!(account = %account_email, "处理账号失败：{:#}", e);
            }
            Err(e) => {
                errors += 1;
                let err = if e.is_panic() {
                    "账号处理任务 panic"
                } else {
                    "账号处理任务被取消"
                };
                error!(account = %account_email, "处理账号的线程发生错误：{}", err);
            }
        }
    }

    if errors > 0 {
        anyhow::bail!("{} 个账号处理线程出现异常", errors);
    }

    Ok(())
}

const USER_DATA_DIR: &str = "./user-data";
const LAST_CLEANUP_MARKER: &str = ".last_cleanup";

fn cleanup_stale_user_data(retention_days: u64) -> Result<()> {
    if retention_days == 0 {
        warn!("user_data_cleanup_days=0，跳过清理");
        return Ok(());
    }

    let root = Path::new(USER_DATA_DIR);
    if !root.exists() {
        return Ok(());
    }

    let ttl = Duration::from_secs(retention_days.saturating_mul(24 * 60 * 60));
    let now = SystemTime::now();

    let last_cleanup = read_last_cleanup_time(root)?;
    let elapsed = now
        .duration_since(last_cleanup)
        .unwrap_or_else(|_| Duration::from_secs(0));

    if elapsed < ttl {
        info!(
            "距离上次清理不足 {} 天，跳过本次清理（已过 {} 秒）",
            retention_days,
            elapsed.as_secs()
        );
        return Ok(());
    }

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        fs::remove_dir_all(&path)?;
        info!("已清理目录 {}（周期清理）", path.display());
    }

    write_last_cleanup_time(root, now)?;
    info!("本次周期清理完成，下次将在至少 {} 天后触发", retention_days);

    Ok(())
}

fn read_last_cleanup_time(root: &Path) -> Result<SystemTime> {
    let marker = root.join(LAST_CLEANUP_MARKER);
    if marker.is_file() {
        let content = fs::read_to_string(&marker)?;
        match content.trim().parse::<u64>() {
            Ok(ts) => return Ok(UNIX_EPOCH + Duration::from_secs(ts)),
            Err(e) => warn!("清理时间戳解析失败（{}），将视为首次清理", e),
        }
    }

    Ok(UNIX_EPOCH)
}

fn write_last_cleanup_time(root: &Path, now: SystemTime) -> Result<()> {
    let ts = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs();
    fs::write(root.join(LAST_CLEANUP_MARKER), ts.to_string())?;
    Ok(())
}

async fn process_account(
    account: Account,
    &Config {
        store_local,
        ref browser_path,
        ..
    }: &Config,
    pool: Arc<BrowserPool>,
) -> Result<()> {
    let mut last_err: Option<anyhow::Error> = None;
    for i in 0..2 {
        let mut bot = pool.get_bot().await;
        match async {
            bot.new_pc_browser(store_local, &account.email, browser_path, &account.proxy)
                .await?;
            info!("开始处理 PC 端账号 {}", account.email);
            pc::process_account(&account.email, &account.password, &mut bot).await?;
            bot.close_browser().await;
            Ok(())
        }
        .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                debug!("账号 {} 第 {} 次处理失败: {}", account.email, i + 1, e);
                last_err = Some(e);
                bot.close_browser().await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    if let Some(e) = last_err {
        return Err(anyhow!("处理账号 {} 失败: {}", account.email, e));
    }
    Err(anyhow!("处理账号 {} 失败，未记录具体原因", account.email))
}

fn default_browser_config(
    args: Vec<String>,
    browser_path: &Option<String>,
    user_dir: Option<std::path::PathBuf>,
    proxy: &Option<String>,
) -> Result<BrowserConfig> {
    let mut config = BrowserConfig::builder();
    if !HEADLESS {
        config = config.with_head();
    }
    config = config
        .launch_timeout(Duration::from_secs(60))
        .request_timeout(Duration::from_secs(60));
    config = config.no_sandbox();
    config = config.window_size(1920, 1080);
    if let Some(path) = browser_path {
        config = config.chrome_executable(std::path::PathBuf::from(path));
    }
    if let Some(dir) = user_dir {
        // Relative --user-data-dir is ignored on Windows; Chrome then hits
        // the desktop profile and exits 21 if that profile is already in use.
        config = config.user_data_dir(resolve_user_data_dir(dir)?);
    }

    let mut chrome_args = vec![
        // headless 模式下禁用 GPU，避免 macOS 显示器重配置（睡眠/插拔）时
        // GPU 进程退出导致整个浏览器进程被带走、WebSocket 断连
        "disable-gpu".to_string(),
        "disable-dev-shm-usage".to_string(),
        "disable-extensions".to_string(),
        "disable-blink-features=AutomationControlled".to_string(),
        "allow-running-insecure-content".to_string(),
        "disable-plugins".to_string(),
        "disable-images".to_string(),
        "disable-web-security".to_string(),
        "mute-audio".to_string(),
        "no-first-run".to_string(),
        "no-default-browser-check".to_string(),
    ];
    chrome_args.extend(args);
    config = config.args(chrome_args);
    if let Some(proxy) = proxy {
        config = config.arg(("proxy-server", proxy.as_str()));
    }

    config
        .build()
        .map_err(|e| anyhow!("构建浏览器启动选项失败：{}", e))
}

fn resolve_user_data_dir(dir: std::path::PathBuf) -> Result<std::path::PathBuf> {
    std::path::absolute(&dir).map_err(|e| anyhow!("解析浏览器用户目录失败：{}", e))
}

#[cfg(test)]
mod tests {
    use super::{acquire_instance_lock_at, resolve_user_data_dir};
    use chrono::Local;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    #[test]
    fn relative_user_data_dir_becomes_absolute() {
        let resolved = resolve_user_data_dir(PathBuf::from("./user-data/pc_test@example.com"))
            .expect("absolutize user-data-dir");
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with(Path::new("user-data").join("pc_test@example.com")));
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "Chrome rejects Windows UNC paths"
        );
    }

    #[test]
    fn absolute_user_data_dir_is_preserved() {
        let abs = std::env::current_dir()
            .unwrap()
            .join("user-data")
            .join("already-abs");
        let resolved = resolve_user_data_dir(abs.clone()).expect("absolutize user-data-dir");
        assert_eq!(resolved, abs);
    }

    #[test]
    fn instance_lock_replaces_stale_pid_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bing-auto-reward.lock");
        fs::write(&path, "99999").unwrap();
        let lock = acquire_instance_lock_at(&path).expect("stale lock should be replaceable");
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written, std::process::id().to_string());
        drop(lock);
        assert!(
            path.exists(),
            "lock file must stay so the next flock is on the same inode"
        );
        let _again = acquire_instance_lock_at(&path).expect("released lock can be reacquired");
    }

    #[test]
    fn instance_lock_rejects_second_holder() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("bing-auto-reward.lock");
        let first = acquire_instance_lock_at(&path).expect("first lock");
        let err = acquire_instance_lock_at(&path).expect_err("second lock should fail");
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("pid={}", std::process::id())) || msg.contains("被占用"),
            "{msg}"
        );
        drop(first);
        acquire_instance_lock_at(&path).expect("lock available after drop");
    }

    #[test]
    fn next_cron_occurrence_is_after_now() {
        let schedule = croner::Cron::from_str("0 9,16 * * *").unwrap();
        let now = Local::now();
        let next = schedule
            .iter_after(now)
            .next()
            .expect("cron has a next time");
        assert!(next > now);
    }
}

async fn get_one_page(browser: &Browser) -> Result<Page> {
    let pages = browser.pages().await?;
    if let Some(page) = pages.into_iter().next() {
        Ok(page)
    } else {
        browser
            .new_page("about:blank")
            .await
            .map_err(|e| anyhow!("创建新页面失败：{}", e))
    }
}

async fn close_tab(before_pages: Vec<Page>, browser: &Browser) -> Result<()> {
    let after_pages = browser.pages().await?;
    for page in after_pages {
        let target_id = page.target_id();
        if !before_pages.iter().any(|p| p.target_id() == target_id) {
            info!("发现新打开的标签页，准备关闭");
            page.close().await?;
            info!("标签页关闭成功");
        }
    }
    Ok(())
}

async fn shot_when_failed(page: &Page, prefix: &str, account: &str) {
    let params = ScreenshotParams::builder()
        .format(CaptureScreenshotFormat::Png)
        .build();
    if let Ok(png) = page.screenshot(params).await {
        std::fs::create_dir_all("failed").ok();
        let file_name = format!("{prefix}_failure_{account}.png");
        if let Err(e) = std::fs::write(Path::new("failed").join(&file_name), &png) {
            warn!("保存失败截图 failed/{file_name} 失败: {e}");
        } else {
            info!("失败截图已保存为 {file_name}");
        }
    }
}
