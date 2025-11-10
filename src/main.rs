use std::{thread::spawn, time::Duration};

use chrono::Local;
use log::{error, info};

use crate::bing::process;

mod bing;
mod hot_searches;

fn main() {
    let _ = log4rs::init_file("log4rs.yaml", Default::default())
        .inspect_err(|_| println!("初始化日志配置文件失败"));
    hot_searches::fetch_hot_words().unwrap_or_else(|e| {
        error!("获取热搜词失败: {}", e);
    });
    spawn(|| {
        for time in croner::parser::CronParser::builder()
            .seconds(croner::parser::Seconds::Disallowed)
            .build()
            .parse("0 */2 * * *")
            .unwrap()
            .iter_after(Local::now())
        {
            let now = Local::now();
            let duration = time.signed_duration_since(now);
            let duration = duration.to_std().unwrap_or_else(|_| Duration::from_secs(0));
            info!(
                "下次热搜词更新任务将在 {} 执行，等待 {} 秒",
                time.format("%Y-%m-%d %H:%M:%S"),
                duration.as_secs()
            );
            std::thread::sleep(duration);
            if let Err(e) = hot_searches::fetch_hot_words() {
                error!("定时热搜词更新任务执行失败：{}", e);
            }
        }
    });
    let _ = process("config.json").inspect_err(|e| error!("{}", e));
}
