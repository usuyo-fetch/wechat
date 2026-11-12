//! 读文章页：认出这一页是什么，取出标题、公众号、作者、发布时间、原创标记和正文。
//!
//! 来源（MIT 许可证）：
//! - 移植 wechat-article-exporter 的单篇做法：`shared/utils/html.ts` 的 `validateHTMLContent`（页面有 `#js_article` 是文章，
//!   `.weui-msg` 是被删除或出错，`.mesg-block` 是别的提示），`shared/utils/renderer.ts` 的 `extractContentHTML`
//!   （按 `item_show_type` 取正文：0 普通图文，8 图片分享，10 文本分享）。MIT License，Copyright (c) 2024 Jock。
//! - 对照 z-mio/ParseHub `provider_api/weixin.py`：同样按 `item_show_type` 分三种，图片消息的图片取 `picture_page_info_list`。
//!   MIT License，Copyright (c) 2024 梓澪。
//! - 对照 fengxxc/wechatmp2markdown：标题、公众号名、发布时间从页面自带的数据里取。MIT License，Copyright (c) 2021 fengxxc。
//!
//! 值都来自页面内嵌的 `window.cgiDataNew`（[`crate::cgi`]，不运行脚本）；正文优先取 `#js_content` 的 HTML，
//! 没有（图片分享、文本分享的页面里没有这个元素）才用数据里的 `content_noencode`。
//!
//! 2026-11-12 对着真实页面量过（三条公开文章链接各请求一次，不带 cookie，桌面浏览器 UA），加上上游样例 30 多页：
//! - 文章页 2.6–4.1 MB，`item_show_type` 0（普通图文，含转载）、8（图片分享，没有 `#js_content`）、10（文本分享）；
//! - `copyright_info.copyright_stat`：1 原创、2 转载（带 `reprint_url`、`platform` 即原公众号名）、0 都不是；
//! - `ori_create_time` 是发布时间的 Unix 秒；`create_time` 是不带时区的字符串，不用；
//! - 图片分享的 `picture_page_info_list[].cdn_url` 是无水印的原图，地址里的 `&` 写成 `&amp;`；
//! - 被作者删除的页面没有 `cgiDataNew`，`.weui-msg__title` 写「该内容已被发布者删除」（英文页是
//!   「The content has been deleted by the author.」）；违规页同一处写「此内容因违规无法查看」；
//!   `<title>该内容暂时无法查看</title>` 的小页面（1 KB）只有一句话。
//! - 验证页：同一天经宿主 `usuyo add` 跑了四次（见 `src/api.rs` 顶部），插件的请求回过一次，通用网页插件的请求回过一次
//!   （后者的原始 HTML 存在 `raw/` 里）：状态 200，18 KB，没有 `cgiDataNew`、没有 `#js_content`；`.weui-msg__title` 写「环境异常」，
//!   `.weui-msg__desc` 写「当前环境异常，完成验证后即可继续访问。」，一个没有 `href` 的 `#js_verify` 按钮写「去验证」。
//!   下面按这两句话认验证页。
//! - 没有量到：只能在微信里打开的页面、视频、音频、付费文章的真实页面。下面对这几种的判断照上游源码和 issue 的文字写，
//!   测试夹具里是手写的。

use scraper::{ElementRef, Html, Node, Selector};
use serde_json::{Map, Value};
use usuyo_plugin_kit::protocol::{CallFailure, ErrorCode, NotFoundReason};

use crate::api::blocked;
use crate::cgi::{self, CgiError};
use crate::markdown::decode_amp;

/// 页面给的内容形态（`item_show_type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Article,
    ImagePost,
    TextPost,
    Video,
    Audio,
    /// 没见过的值：有正文就照文章取。
    Other(i64),
}

impl Shape {
    fn from(value: Option<i64>) -> Shape {
        match value {
            Some(0) | None => Shape::Article,
            Some(5) => Shape::Video,
            Some(7) => Shape::Audio,
            Some(8) => Shape::ImagePost,
            Some(10) => Shape::TextPost,
            Some(other) => Shape::Other(other),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Shape::Article => "article",
            Shape::ImagePost => "image-post",
            Shape::TextPost => "text-post",
            Shape::Video => "video",
            Shape::Audio => "audio",
            Shape::Other(_) => "other",
        }
    }
}

