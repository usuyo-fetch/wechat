//! 对着本机的假服务器跑：普通图文（懒加载的图片）、转载、图片分享、文本分享、视频页、带音频的文章、付费文章、
//! 被删除、违规、验证页、认不出的页面，以及 HTTP 状态和图片下载失败。不碰真网络。
//!
//! `fixtures/` 里的页面都是手写的，内容是编的。结构照 2026-11-12 量到的真实页面写（见 `src/page.rs` 顶部）：
//! `article.html`（单引号字面量、`#js_content`）、`image_post.html`、`text_post.html`、`deleted.html`、
//! `violation.html`、`unavailable.html`、`verification.html`（宿主失败日志和通用网页插件存下的真实验证页，
//! 18 KB：`.weui-msg__title` 写「环境异常」，`.weui-msg__desc` 写「当前环境异常，完成验证后即可继续访问。」，
//! 一个没有 `href` 的 `#js_verify` 按钮写「去验证」）；`wechat_only.html`、`video_post.html`、`article_media.html`、
//! `paid.html` 没有量到真实的页面，是照上游源码和 issue 的文字写的。

mod support;

use std::net::IpAddr;
use std::path::Path;

use support::{Request, Response, Server, fixture};
use usuyo_fetch_wechat::api::{Endpoints, Wechat};
use usuyo_fetch_wechat::fetch;
use usuyo_plugin::protocol::{
    Access, CallFailure, Completeness, ErrorCode, FetchOutput, Identity, ItemKind, NotFoundReason,
    OutputKind, TextFormat,
};
use usuyo_plugin_kit::http::Policy;

const ARTICLE: &str = "AbCdEfGh1234567890_-xY";
const REPOST: &str = "RePoSt0123456789abcdEf";
const IMAGE_POST: &str = "ImGpOsT01234567890abcD";
const TEXT_POST: &str = "TeXtPoSt01234567890abc";
const VIDEO_POST: &str = "ViDeOpOsT0123456789abc";
const MEDIA: &str = "MeDiAaRtIcLe0123456789";
const PAID: &str = "PaIdArTiClE0123456789a";
const DELETED: &str = "DeLeTeD0123456789abcdE";
const DELETED_EN: &str = "DeLeTeDeN123456789abcdE";
const VIOLATION: &str = "ViOlAtIoN0123456789abcd";
const UNAVAILABLE: &str = "UnAvAiLaBlE0123456789ab";
const WECHAT_ONLY: &str = "WeChAtOnLy0123456789abc";
const VERIFICATION: &str = "VeRiFiCaTiOn0123456789a";
const NO_DATA: &str = "NoDaTa0123456789abcdefg";
const BROKEN: &str = "BrOkEn0123456789abcdefg";
/// 只在测试里用的 id：各自触发一种服务器行为。
const ONE_IMAGE_GONE: &str = "OnEiMaGeGoNe0123456789";
const ALL_IMAGES_GONE: &str = "AlLiMaGeSgOnE0123456789";
const BEHIND_REDIRECT: &str = "ReDiReCtToCaPtChA01234";
const OFFSITE: &str = "OfFsItErEdIrEcT0123456";
const HTTP_403: &str = "Http403aaaaaaaaaaaaaaaa";
const HTTP_429: &str = "Http429aaaaaaaaaaaaaaaa";
const HTTP_500: &str = "Http500aaaaaaaaaaaaaaaa";
const HTTP_404: &str = "Http404aaaaaaaaaaaaaaaa";

fn local_policy() -> Policy {
    Policy {
        lookup: |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no DNS in tests",
            ))
        },
        public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
        cookies_over_http: true,
        ..Policy::strict()
    }
}

fn html(name: &str, base: &str) -> Response {
    Response {
        status: 200,
        headers: vec![("Content-Type", "text/html; charset=UTF-8".into())],
        body: fixture(name, base).into_bytes(),
    }
}

fn page_with(name: &str, base: &str, edit: impl Fn(String) -> String) -> Response {
    Response {
        status: 200,
        headers: vec![("Content-Type", "text/html; charset=UTF-8".into())],
        body: edit(fixture(name, base)).into_bytes(),
    }
}

fn image(content_type: Option<&str>, bytes: &[u8]) -> Response {
    Response {
        status: 200,
        headers: content_type
            .map(|t| vec![("Content-Type", t.to_owned())])
            .unwrap_or_default(),
        body: bytes.to_vec(),
    }
}

