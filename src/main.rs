use tracing::error;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

use crate::bing::process;

mod bing;
mod hot_searches;
mod random;
mod user_agent;

#[tokio::main(worker_threads = 1)]
async fn main() -> anyhow::Result<()> {
    let _guard = init_tracing();
    process("config.json")
        .await
        .inspect_err(|e| error!("{}", e))
}

fn init_tracing() -> tracing_appender::non_blocking::WorkerGuard {
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("log")
        .filename_suffix("log")
        .max_log_files(7)
        .build("log")
        .expect("初始化文件日志失败");

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info").add_directive(
            "chromiumoxide=error"
                .parse()
                .expect("chromiumoxide 日志过滤级别解析失败"),
        )
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_target(true)
                .with_level(true)
                .with_line_number(true)
                .with_ansi(false)
                .with_writer(non_blocking),
        )
        .with(
            fmt::layer()
                .with_target(true)
                .with_level(true)
                .with_line_number(true),
        )
        .init();

    guard
}
