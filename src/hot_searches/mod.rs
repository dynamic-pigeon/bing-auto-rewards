use std::sync::{LazyLock, RwLock};

use linkme::distributed_slice;
use rand::seq::IndexedRandom;

mod common;

static HOT_WORDS: LazyLock<RwLock<Vec<String>>> = LazyLock::new(|| RwLock::new(Vec::new()));

#[distributed_slice]
pub static HOT_WORDS_PROVIDERS: [fn() -> anyhow::Result<Vec<String>>];

pub(crate) fn get_hot_words(count: usize) -> Vec<String> {
    let hot_words = HOT_WORDS.read().unwrap();
    hot_words
        .choose_multiple(&mut rand::rng(), count)
        .cloned()
        .collect()
}

pub(crate) fn fetch_hot_words() -> anyhow::Result<()> {
    let mut all_hot_words = Vec::new();
    for provider in HOT_WORDS_PROVIDERS {
        match provider() {
            Ok(mut words) => all_hot_words.append(&mut words),
            Err(e) => log::error!("获取热词失败: {}", e),
        }
    }
    all_hot_words.sort();
    all_hot_words.dedup();
    let mut hot_words_lock = HOT_WORDS.write().unwrap();
    *hot_words_lock = all_hot_words;
    Ok(())
}

mod test {
    #[test]
    fn test_fetch_hot_words() {
        super::fetch_hot_words().unwrap();
        let hot_words = super::get_hot_words(10);
        assert!(!hot_words.is_empty());
        for word in hot_words {
            println!("{}", word);
        }
    }
}
