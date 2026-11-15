# usuyo-fetch/wechat

公众号的获取插件，供 [usuyo](https://github.com/usuyo/usuyo) 调用：按文章链接取回单篇文章。这个插件管公众号文章和微信收藏里的文章链接；微信视频号是另一个插件 `wechat-channels`。

| 功能 | 实现方式（方法 id） | 账号 | 风险层级 | 状态 |
|---|---|---|---|---|
| 按链接取单篇公众号文章 | 文章页（`article`） | 不使用 | `public-web`，默认开 | 已实现（匿名取得频繁时会回验证页，即 `blocked`，节奏设得慢） |
| 列出微信收藏里的文章链接 | iPhone 微信收藏（`favorites-iphone`） | 只在使用者手机的微信内 | `vision-ui`，默认开 | 未实现 |
| | 电脑微信收藏（`favorites-mac`） | 只在使用者电脑的微信内 | `vision-ui`，默认开 | 未实现 |

风险层级用清单里 `access` 的原词；`private-api`、`client-hook` 默认关，其余层级默认开。

## 方法

### `article`

取单篇文章：正文（Markdown）、正文里的图片和封面、页面原样、公众号名和 id、作者、发布时间、原创标记、转载的出处。匿名取得频繁时会出验证页，这种方法回 `blocked`。

### 未实现的方法

`favorites-iphone`、`favorites-mac`：读微信收藏里的文章链接，只交链接，文章内容由 `article` 去取。

文章页的解析移植自 [wechat-article-exporter](https://github.com/wechat-article/wechat-article-exporter)（MIT，Copyright (c) 2024 Jock），元素对应对照 [wechatmp2markdown](https://github.com/fengxxc/wechatmp2markdown)（MIT，Copyright (c) 2021 fengxxc）和 [ParseHub](https://github.com/z-mio/ParseHub)（MIT，Copyright (c) 2024 梓澪）。

## 开发

`Cargo.toml` 里的 `usuyo-plugin`、`usuyo-plugin-kit` 是对 `https://github.com/usuyo/usuyo` 的 git 依赖，钉在固定的 `rev`。

本地开发时用旁边 `usuyo/` 的工作区代替它们：`usuyo-fetch/.cargo/config.toml`（不进任何插件仓库）写了下面的覆盖，cargo 从当前目录往上找这个文件，在插件文件夹里跑 cargo 就生效；从别处跑要加 `--config <这个文件>`。

```toml
[patch."https://github.com/usuyo/usuyo"]
usuyo-plugin = { path = "../usuyo/crates/usuyo-plugin" }
usuyo-plugin-kit = { path = "../usuyo/crates/usuyo-plugin-kit" }
```

`Cargo.lock` 记的是覆盖后的路径形式，所以 CI 不加 `--locked` 构建，cargo 只重新锁这两个 usuyo crate。
