use std::time::Duration;

use serde_json::Value;

use super::HotWordsFuture;
use crate::user_agent::user_agent;

/// JSON 路径步骤枚举
#[derive(Debug, Clone)]
enum PathStep {
    /// 从对象中获取键值
    Key(String),
    /// 遍历数组中的每个元素
    Each,
    /// 过滤数组元素: (字段名, 期望值)
    Filter(String, String),
    /// 从当前值提取为字符串
    Extract,
}

/// JSON 路径提取器
struct JsonPathExtractor {
    steps: Vec<PathStep>,
}

impl JsonPathExtractor {
    fn new(steps: Vec<PathStep>) -> Self {
        Self { steps }
    }

    fn extract(&self, value: Value) -> Vec<String> {
        self.extract_recursive(value, &self.steps)
    }

    fn extract_recursive(&self, value: Value, remaining_steps: &[PathStep]) -> Vec<String> {
        if remaining_steps.is_empty() {
            return vec![];
        }

        match &remaining_steps[0] {
            PathStep::Key(key) => {
                if let Value::Object(mut obj) = value
                    && let Some(next_value) = obj.remove(key)
                {
                    return self.extract_recursive(next_value, &remaining_steps[1..]);
                }
                vec![]
            }
            PathStep::Each => {
                if let Value::Array(arr) = value {
                    return arr
                        .into_iter()
                        .flat_map(|item| self.extract_recursive(item, &remaining_steps[1..]))
                        .collect();
                }
                vec![]
            }
            PathStep::Filter(field, expected_value) => {
                if let Value::Array(arr) = value {
                    return arr
                        .into_iter()
                        .filter(|item| {
                            if let Value::Object(obj) = item {
                                obj.get(field) == Some(&Value::String(expected_value.clone()))
                            } else {
                                false
                            }
                        })
                        .flat_map(|item| self.extract_recursive(item, &remaining_steps[1..]))
                        .collect();
                }
                vec![]
            }
            PathStep::Extract => match value {
                Value::String(s) => vec![s],
                _ => vec![],
            },
        }
    }
}

/// 宏：定义热搜 API 提取函数
///
/// 语法示例：
/// ```
/// hot_search_api! {
///     name: get_example_hot_words,
///     url: "https://api.example.com/hot",
///     user_agent: true,
///     validate: |resp| resp.get("success") == Some(&Value::Bool(true)),
///     path: [Key("data"), Each, Key("title"), Extract],
///     error_name: "示例热搜"
/// }
/// ```
macro_rules! hot_search_api {
    (
        name: $fn_name:ident,
        url: $url:expr,
        $(user_agent: $use_ua:expr,)?
        $(validate: $validate:expr,)?
        path: [$($step:expr),+ $(,)?],
        error_name: $error_name:expr
    ) => {
        fn $fn_name() -> HotWordsFuture {
            #[allow(unused_mut)]
            Box::pin(async move {
                let mut client_builder = reqwest::ClientBuilder::new()
                    .timeout(Duration::from_secs(15));

                $(
                    if $use_ua {
                        client_builder = client_builder.user_agent(user_agent());
                    }
                )?

                let client = client_builder.build()?;
                let resp = client.get($url).send().await?;
                let resp: Value = resp.json().await?;

                $(
                    let validate_fn: fn(&Value) -> bool = $validate;
                    if !validate_fn(&resp) {
                        anyhow::bail!(concat!($error_name, "接口返回失败"));
                    }
                )?

                let extractor = JsonPathExtractor::new(vec![$($step),+]);
                let hot_words = extractor.extract(resp);

                if hot_words.is_empty() {
                    return Err(anyhow::anyhow!(concat!("未获取到", $error_name, "数据")));
                }
                Ok(hot_words)
            })
        }
    };
}

use PathStep::*;

hot_search_api! {
    name: get_baidu_hot_words,
    url: "https://top.baidu.com/api/board?platform=wise&tab=realtime",
    user_agent: true,
    validate: |resp: &Value| resp.get("success") == Some(&Value::Bool(true)),
    path: [
        Key("data".into()),
        Key("cards".into()),
        Filter("component".into(), "tabTextList".into()),
        Key("content".into()),
        Each,
        Key("content".into()),
        Each,
        Key("word".into()),
        Extract
    ],
    error_name: "百度热搜"
}

hot_search_api! {
    name: get_zhihu_hot_words,
    url: "https://api.zhihu.com/topstory/hot-list?limit=10&reverse_order=0",
    path: [
        Key("data".into()),
        Each,
        Key("target".into()),
        Key("title".into()),
        Extract
    ],
    error_name: "知乎热搜"
}

hot_search_api! {
    name: get_toutiao_hot_words,
    url: "https://www.toutiao.com/hot-event/hot-board/?origin=toutiao_pc",
    path: [
        Key("data".into()),
        Each,
        Key("Title".into()),
        Extract
    ],
    error_name: "头条热搜"
}

hot_search_api! {
    name: get_baidu_tiba_hot_words,
    url: "https://tieba.baidu.com/hottopic/browse/topicList",
    path: [
        Key("data".into()),
        Key("bang_topic".into()),
        Key("topic_list".into()),
        Each,
        Key("topic_name".into()),
        Extract
    ],
    error_name: "百度贴吧热搜"
}

hot_search_api! {
    name: get_blibli_tiba_hot_words,
    url: "https://api.bilibili.com/x/web-interface/ranking/v2?rid=0&type=all",
    path: [
        Key("data".into()),
        Key("list".into()),
        Each,
        Key("title".into()),
        Extract
    ],
    error_name: "哔哩哔哩热搜"
}

hot_search_api! {
    name: get_aiqiyi_hot_words,
    url: "https://mesh.if.iqiyi.com/portal/pcw/rankList/comSecRankList?category_id=-1",
    user_agent: true,
    path: [
        Key("data".into()),
        Key("items".into()),
        Each,
        Key("contents".into()),
        Each,
        Key("title".into()),
        Extract
    ],
    error_name: "爱奇艺热搜"
}

hot_search_api! {
    name: get_163_hot_words,
    url: "https://gw.m.163.com/nc-main/api/v1/hqc/no-repeat-hot-list?source=hotTag",
    user_agent: true,
    path: [
        Key("data".into()),
        Key("items".into()),
        Each,
        Key("title".into()),
        Extract
    ],
    error_name: "网易热搜"
}

pub(super) fn providers() -> [fn() -> HotWordsFuture; 7] {
    [
        get_baidu_hot_words,
        get_zhihu_hot_words,
        get_toutiao_hot_words,
        get_baidu_tiba_hot_words,
        get_blibli_tiba_hot_words,
        get_aiqiyi_hot_words,
        get_163_hot_words,
    ]
}
