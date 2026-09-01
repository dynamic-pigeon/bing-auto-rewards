use std::{cell::RefCell, future::Future, pin::Pin};

use rand::seq::IndexedRandom;
use tokio::task::JoinSet;
use tracing::info;

mod common;

thread_local! {
    static HOT_WORDS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub(crate) type HotWordsFuture = Pin<Box<dyn Future<Output = anyhow::Result<Vec<String>>>>>;

pub(crate) fn get_hot_words(count: usize) -> Vec<String> {
    HOT_WORDS.with(|hot_words| {
        hot_words
            .borrow()
            .sample(&mut rand::rng(), count)
            .cloned()
            .collect()
    })
}

pub(crate) async fn fetch_hot_words() -> anyhow::Result<()> {
    let mut all_hot_words = Vec::new();
    let mut join_set = JoinSet::new();

    for provider in common::providers() {
        join_set.spawn_local(async move { provider().await });
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
    HOT_WORDS.with(|hot_words| {
        *hot_words.borrow_mut() = all_hot_words;
    });
    info!("获取热搜词共 {count} 条");
    Ok(())
}
