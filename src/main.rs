use log::error;

use crate::bing::process;

mod bing;
mod hot_searches;
mod random;

fn main() {
    let _ = log4rs::init_file("log4rs.yaml", Default::default())
        .inspect_err(|_| println!("初始化日志配置文件失败"));
    let _ = process("config.json").inspect_err(|e| error!("{}", e));
}
