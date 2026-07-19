# Bing Auto Rewards

一个自动完成 Bing 搜索任务以获取 Microsoft Rewards 积分的工具。

> **注意**：本程序针对中国区域特调，未适配其他区域，请根据实际情况自行判断能否使用。

## 功能特性

- 自动登录 Bing 账号
- 自动完成 PC 端搜索任务
- 自动点击 Rewards 每日任务和奖励卡片
- 支持多账号并行处理
- 支持定时任务调度
- 支持本地持久化浏览器数据

## 快速开始

### 1. 获取程序

从 [Releases](https://github.com/dynamic-pigeon/bing-auto-rewards/releases/) 页面下载对应平台的可执行文件，或者从源码编译：

```bash
git clone https://github.com/dynamic-pigeon/bing-auto-rewards.git
cd bing-auto-rewards
cargo build --release
```

编译完成的可执行文件位于 `target/release/bing-auto-reward`。

### 2. 安装浏览器

需要在本地安装一个基于 Chromium 内核的浏览器（例如 Google Chrome）。理论上其他 Chromium 内核浏览器也可使用，但不保证兼容性。

如果浏览器不在系统 PATH 中，可通过配置项 `browser_path` 指定可执行文件路径。

### 3. 创建配置文件

在程序根目录创建 `config.json`，参考格式如下：

```json
{
    "accounts": [
        {
            "email": "xxxx@qq.com",
            "password": "xxx",
            "proxy": "http://proxy.example.com:port"
        },
        {
            "email": "xxx@qq.com",
            "password": "xxxx"
        }
    ],
    "max_threads": 2,
    "store_local": false,
    "browser_path": "google-chrome-stable",
    "schedule": "0 9 * * *",
    "user_data_cleanup_days": 7,
    "user_agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36"
}
```

### 4. 运行

```bash
./bing-auto-reward
```

程序会自动读取当前目录下的 `config.json`。日志由 `tracing` 管理，默认同时输出到控制台和 `./log` 目录下的滚动日志文件（按天滚动，保留最近 7 天）。

## 配置说明

| 参数 | 必填 | 说明 |
|------|------|------|
| `accounts` | 是 | 账号列表，每个账号包含 `email`、`password`，可选 `proxy` |
| `accounts[].email` | 是 | 账号邮箱 |
| `accounts[].password` | 是 | 账号密码 |
| `accounts[].proxy` | 否 | 该账号使用的代理服务器，例如 `http://proxy.example.com:8080` |
| `max_threads` | 否 | 同时处理的浏览器实例数量，默认 `1` |
| `store_local` | 否 | 是否将浏览器用户数据保存到本地。`true` 保存到 `./user-data`；`false` 使用临时目录，退出后自动清理，默认 `false` |
| `browser_path` | 否 | 浏览器可执行文件路径，不填则尝试自动查找 |
| `schedule` | 否 | Cron 表达式，用于定时执行，例如 `0 9 * * *` 表示每天 9 点。不填则只执行一次。详情参见 [croner](https://crates.io/crates/croner) |
| `user_data_cleanup_days` | 否 | 仅在 `store_local=true` 时生效。程序每次执行任务前会清理 `./user-data` 下超过指定天数未使用的账号目录，根据目录中的 `.last_used` 记录判断 |
| `user_agent` | 否 | 自定义 User-Agent，同时作用于浏览器访问和热搜词接口请求。不填则使用内置默认值 |

## 日志级别

可通过环境变量 `RUST_LOG` 调整日志级别，例如：

```bash
RUST_LOG=debug,chromiumoxide=error ./bing-auto-reward
```

默认项目日志级别为 `info`，`chromiumoxide` 为 `error`。设置 `RUST_LOG` 会覆盖默认过滤器；开启项目调试日志时可同时保留 `chromiumoxide=error` 以避免浏览器协议日志刷屏。支持的写法详见 [tracing-subscriber EnvFilter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)。

## 注意事项

- 请妥善保管 `config.json`，避免密码等敏感信息泄露。
- 使用代理时，请确保代理服务器可正常访问 Bing 和 Microsoft 登录页面。
- 定时任务模式下，程序会按 Cron 表达式顺序执行；若某次执行耗时较长，后续执行会顺延等待。
- 若登录或搜索过程中出现异常，程序会自动截图保存到 `failed/` 目录以便排查。
- 如果登录频繁失败（例如卡在登录页、提示异常或被风控拦截），可以尝试在 `config.json` 中通过 `user_agent` 换成其他 UA（建议使用与本地浏览器版本一致的 Chrome UA），往往能绕过部分检测。

## 开发

```bash
# 检查
cargo check

# 格式化
cargo fmt

# 运行测试
cargo test

# 构建 release
cargo build --release
```

## 许可证

[MIT](./LICENSE)
