use std::time::Duration;

use anyhow::Result;
use tracing::debug;

/// 用于给返回 `anyhow::Result<T>` 的闭包添加重试能力，支持链式调用：
pub trait Retryable<T> {
    /// 最简单的重试，固定间隔 2 秒
    fn retry(self, times: usize) -> Result<T>;
    #[allow(dead_code)]
    /// 带线性退避的重试：每次等待 base_delay * attempt（attempt 从 1 开始）
    fn retry_with_backoff(self, times: usize, base_delay: Duration) -> Result<T>;
}

impl<F, T> Retryable<T> for F
where
    F: FnMut() -> Result<T>,
{
    fn retry(mut self, times: usize) -> Result<T> {
        assert!(times > 0, "times must be > 0");
        let mut last_err: Option<anyhow::Error> = None;
        for i in 0..times {
            match self() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    debug!("尝试第 {} 次失败: {}", i + 1, e);
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        Err(last_err.expect("重试逻辑保证 times>0 且至少一次失败后会有错误"))
    }

    fn retry_with_backoff(mut self, times: usize, base_delay: Duration) -> Result<T> {
        assert!(times > 0, "times must be > 0");
        let mut last_err: Option<anyhow::Error> = None;
        for i in 0..times {
            match self() {
                Ok(v) => return Ok(v),
                Err(e) => {
                    debug!("尝试第 {} 次失败: {}", i + 1, e);
                    last_err = Some(e);
                    let attempt = i as u64 + 1;
                    // 线性退避：base_delay * attempt
                    let secs = base_delay.as_secs().saturating_mul(attempt);
                    let nanos = base_delay.subsec_nanos();
                    let sleep_dur = Duration::from_secs(secs)
                        .checked_add(Duration::from_nanos(u64::from(nanos) * attempt))
                        .unwrap_or(base_delay);
                    std::thread::sleep(sleep_dur);
                }
            }
        }
        Err(last_err.expect("重试逻辑保证 times>0 且至少一次失败后会有错误"))
    }
}
