# 一个自动获取 bing 积分的程序

**针对中国区域特调，没有适配其他区域，是否能够使用请自行斟酌**

第一步，从 [release](https://github.com/dynamic-pigeon/bing-auto-rewards/releases/) 页面下载可执行文件，或者从源代码编译

第二步你需要在本地安装一个谷歌浏览器（理论上 chromium 内核的浏览器都行，但是不保证兼容性）

然后，你需要在根目录创建 `config.json`，格式大致如下：

```json
{
    "accounts": [
        {
            "email": "xxxx@qq.com",
            "password": "xxx",
            "proxy": "http://proxy.excample.com:prot"
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
    "mobile": false
}
```

参数解释：

- accounts: 必填，账号列表
  - email：必填，账号邮箱
  - password：必填，密码
  - proxy：可选，这个账号的代理服务器
- max_threads：可选，同时进行积分处理的浏览器数量，默认 1
- store_local：可选，是否把浏览器数据保存到本地，true 则保存到 ./user-data 目录下，默认 false，程序正常退出时会自动删除浏览器数据，保存在 ./tmp 文件夹
- browser_path：可选，浏览器可执行路径，不填由程序自动寻找
- schedule：可选，自动执行配置，[详细参数介绍](https://crates.io/crates/croner)，不填默认执行一次，注意，当这次执行的时候上次执行没有结束，这次执行会被跳过
- mobile：可选，是否执行手机搜索，默认 true

本程序启动的时候同时会拉起一个更新热搜的线程，默认两小时执行一次，更新热搜词

## TODO：

- [ ] 更好的重试机制
- [x] 定时任务
- [x] 本地存储的时候能存储移动端的数据