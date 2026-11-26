# 部署

## 二进制部署

从 [Releases](https://github.com/indes/flowerss-bot/releases) 页面下载对应的版本解压运行即可。

## Docker 部署

GitHub Actions 将 `linux/amd64` 和 `linux/arm64` 镜像发布到 GitHub Container Registry。
推送到 `master` 会更新 `:latest`，推送 `v*` 标签会发布同名镜像标签（例如 `:v1.2.3`）。
如果镜像设为私有，请先登录 `ghcr.io`。

1.下载配置文件
在项目目录下新建 `config.yml` 文件


```bash
mkdir ~/flowerss &&\
wget -O ~/flowerss/config.yml \
    https://raw.githubusercontent.com/SlopCommunistCooperative/flowerss-bot/master/config.yml.sample
```


2.修改配置文件

```bash
vim ~/flowerss/config.yml
```

修改配置文件中sqlite路径（如果使用sqlite作为数据库）：
```yaml
sqlite:
  path: /root/.flowerss/data.db
```

3.运行

```shell script
docker run -d -v "$HOME/flowerss:/root/.flowerss" ghcr.io/slopcommunistcooperative/flowerss-bot:latest
```

## 源码编译部署

安装 Rust、Cargo 和 C 工具链（用于编译内置 SQLite 和 TLS 依赖）后运行：

```shell script
git clone https://github.com/indes/flowerss-bot && cd flowerss-bot
make build
./flowerss-bot
```

直接运行 `cargo run --locked` 也可启动。`-d` 指定工作目录，`-c` 指定配置文件，
`-v` 打印版本。

## 从 Go 版本升级

先停止旧进程，再用 Rust 版启动。继续使用原来的 `config.yml` 和数据库；
SQLite 和 MySQL 的订阅 ID、设置与文章去重记录无需导入或转换。
SQLite 和 MySQL 均由 SQLx 访问；SQLite 引擎仍内置，无需安装外部 SQLite 库。
旧版 RSS 推送配置 `telegraph_*`、`message_tpl`、`message_mode` 和 `disable_web_page_preview` 不再使用，可以从配置文件中删除。


## 配置

根据以下模板，新建 `config.yml` 文件。

```yml
bot_token: XXX
user_agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/51.0.2704.103 Safari/537.36
preview_text: 300
socks5: 127.0.0.1:1080
update_interval: 10
error_threshold: 100
telegram:
  endpoint: https://xxx.com/
mysql:
  host: 127.0.0.1
  port: 3306
  user: user
  password: pwd
  database: flowerss
sqlite:
  path: ./data.db
allowed_users:
  - 123
  - 234
```

配置说明：

| 配置项                     | 含义                                      | 是否必填                                       |
| --------------------------| ----------------------------------------- | ------------------------------------------ |
| bot_token                 | Telegram Bot Token                        | 必填                                       |
| preview_text              | 富消息的摘录字数和折叠阈值                  | 可忽略（默认 300，最多 1000）                |
| user_agent                | User Agent                                |可忽略                                     |
| update_interval           | RSS 源扫描间隔（分钟）                      | 可忽略（默认 10）                          |
| error_threshold           | 源最大出错次数                              |可忽略（默认 100）                          |
| socks5                    | 用于无法正常 Telegram API 的环境            | 可忽略（能正常连接上 Telegram API 服务器） |
| mysql                     | MySQL 数据库配置                           | 可忽略（使用 SQLite ）                     |
| sqlite                    | SQLite 配置                               | 可忽略（已配置mysql时，该项失效）          |
| telegram.endpoint         | 自定义 Telegram Bot API URL；需支持 Bot API 10.1 或更高 | 可忽略（使用默认 API URL） |
| allowed_users             | 允许使用bot的用户telegram id，                        | 可忽略，为空时所有用户都能使用bot          |