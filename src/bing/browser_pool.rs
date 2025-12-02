use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
    thread::sleep,
    time::Duration,
};

use headless_chrome::Browser;
use log::{debug, error};
use parking_lot::{Condvar, Mutex};

/// 需要保证 temp_dir 的生命周期长于 browser
#[derive(Default)]
pub(crate) struct BingBot {
    pub(crate) browser: Option<Browser>,
    pub(crate) temp_dir: Option<tempfile::TempDir>,
    pub(crate) store_local: bool,
    pub(crate) account: String,
    pub(crate) browser_path: Option<String>,
    pub(crate) proxy: Option<String>,
}

impl BingBot {
    pub(crate) fn get_browser(&mut self) -> anyhow::Result<&mut Browser> {
        self.browser.as_mut().ok_or(anyhow::anyhow!("浏览器未启动"))
    }
}

impl Drop for BingBot {
    fn drop(&mut self) {
        self.browser.take();
        if let Some(dir) = self.temp_dir.take() {
            sleep(Duration::from_secs(3));
            drop(dir);
        }
    }
}

pub(crate) struct BrowserPool {
    cond: Condvar,
    pool: Mutex<Vec<BingBot>>,
    mutex: Arc<Mutex<()>>,
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
        if let Some(mut bot) = self.bot.take() {
            debug!("归还浏览器实例到浏览器池：{}", bot.account);
            // 先丢弃浏览器实例
            bot.browser.take();
            if let Some(t) = bot.temp_dir.take() {
                sleep(Duration::from_secs(3));
                drop(t);
            }
            {
                let mut pool = self.pool.pool.lock();
                pool.push(bot);
            }

            self.pool.cond.notify_one();
        } else {
            error!("浏览器池中的浏览器实例丢失！");
            self.pool.pool.lock().push(BingBot::default());
            self.pool.cond.notify_one();
        }
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
            mutex: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn get_bot<'a>(&'a self) -> PoolWrapper<'a> {
        let guard = self.mutex.lock_arc();
        std::thread::spawn(move || {
            sleep(Duration::from_secs(10));
            drop(guard);
        });
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
