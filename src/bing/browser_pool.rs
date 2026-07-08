use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
    thread::sleep,
    time::Duration,
};

use anyhow::anyhow;
use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use parking_lot::{Condvar, Mutex};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tracing::{debug, error};

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
    pub(crate) runtime: Option<Arc<Runtime>>,
    pub(crate) handler_task: Option<JoinHandle<()>>,
}

impl BingBot {
    pub(crate) fn get_browser(&mut self) -> anyhow::Result<&mut Browser> {
        self.browser.as_mut().ok_or(anyhow!("浏览器未启动"))
    }

    pub(crate) fn get_page(&mut self) -> anyhow::Result<&mut Page> {
        self.page.as_mut().ok_or(anyhow!("页面未打开"))
    }

    pub(crate) fn get_runtime(&self) -> anyhow::Result<Arc<Runtime>> {
        self.runtime.clone().ok_or(anyhow!("运行时未初始化"))
    }
}

impl Drop for BingBot {
    fn drop(&mut self) {
        if let (Some(mut browser), Some(runtime), Some(task)) =
            (self.browser.take(), self.runtime.take(), self.handler_task.take())
        {
            let _ = runtime.block_on(async {
                let _ = browser.close().await;
            });
            task.abort();
        }
        self.page.take();
        if let Some(dir) = self.temp_dir.take() {
            sleep(Duration::from_secs(3));
            drop(dir);
        }
    }
}

pub(crate) struct BrowserPool {
    cond: Condvar,
    pool: Mutex<Vec<BingBot>>,
}

pub(crate) struct PoolWrapper<'a> {
    pool: &'a BrowserPool,
    bot: Option<BingBot>,
}

impl<'a> Deref for PoolWrapper<'a> {
    type Target = BingBot;
    fn deref(&self) -> &Self::Target {
        self.bot.as_ref().expect("浏览器实例丢失")
    }
}

impl<'a> DerefMut for PoolWrapper<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.bot.as_mut().expect("浏览器实例丢失")
    }
}

impl<'a> Drop for PoolWrapper<'a> {
    fn drop(&mut self) {
        let Some(mut bot) = self.bot.take() else {
            error!("浏览器池中的浏览器实例丢失！");
            self.pool.cond.notify_one();
            return;
        };

        debug!("归还浏览器实例到浏览器池：{}", bot.account);
        if let (Some(mut browser), Some(runtime), Some(task)) =
            (bot.browser.take(), bot.runtime.take(), bot.handler_task.take())
        {
            let _ = runtime.block_on(async {
                let _ = browser.close().await;
            });
            task.abort();
        }
        bot.page.take();
        if let Some(t) = bot.temp_dir.take() {
            sleep(Duration::from_secs(3));
            drop(t);
        }
        self.pool.pool.lock().push(bot);
        self.pool.cond.notify_one();
    }
}

impl BrowserPool {
    pub(crate) fn new(cnt: usize) -> Self {
        Self {
            cond: Condvar::new(),
            pool: Mutex::new({
                let mut v = Vec::with_capacity(cnt);
                for _ in 0..cnt {
                    v.push(BingBot::default());
                }
                v
            }),
        }
    }

    pub(crate) fn get_bot<'a>(&'a self) -> PoolWrapper<'a> {
        let mut pool = self.pool.lock();
        loop {
            if let Some(bot) = pool.pop() {
                return PoolWrapper {
                    pool: self,
                    bot: Some(bot),
                };
            }
            self.cond.wait(&mut pool);
        }
    }
}