fn article_page(id: &str, base: &str) -> Response {
    match id {
        ARTICLE => html("article.html", base),
        REPOST => html("article_repost.html", base),
        IMAGE_POST => html("image_post.html", base),
        TEXT_POST => html("text_post.html", base),
        VIDEO_POST => html("video_post.html", base),
        MEDIA => html("article_media.html", base),
        PAID => html("paid.html", base),
        DELETED => html("deleted.html", base),
        DELETED_EN => html("deleted_en.html", base),
        VIOLATION => html("violation.html", base),
        UNAVAILABLE => html("unavailable.html", base),
        WECHAT_ONLY => html("wechat_only.html", base),
        VERIFICATION => html("verification.html", base),
        NO_DATA => html("no_data.html", base),
        BROKEN => html("broken_data.html", base),
        ONE_IMAGE_GONE => page_with("article.html", base, |p| p.replace("BBBB2222", "MISSING0")),
        ALL_IMAGES_GONE => page_with("article.html", base, |p| p.replace("mmbiz_", "gone_mmbiz_")),
        BEHIND_REDIRECT => Response::redirect("/mp/wappoc_appmsgcaptcha?poc_token=x"),
        OFFSITE => Response::redirect("https://example.com/x"),
        HTTP_403 => Response::status(403).header("Content-Type", "text/html"),
        HTTP_429 => Response::status(429).header("Retry-After", "30"),
        HTTP_500 => Response::status(500),
        _ => Response::status(404),
    }
}

/// 一个什么都有的微信。
fn wechat(req: &Request, base: &str) -> Response {
    if let Some(id) = req.path.strip_prefix("/s/") {
        return article_page(id, base);
    }
    match req.path.as_str() {
        // 长链接：`mid` 认出是哪一篇
        "/s" => match req.query.get("mid").map(String::as_str) {
            Some("2650000001") => html("article.html", base),
            _ => Response::status(404),
        },
        "/mp/wappoc_appmsgcaptcha" => html("verification.html", base),
        "/mmbiz_jpg/AAAA1111/640" => image(Some("image/jpeg"), b"IMG-A"),
        "/mmbiz_png/BBBB2222/0" => image(Some("image/png"), b"IMG-B"),
        // 没有 Content-Type：扩展名看地址里的 `wx_fmt`
        "/mmbiz_gif/CCCC3333/0" => image(None, b"IMG-C"),
        "/mmbiz_jpg/COVER000/0" => image(Some("image/jpeg"), b"COVER"),
        "/mmbiz_jpg/COVER888/0" => image(Some("image/jpeg"), b"COVER8"),
        "/mmbiz_jpg/DDDD4444/0" => image(Some("image/jpeg"), b"IMG-D"),
        "/mmbiz_jpg/PICA0001/0" => image(Some("image/jpeg"), b"PIC-A"),
        "/mmbiz_jpg/PICB0002/0" => image(Some("image/webp"), b"PIC-B"),
        "/mmbiz_jpg/VCOVER00/0" => image(Some("image/jpeg"), b"VCOVER"),
        _ => Response::status(404),
    }
}

fn api_for(server: &Server) -> Wechat {
    Wechat::new(Endpoints::local(&server.base, local_policy()))
}

fn failure<T: std::fmt::Debug>(result: Result<T, CallFailure>) -> CallFailure {
    result.expect_err("expected a failure")
}

fn short_url(id: &str) -> String {
    format!("https://mp.weixin.qq.com/s/{id}")
}

fn fetch_with(
    server: &Server,
    input: &str,
    want: Option<&[OutputKind]>,
) -> (tempfile::TempDir, Result<FetchOutput, CallFailure>) {
    let tmp = tempfile::tempdir().unwrap();
    let result = fetch::fetch(&api_for(server), input, tmp.path(), want);
    (tmp, result)
}

fn fetch_ok(server: &Server, id: &str) -> (tempfile::TempDir, FetchOutput) {
    let (tmp, result) = fetch_with(server, &short_url(id), None);
    (tmp, result.unwrap())
}

