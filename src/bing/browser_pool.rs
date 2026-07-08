use std::{
    ops::{Deref, DerefMut},
};

use anyhow::anyhow;
use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use tokio::task::JoinHandle;

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
        if let (Some(mut browser), Some(task)) =
            (self.browser.take(), self.handler_task.take())
        {
            let _ = browser.close().await;
            task.abort();
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
            if let (Some(mut browser), Some(task)) = (browser, task) {
                let _ = browser.close().await;
                task.abort();
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
