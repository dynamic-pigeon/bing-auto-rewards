use std::{
    future::Future,
    pin::Pin,
    sync::{LazyLock, RwLock},
};

use linkme::distributed_slice;
use log::info;
use rand::seq::IndexedRandom;
use tokio::task::JoinSet;

mod common;

static HOT_WORDS: LazyLock<RwLock<Vec<String>>> = LazyLock::new(|| RwLock::new(Vec::new()));

#[distributed_slice]
pub static HOT_WORDS_PROVIDERS: [fn() -> HotWordsFuture];

pub(crate) type HotWordsFuture = Pin<Box<dyn Future<Output = anyhow::Result<Vec<String>>> + Send>>;

pub(crate) fn get_hot_words(count: usize) -> Vec<String> {
    let hot_words = HOT_WORDS.read().unwrap();
    hot_words.sample(&mut rand::rng(), count).cloned().collect()
}

pub(crate) async fn fetch_hot_words() -> anyhow::Result<()> {
    let mut all_hot_words = Vec::new();
    let mut join_set = JoinSet::new();

    for provider in HOT_WORDS_PROVIDERS {
        join_set.spawn(async move { provider().await });
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(words)) => all_hot_words.extend(words),
            Ok(Err(e)) => log::error!("获取热词失败: {}", e),
            Err(e) => log::error!("获取热词任务异常: {}", e),
        }
    }

    let all_hot_words = modify_hot_words(all_hot_words);
    let mut hot_words_lock = HOT_WORDS.write().unwrap();
    *hot_words_lock = all_hot_words;
    info!("获取热搜词共 {} 条", hot_words_lock.len());
    Ok(())
}

pub(crate) fn fetch_hot_words_blocking() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(fetch_hot_words())
}

fn modify_hot_words(mut words: Vec<String>) -> Vec<String> {
    words.sort_unstable();
    words.dedup();
    words
}

#[cfg(test)]
mod test {
    #[tokio::test]
    async fn test_fetch_hot_words() {
        super::fetch_hot_words().await.unwrap();
        let hot_words = super::get_hot_words(10);
        assert!(!hot_words.is_empty());
        for word in hot_words {
            println!("{}", word);
        }
    }
}