/// 输出目录里的全部文件，相对路径，排好序。
fn files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(
                    path.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn image_paths(out: &FetchOutput) -> Vec<&str> {
    out.images.iter().map(|p| p.as_str()).collect()
}

fn content_id(out: &FetchOutput) -> &str {
    match &out.identity {
        Some(Identity::Platform {
            platform,
            content_id,
            ..
        }) => {
            assert_eq!(platform.as_str(), "wechat");
            content_id
        }
        other => panic!("{other:?}"),
    }
}

fn meta(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap()
}

fn text(out: &FetchOutput) -> &str {
    out.text.as_deref().unwrap()
}

#[test]
fn an_article_gives_markdown_in_order_images_the_page_and_meta() {
    let server = Server::start(wechat);
    let (tmp, out) = fetch_ok(&server, ARTICLE);

    assert_eq!(content_id(&out), ARTICLE);
    assert_eq!(out.kind, ItemKind::Article);
    assert_eq!(out.title.as_deref(), Some("周刊第 12 期：慢下来"));
    let author = out.author.as_ref().unwrap();
    assert_eq!(
        (
            author.name.as_str(),
            author.platform_id.as_deref(),
            author.url.as_deref()
        ),
        ("测试周刊", Some("gh_0123456789ab"), None)
    );
    assert_eq!(
        out.published_at.as_ref().unwrap().as_str(),
        "2025-10-01T00:00:00Z"
    );

    // 正文：Markdown，图片留在原处，写页面给的原地址（懒加载的 data-src）
    assert_eq!(out.text_format, Some(TextFormat::Markdown));
    let md = text(&out);
    let base = &server.base;
    assert_eq!(
        md,
        format!(
            "这是一篇用来测试的文章，内容是编的。\n\n\
             ## 第一节\n\n\
             有 **加粗** 和 [链接](https://example.com/a?x=1&y=2)，还有 1 &lt; 2。\n\n\
             ![第一张图]({base}/mmbiz_jpg/AAAA1111/640?wx_fmt=jpeg&from=appmsg)\n\n\
             - 条目一\n- 条目二\n\n\
             > 一句引用。\n\n\
             ![]({base}/mmbiz_png/BBBB2222/0?wx_fmt=png)![同一张图再出现]({base}/mmbiz_jpg/AAAA1111/640?wx_fmt=jpeg&from=appmsg)\n\n\
             ![微笑](https://res.wx.qq.com/t/wx_fed/we-emoji/res/assets/newemoji/Smile.png)\n\n\
             ```rust\nfn main() {{\n    println!(\"hi\");\n}}\n```\n\n\
             ## 第二节\n\n\
             最后一张图：\n\n\
             ![]({base}/mmbiz_gif/CCCC3333/0?wx_fmt=gif)"
        )
    );
    // 宿主要把它原样写进 `content.md`（SPEC §1.2.3）：没有 `<`（代码块除外，这里没有），每个目标都是 http(s)
    assert!(!md.contains('<'));
    for target in md.split("](").skip(1) {
        assert!(target.starts_with("http"), "{target}");
    }

    // 图片：封面，再是正文里第一次出现的次序；同一个地址只存一份；名单外的主机不下载、不算缺
    assert_eq!(
        image_paths(&out),
        [
            "images/cover.jpg",
            "images/001.jpg",
            "images/002.png",
            "images/003.gif"
        ]
    );
    let read = |p: &str| std::fs::read(tmp.path().join(p)).unwrap();
    assert_eq!(read("images/cover.jpg"), b"COVER");
    assert_eq!(read("images/001.jpg"), b"IMG-A");
    assert_eq!(read("images/002.png"), b"IMG-B");
    assert_eq!(read("images/003.gif"), b"IMG-C");
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.missing.is_empty() && out.missing_reason.is_none());
    assert_eq!(out.evidence.access, Access::PublicWeb);
    assert_eq!(
        (out.evidence.site.as_deref(), out.evidence.status),
        (None, Some(200))
    );
    assert!(out.media.is_empty());

    // 页面原样存
    assert_eq!(out.html.as_ref().unwrap().as_str(), "page.html");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("page.html")).unwrap(),
        fixture("article.html", &server.base)
    );
    assert_eq!(
        files(tmp.path()),
        [
            "images/001.jpg",
            "images/002.png",
            "images/003.gif",
            "images/cover.jpg",
            "meta.json",
            "page.html"
        ]
    );

    let meta = meta(tmp.path());
    assert_eq!(meta["kind"], "article");
    assert_eq!(meta["account"]["name"], "测试周刊");
    assert_eq!(meta["account"]["id"], "gh_0123456789ab");
    assert_eq!(meta["account"]["alias"], "test-weekly");
    assert_eq!(meta["account"]["biz"], "MzAwMDAwMDAwMA==");
    assert_eq!(meta["author"], "编者");
    assert_eq!(meta["publishedAt"], "2025-10-01T00:00:00Z");
    assert_eq!(meta["original"], true);
    assert_eq!(meta["repostedFrom"], serde_json::Value::Null);
    assert_eq!(meta["readMoreUrl"], "https://example.com/weekly/12");
    assert_eq!(meta["ids"]["inputForm"], "short");
    assert_eq!(meta["ids"]["shortId"], ARTICLE);
    assert_eq!(
        (
            meta["ids"]["mid"].as_str(),
            meta["ids"]["idx"].as_str(),
            meta["ids"]["sn"].as_str()
        ),
        (
            Some("2650000001"),
            Some("1"),
            Some("0123456789abcdef0123456789abcdef")
        )
    );
    assert_eq!(meta["ipLocation"], "北京");
    let images = meta["images"].as_array().unwrap();
    assert_eq!(images.len(), 4);
    assert_eq!(images[0]["name"], "cover");
    assert_eq!(images[0]["path"], "images/cover.jpg");
    assert_eq!(
        images[1]["url"],
        format!("{base}/mmbiz_jpg/AAAA1111/640?wx_fmt=jpeg&from=appmsg")
    );
    assert_eq!(images[1]["path"], "images/001.jpg");

    // 页面请求带浏览器 UA、不带 cookie；每张图只请求一次；没有请求名单外的主机
    let requests = server.requests();
    let page = requests.iter().find(|r| r.path.starts_with("/s/")).unwrap();
    assert!(page.headers["user-agent"].contains("Chrome"));
    assert!(requests.iter().all(|r| r.cookie().is_none()));
    assert!(requests.iter().all(|r| !r.headers.contains_key("referer")));
    let mut paths = server.paths();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/mmbiz_gif/CCCC3333/0",
            "/mmbiz_jpg/AAAA1111/640",
            "/mmbiz_jpg/COVER000/0",
            "/mmbiz_png/BBBB2222/0",
            &format!("/s/{ARTICLE}"),
        ]
    );
}

