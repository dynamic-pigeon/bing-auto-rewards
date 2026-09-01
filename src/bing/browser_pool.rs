use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::poll_fn,
    ops::{Deref, DerefMut},
    task::{Poll, Waker},
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

        // Drop 里不能 await。spawn_local 的清理任务会在当前 local runtime
        // 下次让出时执行，不要在这里做阻塞等待。
        let _handle = tokio::task::spawn_local(async move {
            if let (Some(browser), Some(task)) = (browser, task) {
                shutdown_browser(browser, task).await;
            }
            // 浏览器关闭后再释放临时 profile 目录，避免文件仍被占用。
            drop(temp_dir);
        });
    }
}

/// 单线程任务间的许可计数；不需要跨线程同步。
struct LocalSemaphore {
    remaining: Cell<usize>,
    waiters: RefCell<VecDeque<Waker>>,
}

struct LocalPermit<'a> {
    sem: &'a LocalSemaphore,
}

impl Drop for LocalPermit<'_> {
    fn drop(&mut self) {
        self.sem.release();
    }
}

impl LocalSemaphore {
    fn new(permits: usize) -> Self {
        Self {
            remaining: Cell::new(permits),
            waiters: RefCell::new(VecDeque::new()),
        }
    }

    async fn acquire(&self) -> LocalPermit<'_> {
        poll_fn(|cx| {
            let remaining = self.remaining.get();
            if remaining > 0 {
                self.remaining.set(remaining - 1);
                Poll::Ready(LocalPermit { sem: self })
            } else {
                self.waiters.borrow_mut().push_back(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    fn release(&self) {
        self.remaining.set(self.remaining.get() + 1);
        if let Some(waker) = self.waiters.borrow_mut().pop_front() {
            waker.wake();
        }
    }
}

pub(crate) struct BrowserPool {
    semaphore: LocalSemaphore,
}

pub(crate) struct PoolWrapper<'a> {
    _permit: LocalPermit<'a>,
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
            semaphore: LocalSemaphore::new(cnt),
        }
    }

    pub(crate) async fn get_bot(&self) -> PoolWrapper<'_> {
        PoolWrapper {
            _permit: self.semaphore.acquire().await,
            bot: BingBot::default(),
        }
    }
}
