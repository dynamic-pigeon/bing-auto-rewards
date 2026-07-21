use std::sync::OnceLock;

use tracing::info;

/// 默认 User-Agent，未在 config.json 中配置 `user_agent` 时使用
pub(crate) const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36";

static USER_AGENT: OnceLock<String> = OnceLock::new();

/// 设置全局 User-Agent（在启动时调用一次；重复调用以首次生效）
pub(crate) fn set_user_agent(user_agent: &str) {
    if USER_AGENT.set(user_agent.to_string()).is_ok() {
        info!("使用自定义 User-Agent: {}", user_agent);
    }
}

/// 获取当前生效的 User-Agent
pub(crate) fn user_agent() -> &'static str {
    USER_AGENT
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_USER_AGENT)
}