/// 正文：HTML（普通图文），或一段带少量标签的文字（图片分享、文本分享）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Html(String),
    Text(String),
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reprint {
    pub url: Option<String>,
    /// 原公众号名。
    pub account: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Post {
    pub shape: Shape,
    pub item_show_type: Option<i64>,
    pub title: Option<String>,
    pub account: Option<String>,
    /// 公众号的微信号 `gh_…`。
    pub account_id: Option<String>,
    pub alias: Option<String>,
    pub avatar: Option<String>,
    /// 文章署名的作者（不是公众号）。
    pub author: Option<String>,
    pub description: Option<String>,
    /// 发布时间，Unix 秒。
    pub published_at: Option<i64>,
    /// `copyright_info.copyright_stat`：1 原创，2 转载。
    pub copyright_stat: Option<i64>,
    pub reprint: Option<Reprint>,
    /// 「阅读原文」的链接，不是转载的出处。
    pub read_more_url: Option<String>,
    /// 页面自己写的 `link`。
    pub link: Option<String>,
    pub biz: Option<String>,
    pub mid: Option<String>,
    pub idx: Option<String>,
    pub sn: Option<String>,
    pub cover: Option<String>,
    pub body: Body,
    /// 图片分享的图片地址，按页面里的次序。
    pub pictures: Vec<String>,
    /// 页面数据里有视频或音频的记录。
    pub has_page_video: bool,
    pub has_page_audio: bool,
    pub ip_location: Option<String>,
    pub comment_id: Option<String>,
}

impl Post {
    pub fn is_original(&self) -> Option<bool> {
        self.copyright_stat.map(|s| s == 1)
    }
}

#[derive(Debug)]
pub enum PageError {
    /// 页面是一句话：内容没有了、或这里看不到。
    Unavailable {
        reason: Option<NotFoundReason>,
        message: String,
        hint: Option<String>,
        detail: String,
    },
    /// 「环境异常，完成验证后即可继续访问」。
    Verification { detail: String },
    /// 既没有文章数据，也不是认得的提示页。
    Missing { what: String, detail: String },
}

impl From<PageError> for CallFailure {
    fn from(err: PageError) -> CallFailure {
        match err {
            PageError::Unavailable {
                reason,
                message,
                hint,
                detail,
            } => CallFailure {
                reason,
                hint,
                ..CallFailure::new(ErrorCode::NotFound, message)
            }
            .with_detail(&detail),
            PageError::Verification { detail } => {
                blocked("the page is a verification page (环境异常)".into()).with_detail(&detail)
            }
            PageError::Missing { what, detail } => CallFailure {
                hint: Some("公众号文章页的结构变了，这个插件需要更新".into()),
                ..CallFailure::new(ErrorCode::UpstreamChanged, what)
            }
            .with_detail(&detail),
        }
    }
}

pub fn parse(html: &str) -> Result<Post, PageError> {
    match cgi::parse(html) {
        Ok(data) => post(html, &data),
        Err(CgiError::Missing) => Err(classify(html)),
        Err(e @ CgiError::Syntax(..)) => Err(PageError::Missing {
            what: e.to_string(),
            detail: visible_text(html),
        }),
    }
}

fn post(html: &str, data: &Map<String, Value>) -> Result<Post, PageError> {
    let item_show_type = cgi::int(data, "item_show_type");
    let shape = Shape::from(item_show_type);
    let content = cgi::text(data, "content_noencode");

    // 付费文章没买时，正文只剩一个占位元素
    if cgi::int(data, "is_pay_subscribe") == Some(1)
        && content.is_none_or(|c| c.contains("mp-pay-preview-filter"))
    {
        return Err(PageError::Unavailable {
            reason: Some(NotFoundReason::Paid),
            message: "the article is a paid one and its body is not in the page".into(),
            hint: Some("这是一篇付费文章，要购买后才看得到正文".into()),
            detail: visible_text(html),
        });
    }

    let body = match shape {
        Shape::TextPost => text_page(data).map_or(Body::None, Body::Text),
        Shape::ImagePost => content.map_or(Body::None, |c| Body::Text(c.to_owned())),
        _ => js_content(html)
            .or_else(|| content.map(str::to_owned))
            .map_or(Body::None, Body::Html),
    };
    let pictures = data
        .get("picture_page_info_list")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|p| p.get("cdn_url")?.as_str())
                .map(decode_amp)
                .filter(|u| !u.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let copyright = data.get("copyright_info").and_then(Value::as_object);
    let copyright_stat = copyright.and_then(|c| cgi::int(c, "copyright_stat"));
    let reprint = copyright
        .filter(|_| copyright_stat == Some(2))
        .map(|c| Reprint {
            url: cgi::text(c, "reprint_url").map(decode_amp),
            account: cgi::text(c, "platform").map(str::to_owned),
        });

    let non_empty = |key: &str| {
        data.get(key)
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    };
    let has_page_video = shape == Shape::Video
        || data
            .get("video_page_info")
            .and_then(|v| v.get("mp_video_trans_info"))
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty());
    let has_page_audio = shape == Shape::Audio || non_empty("voice_in_appmsg");

    // 页面自己写的 `link`、`bizuin`、`mid`、`idx`、`sn`；老页面没有后四个，从长链接里补
    let link = cgi::text(data, "link").map(decode_amp);
    let from_link = link
        .as_deref()
        .and_then(|l| match crate::link::parse_input(l) {
            Some(crate::link::Article::Long {
                biz, mid, idx, sn, ..
            }) => Some((biz, mid, idx, sn)),
            _ => None,
        });
    let id = |key: &str| {
        cgi::text(data, key)
            .map(str::to_owned)
            .or_else(|| cgi::int(data, key).map(|n| n.to_string()))
    };

    Ok(Post {
        shape,
        item_show_type,
        title: cgi::text(data, "title").map(str::to_owned),
        account: cgi::text(data, "nick_name").map(str::to_owned),
        account_id: cgi::text(data, "user_name").map(str::to_owned),
        alias: cgi::text(data, "alias").map(str::to_owned),
        avatar: cgi::text(data, "round_head_img")
            .or_else(|| cgi::text(data, "hd_head_img"))
            .map(decode_amp),
        author: cgi::text(data, "author").map(str::to_owned),
        description: cgi::text(data, "desc").map(str::to_owned),
        published_at: cgi::int(data, "ori_create_time")
            .or_else(|| cgi::int(data, "create_timestamp"))
            .filter(|t| *t > 0),
        copyright_stat,
        reprint,
        read_more_url: cgi::text(data, "source_url").map(decode_amp),
        link,
        biz: id("bizuin").or_else(|| from_link.as_ref().map(|l| l.0.clone())),
        mid: id("mid").or_else(|| from_link.as_ref().map(|l| l.1.clone())),
        idx: id("idx").or_else(|| from_link.as_ref().map(|l| l.2.clone())),
        sn: id("sn").or_else(|| from_link.as_ref().and_then(|l| l.3.clone())),
        cover: cgi::text(data, "cdn_url").map(decode_amp),
        body,
        pictures,
        has_page_video,
        has_page_audio,
        ip_location: data
            .get("ip_wording")
            .and_then(Value::as_object)
            .and_then(|w| cgi::text(w, "province_name"))
            .map(str::to_owned),
        comment_id: cgi::text(data, "comment_id").map(str::to_owned),
    })
}

