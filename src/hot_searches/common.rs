use std::time::Duration;

use super::HOT_WORDS_PROVIDERS;
use anyhow::Result;
use linkme::distributed_slice;
use serde_json::Value;

static USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";

#[distributed_slice(HOT_WORDS_PROVIDERS)]
fn get_baidu_hot_words() -> Result<Vec<String>> {
    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;

    let resp = client
        .get("https://top.baidu.com/api/board?platform=wise&tab=realtime")
        .send()?;

    let resp: Value = resp.json()?;

    if resp.get("success") != Some(&Value::Bool(true)) {
        anyhow::bail!("百度热搜接口返回失败");
    }

    if let Value::Object(mut resp) = resp
        && let Some(Value::Object(mut data)) = resp.remove("data")
        && let Some(Value::Array(cards)) = data.remove("cards")
    {
        let hot_words = cards
            .into_iter()
            .filter_map(|card| {
                if let Value::Object(mut card) = card {
                    if card.get("component") != Some(&Value::String("hotList".to_string())) {
                        return None;
                    }
                    let Some(Value::Array(content)) = card.remove("content") else {
                        return None;
                    };

                    let hot_words = content
                        .into_iter()
                        .filter_map(|c| {
                            if let Value::Object(mut c) = c
                                && let Some(Value::String(query)) = c.remove("query")
                            {
                                Some(query)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();

                    Some(hot_words)
                } else {
                    None
                }
            })
            .flatten()
            .collect::<Vec<_>>();
        if hot_words.is_empty() {
            return Err(anyhow::anyhow!("未获取到百度热搜数据"));
        }
        Ok(hot_words)
    } else {
        Err(anyhow::anyhow!("百度热搜接口返回数据格式异常"))
    }
}

#[distributed_slice(HOT_WORDS_PROVIDERS)]
fn get_zhihu_hot_words() -> Result<Vec<String>> {
    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;

    let resp = client
        .get("https://api.zhihu.com/topstory/hot-list?limit=10&reverse_order=0")
        .send()?;

    let resp: Value = resp.json()?;

    if let Value::Object(mut resp) = resp
        && let Some(Value::Array(data)) = resp.remove("data")
    {
        let hot_words = data
            .into_iter()
            .filter_map(|item| {
                if let Value::Object(mut item) = item
                    && let Some(Value::String(title)) = item.remove("target").and_then(|t| {
                        if let Value::Object(mut t) = t {
                            t.remove("title")
                        } else {
                            None
                        }
                    })
                {
                    Some(title)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if hot_words.is_empty() {
            return Err(anyhow::anyhow!("未获取到知乎热搜数据"));
        }
        Ok(hot_words)
    } else {
        Err(anyhow::anyhow!("知乎热搜接口返回数据格式异常"))
    }
}

#[distributed_slice(HOT_WORDS_PROVIDERS)]
fn get_toutiao_hot_words() -> Result<Vec<String>> {
    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;

    let resp = client
        .get("https://www.toutiao.com/hot-event/hot-board/?origin=toutiao_pc")
        .send()?;

    let resp: Value = resp.json()?;

    if let Value::Object(mut resp) = resp
        && let Some(Value::Array(data)) = resp.remove("data")
    {
        let hot_words = data
            .into_iter()
            .filter_map(|item| {
                if let Value::Object(mut item) = item
                    && let Some(Value::String(title)) = item.remove("Title")
                {
                    Some(title)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if hot_words.is_empty() {
            return Err(anyhow::anyhow!("未获取到头条热搜数据"));
        }
        Ok(hot_words)
    } else {
        Err(anyhow::anyhow!("头条热搜接口返回数据格式异常"))
    }
}

#[distributed_slice(HOT_WORDS_PROVIDERS)]
fn get_baidu_tiba_hot_words() -> Result<Vec<String>> {
    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;

    let resp = client
        .get("https://tieba.baidu.com/hottopic/browse/topicList")
        .send()?;

    let resp: Value = resp.json()?;

    if let Value::Object(mut resp) = resp
        && let Some(Value::Object(mut data)) = resp.remove("data")
        && let Some(Value::Object(mut bang_topic)) = data.remove("bang_topic")
        && let Some(Value::Array(topic_list)) = bang_topic.remove("topic_list")
    {
        let hot_words = topic_list
            .into_iter()
            .filter_map(|item| {
                if let Value::Object(mut item) = item
                    && let Some(Value::String(title)) = item.remove("topic_name")
                {
                    Some(title)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if hot_words.is_empty() {
            return Err(anyhow::anyhow!("未获取到百度贴吧热搜数据"));
        }
        Ok(hot_words)
    } else {
        Err(anyhow::anyhow!("百度贴吧热搜接口返回数据格式异常"))
    }
}

#[distributed_slice(HOT_WORDS_PROVIDERS)]
fn get_blibli_tiba_hot_words() -> Result<Vec<String>> {
    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;
    let resp = client
        .get("https://api.bilibili.com/x/web-interface/ranking/v2?rid=0&type=all")
        .send()?;

    let resp: Value = resp.json()?;

    if let Value::Object(mut resp) = resp
        && let Some(Value::Object(mut data)) = resp.remove("data")
        && let Some(Value::Array(list)) = data.remove("list")
    {
        let hot_words = list
            .into_iter()
            .filter_map(|item| {
                if let Value::Object(mut item) = item
                    && let Some(Value::String(title)) = item.remove("title")
                {
                    Some(title)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if hot_words.is_empty() {
            return Err(anyhow::anyhow!("未获取到哔哩哔哩热搜数据"));
        }
        Ok(hot_words)
    } else {
        Err(anyhow::anyhow!("哔哩哔哩热搜接口返回数据格式异常"))
    }
}

#[distributed_slice(HOT_WORDS_PROVIDERS)]
fn get_aiqiyi_hot_words() -> Result<Vec<String>> {
    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .build()?;
    let resp = client
        .get("https://mesh.if.iqiyi.com/portal/pcw/rankList/comSecRankList?category_id=-1")
        .send()?;

    let resp: Value = resp.json()?;

    if let Value::Object(mut resp) = resp
        && let Some(Value::Object(mut data)) = resp.remove("data")
        && let Some(Value::Array(list)) = data.remove("items")
    {
        let hot_words = list
            .into_iter()
            .filter_map(|item| {
                if let Value::Object(mut item) = item
                    && let Some(Value::Array(contents)) = item.remove("contents")
                {
                    let hot_words = contents
                        .into_iter()
                        .filter_map(|c| {
                            if let Value::Object(mut c) = c
                                && let Some(Value::String(title)) = c.remove("title")
                            {
                                Some(title)
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    Some(hot_words)
                } else {
                    None
                }
            })
            .flatten()
            .collect::<Vec<_>>();
        if hot_words.is_empty() {
            return Err(anyhow::anyhow!("未获取到爱奇艺热搜数据"));
        }
        Ok(hot_words)
    } else {
        Err(anyhow::anyhow!("爱奇艺热搜接口返回数据格式异常"))
    }
}

#[distributed_slice(HOT_WORDS_PROVIDERS)]
fn get_163_hot_words() -> Result<Vec<String>> {
    let client = reqwest::blocking::ClientBuilder::new()
        .timeout(Duration::from_secs(5))
        .user_agent(USER_AGENT)
        .build()?;
    let resp = client
        .get("https://gw.m.163.com/nc-main/api/v1/hqc/no-repeat-hot-list?source=hotTag")
        .send()?;

    let resp: Value = resp.json()?;

    if let Value::Object(mut resp) = resp
        && let Some(Value::Object(mut data)) = resp.remove("data")
        && let Some(Value::Array(items)) = data.remove("items")
    {
        let hot_words = items
            .into_iter()
            .filter_map(|item| {
                if let Value::Object(mut item) = item
                    && let Some(Value::String(title)) = item.remove("title")
                {
                    Some(title)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        if hot_words.is_empty() {
            return Err(anyhow::anyhow!("未获取到网易热搜数据"));
        }
        Ok(hot_words)
    } else {
        Err(anyhow::anyhow!("网易热搜接口返回数据格式异常"))
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test_163_hot_words() {
        let hot_words = super::get_163_hot_words().unwrap();
        assert!(!hot_words.is_empty());
        println!("网易热搜词数量: {}", hot_words.len());
        for word in hot_words {
            println!("{}", word);
        }
    }
    #[test]
    fn test_aiqiyi_hot_words() {
        let hot_words = super::get_aiqiyi_hot_words().unwrap();
        assert!(!hot_words.is_empty());
        println!("爱奇艺热搜词数量: {}", hot_words.len());
        for word in hot_words {
            println!("{}", word);
        }
    }

    #[test]
    fn test_blili_tiba_hot_words() {
        let hot_words = super::get_blibli_tiba_hot_words().unwrap();
        assert!(!hot_words.is_empty());
        println!("哔哩哔哩热搜词数量: {}", hot_words.len());
        for word in hot_words {
            println!("{}", word);
        }
    }
    #[test]
    fn test_baidu_tiba_hot_words() {
        let hot_words = super::get_baidu_tiba_hot_words().unwrap();
        assert!(!hot_words.is_empty());
        println!("百度贴吧热搜词数量: {}", hot_words.len());
        for word in hot_words {
            println!("{}", word);
        }
    }
    #[test]
    fn test_toutiao_hot_words() {
        let hot_words = super::get_toutiao_hot_words().unwrap();
        assert!(!hot_words.is_empty());
        println!("头条热搜词数量: {}", hot_words.len());
        for word in hot_words {
            println!("{}", word);
        }
    }
    #[test]
    fn test_zhihu_hot_words() {
        let hot_words = super::get_zhihu_hot_words().unwrap();
        assert!(!hot_words.is_empty());
        println!("知乎热搜词数量: {}", hot_words.len());
        for word in hot_words {
            println!("{}", word);
        }
    }
    #[test]
    fn test_baidu_hot_words() {
        let hot_words = super::get_baidu_hot_words().unwrap();
        assert!(!hot_words.is_empty());
        println!("百度热搜词数量: {}", hot_words.len());
        for word in hot_words {
            println!("{}", word);
        }
    }
}
