use crate::bing::process;

mod bing;

fn main() {
    log4rs::init_file("log4rs.yaml", Default::default()).unwrap();
    process("accounts.json").unwrap();
}
