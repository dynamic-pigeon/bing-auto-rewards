use std::cell::RefCell;

use tracing::info;

/// 默认 User-Agent，未在 config.json 中配置 `user_agent` 时使用
pub(crate) const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";

thread_local! {
    static USER_AGENT: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 设置全局 User-Agent（在启动时调用一次；重复调用以首次生效）
pub(crate) fn set_user_agent(user_agent: &str) {
    USER_AGENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            info!("使用自定义 User-Agent: {}", user_agent);
            *slot = Some(user_agent.to_string());
        }
    });
}

/// 获取当前生效的 User-Agent
pub(crate) fn user_agent() -> String {
    USER_AGENT.with(|slot| {
        slot.borrow()
            .clone()
            .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string())
    })
}
