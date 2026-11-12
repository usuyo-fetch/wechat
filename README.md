# usuyo-fetch/wechat-mp

公众号的获取插件，供 [usuyo](https://github.com/usuyo/usuyo) 调用：按文章链接取回单篇文章，微信收藏里的文章链接由手机或电脑上的流程交来。

| 方法 | 清单里的 id | 取得 | 账号 | 层级 | 现状 |
|---|---|---|---|---|---|
| 文章页 | `article` | 单篇文章：正文（Markdown）、正文里的图片和封面、页面原样、公众号名和 id、作者、发布时间、原创标记、转载的出处 | 不使用 | `public-web` | 已实现；匿名取得频繁时会回验证页（`blocked`），节奏设得慢 |
| iPhone 微信收藏 | `favorites-iphone` | 存过的：微信收藏里的文章链接 | 只在使用者手机的微信内 | `vision-ui` | 未实现 |
| 电脑微信收藏 | `favorites-mac` | 存过的：微信收藏里的文章链接 | 只在使用者电脑的微信内 | `vision-ui` | 未实现 |

文章页的解析移植自 [wechat-article-exporter](https://github.com/wechat-article/wechat-article-exporter)（MIT，Copyright (c) 2024 Jock），元素对应对照 [wechatmp2markdown](https://github.com/fengxxc/wechatmp2markdown)（MIT，Copyright (c) 2021 fengxxc）和 [ParseHub](https://github.com/z-mio/ParseHub)（MIT，Copyright (c) 2024 梓澪）。
