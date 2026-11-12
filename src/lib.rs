//! usuyo 的公众号获取插件：按一条文章链接取回文章，用于个人备份和离线阅读。
//! 协议见 usuyo 仓库的 `docs/protocol/`。

pub mod api;
pub mod cgi;
pub mod fetch;
pub mod link;
pub mod markdown;
pub mod page;
