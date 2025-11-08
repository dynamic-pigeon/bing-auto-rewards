use crate::bing::process;

mod bing;

fn main() {
    let _ = log4rs::init_file("log4rs.yaml", Default::default());
    process("config.json").unwrap();
}