#[test]
fn a_long_link_keeps_chksm_for_the_request_only_and_has_its_own_identity() {
    // 长链接取到的页面，`link` 也是长链接的写法（2026-11-12 实测，见 `src/api.rs`），页面里没有短 id
    let server = Server::start(|req, base| {
        if req.path == "/s" {
            return page_with("article.html", base, |p| {
                p.replace(
                    "link: 'https://mp.weixin.qq.com/s/AbCdEfGh1234567890_-xY'",
                    "link: 'http://mp.weixin.qq.com/s?__biz=MzAwMDAwMDAwMA==\\x26amp;mid=2650000001\\x26amp;idx=1\\x26amp;sn=0123456789abcdef0123456789abcdef'",
                )
            });
        }
        wechat(req, base)
    });
    let input = "http://mp.weixin.qq.com/s?__biz=MzAwMDAwMDAwMA==&mid=2650000001&idx=1\
                 &sn=0123456789abcdef0123456789abcdef&chksm=ab12cd&scene=21&srcid=0&token=1#wechat_redirect";
    let (tmp, result) = fetch_with(&server, input, None);
    let out = result.unwrap();
    assert_eq!(content_id(&out), "MzAwMDAwMDAwMA==:2650000001:1");
    match &out.identity {
        Some(Identity::Platform { url, .. }) => assert_eq!(
            url.as_deref(),
            Some(
                "https://mp.weixin.qq.com/s?__biz=MzAwMDAwMDAwMA%3D%3D&mid=2650000001&idx=1&sn=0123456789abcdef0123456789abcdef"
            )
        ),
        other => panic!("{other:?}"),
    }
    // 请求：四个参数加 `chksm`；`scene`、`srcid`、`token` 和片段不带
    let request = server
        .requests()
        .into_iter()
        .find(|r| r.path == "/s")
        .unwrap();
    let keys: Vec<&str> = request.query.keys().map(String::as_str).collect();
    assert_eq!(keys, ["__biz", "chksm", "idx", "mid", "sn"]);
    assert_eq!(request.query["__biz"], "MzAwMDAwMDAwMA==");
    assert_eq!(request.query["chksm"], "ab12cd");
    // meta.json：页面自己的 `link` 是长写法，没有短 id；四个标识从页面数据里来
    let meta = meta(tmp.path());
    assert_eq!(meta["ids"]["inputForm"], "long");
    assert_eq!(meta["ids"]["shortId"], serde_json::Value::Null);
    assert_eq!(
        meta["ids"]["link"],
        "http://mp.weixin.qq.com/s?__biz=MzAwMDAwMDAwMA==&mid=2650000001&idx=1&sn=0123456789abcdef0123456789abcdef"
    );
    assert_eq!(meta["ids"]["mid"], "2650000001");

    // 没有 `chksm` 的长链接：只请求四个参数
    let bare = "https://mp.weixin.qq.com/s?__biz=MzAwMDAwMDAwMA==&mid=2650000001&idx=1&sn=0123456789abcdef0123456789abcdef";
    let before = server.requests().len();
    let (_tmp, result) = fetch_with(&server, bare, Some(&[OutputKind::Text]));
    result.unwrap();
    let request = server.requests().into_iter().nth(before).unwrap();
    let keys: Vec<&str> = request.query.keys().map(String::as_str).collect();
    assert_eq!(keys, ["__biz", "idx", "mid", "sn"]);
}

