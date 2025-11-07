# 一个自动获取 bing 积分的程序

**针对中国区域特调，没有适配其他区域，是否能够使用请自行斟酌**

第一步，你需要在本地安装一个谷歌浏览器（理论上 chromium 内核的浏览器都行，但是不保证兼容性）

然后，你需要在根目录创建 `config.json`，格式大致如下：

```json
{
    "accounts": [
        {
            "email": "xxxx@qq.com",
            "password": "xxx"
        },
        {
            "email": "xxx@qq.com",
            "password": "xxxx"
        }
    ],
    // 这是可选的，默认是一个线程
    "max_threads": 2,
    // 是否把浏览器数据存储在本地，false 的话会存储在 tmp 文件夹，在程序正常退出时会自动清空
    "store_local": false,
    // 浏览器可执行路径，可选，不填的时候由程序自动寻找
    "browser_path": "google-chrome-stable",
    // cron 的定时任务格式（不含秒），see <https://crates.io/crates/croner>，不填就默认只执行一次
    "sechedule": "0 9 * * *"
}
```

然后，你再装一个 rust，你就可以启动了！

```rust
// 编译还挺占用资源的，请不要在低性能设备编译
cargo run --release
```

## TODO：

- [ ] 不同线程日志分文件存储
- [ ] 更好的重试机制
- [x] 定时任务
- [ ] 本地存储的时候能存储移动端的数据