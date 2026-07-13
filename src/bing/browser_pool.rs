use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};

use anyhow::anyhow;
use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use tokio::task::JoinHandle;
use tracing::warn;

async fn shutdown_browser(mut browser: Browser, task: JoinHandle<()>) {
    let closed = browser.close().await.is_ok();
    let exited = if closed {
        matches!(
            tokio::time::timeout(Duration::from_secs(5), browser.wait()).await,
            Ok(Ok(_))
        )
    } else {
        false
    };

    if !exited && let Some(Err(e)) = browser.kill().await {
        warn!("强制关闭浏览器失败：{}", e);
    }

    task.abort();
    let _ = task.await;
}

/// 需要保证 temp_dir 的生命周期长于 browser
#[derive(Default)]
pub(crate) struct BingBot {
    pub(crate) browser: Option<Browser>,
    pub(crate) page: Option<Page>,
    pub(crate) temp_dir: Option<tempfile::TempDir>,
    pub(crate) store_local: bool,
    pub(crate) account: String,
    pub(crate) browser_path: Option<String>,
    pub(crate) proxy: Option<String>,
    pub(crate) handler_task: Option<JoinHandle<()>>,
}

impl BingBot {
    pub(crate) fn get_browser(&self) -> anyhow::Result<&Browser> {
        self.browser.as_ref().ok_or(anyhow!("浏览器未启动"))
    }

    pub(crate) fn get_page(&self) -> anyhow::Result<&Page> {
        self.page.as_ref().ok_or(anyhow!("页面未打开"))
    }

    pub(crate) async fn close_browser(&mut self) {
        if let (Some(browser), Some(task)) = (self.browser.take(), self.handler_task.take()) {
            shutdown_browser(browser, task).await;
        }
        self.page.take();
    }
}

impl Drop for BingBot {
    fn drop(&mut self) {
        let browser = self.browser.take();
        let task = self.handler_task.take();
        let temp_dir = self.temp_dir.take();
        self.page.take();

        // 在 Drop 中无法 await，尽量异步关闭浏览器并释放资源，避免阻塞 tokio worker。
        let _handle = tokio::spawn(async move {
            if let (Some(browser), Some(task)) = (browser, task) {
                shutdown_browser(browser, task).await;
            }
            // 浏览器关闭后再释放临时 profile 目录，避免文件仍被占用。
            drop(temp_dir);
        });
    }
}

pub(crate) struct BrowserPool {
    semaphore: tokio::sync::Semaphore,
}

pub(crate) struct PoolWrapper<'a> {
    _permit: tokio::sync::SemaphorePermit<'a>,
    bot: BingBot,
}

impl<'a> Deref for PoolWrapper<'a> {
    type Target = BingBot;
    fn deref(&self) -> &Self::Target {
        &self.bot
    }
}

impl<'a> DerefMut for PoolWrapper<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bot
    }
}

impl BrowserPool {
    pub(crate) fn new(cnt: usize) -> Self {
        Self {
            semaphore: tokio::sync::Semaphore::new(cnt),
        }
    }

    pub(crate) async fn get_bot<'a>(&'a self) -> PoolWrapper<'a> {
        let permit = self
            .semaphore
            .acquire()
            .await
            .expect("浏览器池信号量已关闭");
        PoolWrapper {
            _permit: permit,
            bot: BingBot::default(),
        }
    }
}