#[test]
fn a_repost_records_the_original_link_and_reads_the_json_decode_dialect() {
    let server = Server::start(wechat);
    let (tmp, out) = fetch_ok(&server, REPOST);
    assert_eq!(out.kind, ItemKind::Article);
    assert_eq!(out.title.as_deref(), Some("转载：一篇别处的文章"));
    // 页面没有 #js_content 的内容，正文取数据里的 content_noencode，图片同样是 data-src
    let md = text(&out);
    assert!(
        md.starts_with("推荐一篇好文，下面是原文的片段。\n\n![]("),
        "{md}"
    );
    assert_eq!(image_paths(&out), ["images/001.jpg"]);
    assert_eq!(out.completeness, Completeness::Full);
    let meta = meta(tmp.path());
    assert_eq!(meta["original"], false);
    assert_eq!(meta["copyrightStat"], 2);
    assert_eq!(meta["repostedFrom"]["account"], "原公众号");
    assert_eq!(
        meta["repostedFrom"]["url"],
        "http://mp.weixin.qq.com/s?__biz=MzAxMTExMTExMQ%3D%3D&mid=2654000009&idx=1&sn=fedcba9876543210fedcba9876543210"
    );
    assert_eq!(meta["author"], "原作者");
}

#[test]
fn an_image_post_gives_the_text_the_original_images_and_the_cover() {
    let server = Server::start(wechat);
    let (tmp, out) = fetch_ok(&server, IMAGE_POST);
    assert_eq!(out.kind, ItemKind::Post);
    assert_eq!(out.title.as_deref(), Some("周末随拍"));
    let base = &server.base;
    assert_eq!(
        text(&out),
        format!(
            "周末随拍\n\n第一张是湖边 图1  \n第二张是树 1 &lt; 2\n\n\
             ![图1]({base}/mmbiz_jpg/PICA0001/0?wx_fmt=jpeg&from=appmsg)\n\n\
             ![图2]({base}/mmbiz_jpg/PICB0002/0?wx_fmt=jpeg&from=appmsg)"
        )
    );
    // 封面，再是图片；用 `cdn_url`（原图），不用 `watermark_info`；扩展名看响应的 Content-Type
    assert_eq!(
        image_paths(&out),
        ["images/cover.jpg", "images/001.jpg", "images/002.webp"]
    );
    assert_eq!(
        std::fs::read(tmp.path().join("images/002.webp")).unwrap(),
        b"PIC-B"
    );
    assert_eq!(out.completeness, Completeness::Full);
    assert!(
        !server
            .paths()
            .iter()
            .any(|p| p.contains("WMARK") || p.contains("SHARE"))
    );
    let meta = meta(tmp.path());
    assert_eq!(meta["kind"], "image-post");
    assert_eq!(meta["original"], false);
}

