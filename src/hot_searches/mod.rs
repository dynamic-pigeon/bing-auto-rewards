use std::{
    future::Future,
    pin::Pin,
    sync::{LazyLock, RwLock},
};

use rand::seq::IndexedRandom;
use tokio::task::JoinSet;
use tracing::info;

mod common;

static HOT_WORDS: LazyLock<RwLock<Vec<String>>> = LazyLock::new(|| RwLock::new(Vec::new()));

pub(crate) type HotWordsFuture = Pin<Box<dyn Future<Output = anyhow::Result<Vec<String>>> + Send>>;

pub(crate) fn get_hot_words(count: usize) -> Vec<String> {
    let hot_words = HOT_WORDS.read().unwrap();
    hot_words.sample(&mut rand::rng(), count).cloned().collect()
}

pub(crate) async fn fetch_hot_words() -> anyhow::Result<()> {
    let mut all_hot_words = Vec::new();
    let mut join_set = JoinSet::new();

    for provider in common::providers() {
        join_set.spawn(async move { provider().await });
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok(words)) => all_hot_words.extend(words),
            Ok(Err(e)) => tracing::error!("获取热词失败: {}", e),
            Err(e) => tracing::error!("获取热词任务异常: {}", e),
        }
    }

    all_hot_words.sort_unstable();
    all_hot_words.dedup();
    if all_hot_words.is_empty() {
        anyhow::bail!("未获取到任何热搜词");
    }

    let count = all_hot_words.len();
    *HOT_WORDS.write().unwrap() = all_hot_words;
    info!("获取热搜词共 {count} 条");
    Ok(())
}