fn text_page(data: &Map<String, Value>) -> Option<String> {
    let info = data.get("text_page_info")?.as_object()?;
    cgi::text(info, "content_noencode").map(str::to_owned)
}

/// `#js_content` 里面的 HTML（页面把它设成不可见，由脚本改回来；这里直接取内容）。
fn js_content(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("#js_content").expect("constant selector");
    let inner = doc.select(&selector).next()?.inner_html();
    (!inner.trim().is_empty()).then_some(inner)
}

/// 没有文章数据的页面：验证页、被删除、违规、只在微信里能开，或认不出。
fn classify(html: &str) -> PageError {
    let doc = Html::parse_document(html);
    let text = visible_text_of(&doc);
    let detail = text.clone();
    let title_selector =
        Selector::parse(".weui-msg__title, .mesg-block, h1, h2").expect("constant selector");
    let heading = doc
        .select(&title_selector)
        .map(|e| squash(&e.text().collect::<String>()))
        .find(|t| !t.is_empty())
        .unwrap_or_default();

    if text.contains("环境异常") && text.contains("验证") {
        return PageError::Verification { detail };
    }
    let unavailable = |reason, hint: &str| PageError::Unavailable {
        reason,
        message: if heading.is_empty() {
            "the page has no article".to_owned()
        } else {
            heading.clone()
        },
        hint: Some(hint.to_owned()),
        detail: detail.clone(),
    };
    let lower = text.to_ascii_lowercase();
    if text.contains("已被发布者删除") || lower.contains("deleted by the author") {
        return unavailable(Some(NotFoundReason::Deleted), "作者已经把这篇文章删除了");
    }
    if ["违规", "封禁", "被投诉", "已被屏蔽", "已被删除"]
        .iter()
        .any(|m| heading.contains(m))
    {
        return unavailable(Some(NotFoundReason::Deleted), "这篇文章被平台下架了");
    }
    if text.contains("请在微信客户端打开") || text.contains("请使用微信") {
        return unavailable(None, "这篇文章只能在微信里打开，匿名网页看不到");
    }
    if text.contains("暂时无法查看") {
        return unavailable(None, "平台说这篇文章暂时无法查看");
    }
    PageError::Missing {
        what: "the page has neither article data nor a known notice".into(),
        detail,
    }
}

/// 页面上看得见的文字（不含脚本和样式），空白收成一个空格，最多 4 KB。
pub fn visible_text(html: &str) -> String {
    visible_text_of(&Html::parse_document(html))
}

fn visible_text_of(doc: &Html) -> String {
    let mut out = String::new();
    collect(doc.root_element(), &mut out);
    let out = squash(&out);
    let end = (0..=out.len().min(4096))
        .rev()
        .find(|&i| out.is_char_boundary(i))
        .unwrap_or(0);
    out[..end].to_owned()
}

fn collect(e: ElementRef, out: &mut String) {
    for child in e.children() {
        match child.value() {
            Node::Text(t) => {
                out.push_str(t);
                out.push(' ');
            }
            Node::Element(el) if !matches!(el.name(), "script" | "style" | "noscript") => {
                if let Some(c) = ElementRef::wrap(child) {
                    collect(c, out);
                }
            }
            _ => {}
        }
    }
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