#[test]
fn a_text_post_has_text_only_and_nothing_else_is_missing() {
    let server = Server::start(wechat);
    let (tmp, out) = fetch_ok(&server, TEXT_POST);
    assert_eq!(out.kind, ItemKind::Post);
    // 标题取第一行
    assert_eq!(out.title.as_deref(), Some("今天只写两句话"));
    assert_eq!(text(&out), "今天只写两句话\n\n第二段有 # 号和 \\* 号。");
    assert!(out.images.is_empty());
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.missing.is_empty());
    assert_eq!(files(tmp.path()), ["meta.json", "page.html"]);
    assert_eq!(server.paths(), [format!("/s/{TEXT_POST}")]);
}

#[test]
fn a_video_post_gives_the_description_and_cover_and_says_the_video_is_missing() {
    let server = Server::start(wechat);
    let (tmp, out) = fetch_ok(&server, VIDEO_POST);
    assert_eq!(out.kind, ItemKind::Video);
    assert_eq!(text(&out), "视频的简介，内容是编的。");
    assert_eq!(image_paths(&out), ["images/cover.jpg"]);
    assert!(out.media.is_empty());
    assert_eq!(out.missing, [OutputKind::Video]);
    assert_eq!(out.completeness, Completeness::Partial);
    assert!(out.missing_reason.as_deref().unwrap().contains("视频"));
    // 带签名的视频地址没有被请求
    assert!(!server.paths().iter().any(|p| p.contains("signed")));
    assert_eq!(meta(tmp.path())["embeds"]["pageVideo"], true);
}

#[test]
fn audio_and_embedded_video_are_recorded_and_reported_missing() {
    let server = Server::start(wechat);
    let (tmp, out) = fetch_ok(&server, MEDIA);
    assert_eq!(
        text(&out),
        "先听一段。\n\n[音频：一期节目](https://res.wx.qq.com/voice/getvoice?mediaid=MzAwMDAwMDAwMF8xMjM0)\n\n再看视频。\n\n[视频](https://v.qq.com/iframe/preview.html?vid=v0123abc&width=500)"
    );
    assert_eq!(out.missing, [OutputKind::Video, OutputKind::Audio]);
    assert_eq!(out.completeness, Completeness::Partial);
    let meta = meta(tmp.path());
    assert_eq!(meta["embeds"]["audios"][0]["name"], "一期节目");
    assert_eq!(
        meta["embeds"]["audios"][0]["url"],
        "https://res.wx.qq.com/voice/getvoice?mediaid=MzAwMDAwMDAwMF8xMjM0"
    );
    assert_eq!(
        meta["embeds"]["videos"][0],
        "https://v.qq.com/iframe/preview.html?vid=v0123abc&width=500"
    );
    // 只有页面本身被请求
    assert_eq!(server.paths(), [format!("/s/{MEDIA}")]);
}

#[test]
fn a_failed_image_makes_images_missing_with_the_count_and_keeps_the_rest() {
    let server = Server::start(wechat);
    let (tmp, out) = fetch_ok(&server, ONE_IMAGE_GONE);
    assert_eq!(
        image_paths(&out),
        ["images/cover.jpg", "images/001.jpg", "images/003.gif"]
    );
    assert_eq!(out.missing, [OutputKind::Images]);
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(
        out.missing_reason.as_deref(),
        Some("1 张图片没有下载下来（共 4 张）")
    );
    // 正文照样是全的
    assert!(text(&out).contains("![](") && text(&out).contains("MISSING0"));
    let images = meta(tmp.path())["images"].as_array().unwrap().clone();
    let failed = images.iter().find(|i| i["name"] == "002").unwrap();
    assert!(failed.get("path").is_none());
    assert!(failed["why"].as_str().unwrap().contains("404"));
}

#[test]
fn when_no_image_arrives_images_are_missing_and_the_text_is_still_given() {
    let server = Server::start(wechat);
    let (_tmp, out) = fetch_ok(&server, ALL_IMAGES_GONE);
    assert!(out.images.is_empty());
    assert_eq!(out.missing, [OutputKind::Images]);
    assert_eq!(
        out.missing_reason.as_deref(),
        Some("图片都没有下载下来（共 4 张）")
    );
    assert!(out.text.is_some());
}

#[test]
fn want_limits_the_work_and_what_was_not_wanted_is_not_missing() {
    let server = Server::start(wechat);
    let (tmp, result) = fetch_with(&server, &short_url(ARTICLE), Some(&[OutputKind::Text]));
    let out = result.unwrap();
    assert!(out.text.is_some() && out.images.is_empty() && out.html.is_none());
    assert!(out.title.is_none() && out.author.is_none());
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.missing.is_empty());
    assert_eq!(files(tmp.path()), Vec::<String>::new());
    assert_eq!(server.paths(), [format!("/s/{ARTICLE}")]);

    let (_tmp, result) = fetch_with(&server, &short_url(ARTICLE), Some(&[OutputKind::Video]));
    assert_eq!(failure(result).code, ErrorCode::InvalidArgs);
}

#[test]
fn pages_that_are_not_articles_give_the_right_error_with_detail() {
    let server = Server::start(wechat);
    let call = |id: &str| failure(fetch_with(&server, &short_url(id), None).1);

    let deleted = call(DELETED);
    assert_eq!(
        (deleted.code, deleted.reason),
        (ErrorCode::NotFound, Some(NotFoundReason::Deleted))
    );
    assert!(
        deleted
            .detail
            .as_deref()
            .unwrap()
            .contains("该内容已被发布者删除")
    );
    assert!(deleted.message.contains("该内容已被发布者删除"));

    let deleted_en = call(DELETED_EN);
    assert_eq!(deleted_en.reason, Some(NotFoundReason::Deleted));

    // 违规下架：平台上看不到了，原因写在 message 里
    let violation = call(VIOLATION);
    assert_eq!(
        (violation.code, violation.reason),
        (ErrorCode::NotFound, Some(NotFoundReason::Deleted))
    );
    assert!(violation.message.contains("违规"));
    assert!(
        violation
            .detail
            .as_deref()
            .unwrap()
            .contains("涉嫌违反相关规定")
    );

    // 「暂时无法查看」和只能在微信里开：不写 reason，列进「取不到」
    for id in [UNAVAILABLE, WECHAT_ONLY] {
        let f = call(id);
        assert_eq!((f.code, f.reason), (ErrorCode::NotFound, None), "{id}");
        assert!(f.hint.is_some() && f.detail.is_some());
    }

    let paid = call(PAID);
    assert_eq!(
        (paid.code, paid.reason),
        (ErrorCode::NotFound, Some(NotFoundReason::Paid))
    );

    let verification = call(VERIFICATION);
    assert_eq!(verification.code, ErrorCode::Blocked);
    assert!(
        verification
            .detail
            .as_deref()
            .unwrap()
            .contains("当前环境异常，完成验证后即可继续访问")
    );
    assert!(verification.hint.is_some());

    // 认不出的页面和读不出的数据：页面改了
    let no_data = call(NO_DATA);
    assert_eq!(no_data.code, ErrorCode::UpstreamChanged);
    assert!(
        no_data
            .detail
            .as_deref()
            .unwrap()
            .contains("一个认不出的页面")
    );
    let broken = call(BROKEN);
    assert_eq!(broken.code, ErrorCode::UpstreamChanged);
    assert!(broken.message.contains("cgiDataNew"));
}

#[test]
fn http_statuses_and_redirects_map_to_the_error_codes() {
    let server = Server::start(wechat);
    let call = |id: &str| failure(fetch_with(&server, &short_url(id), None).1);
    assert_eq!(call(HTTP_403).code, ErrorCode::Blocked);
    let limited = call(HTTP_429);
    assert_eq!(
        (limited.code, limited.retry_after),
        (ErrorCode::RateLimited, Some(30))
    );
    assert_eq!(call(HTTP_500).code, ErrorCode::Network);
    assert_eq!(call(HTTP_404).code, ErrorCode::NotFound);
    // 跳到验证页：跟过去，读到验证页，回 blocked
    assert_eq!(call(BEHIND_REDIRECT).code, ErrorCode::Blocked);
    // 跳去别的主机：不跟
    assert_eq!(call(OFFSITE).code, ErrorCode::UpstreamChanged);
    assert!(!server.paths().iter().any(|p| p.contains("example")));
}

#[test]
fn a_link_that_is_not_an_article_makes_no_request() {
    let server = Server::start(wechat);
    for input in ["https://example.com/s/AbCdEfGh1234567890_-xY", "nonsense"] {
        let (_tmp, result) = fetch_with(&server, input, None);
        assert_eq!(failure(result).code, ErrorCode::UnsupportedInput, "{input}");
    }
    assert!(server.paths().is_empty());
}
