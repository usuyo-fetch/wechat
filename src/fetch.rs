//! 方法 `article`：一条文章链接取一篇文章（协议 §3.6、§3.7）。不带账号：请求都不带 cookie，不读宿主交来的凭据，
//! 不带任何客户端算出来的参数。
//!
//! 交出的东西：
//! - `text`：正文的 Markdown（`textFormat: markdown`，写法和限制见 `markdown.rs`）。宿主要用它生成 `content.md` 和索引。
//!   图片的位置写页面给的原地址；
//! - `html`：`page.html`，取到的页面原样（几 MB，带页面自己的脚本）。文章页是服务端渲染的，原样存下来以后换别的读法不必再取；
//! - `images`：封面 `images/cover.<ext>`（页面的 `cdn_url`），再是正文里的图片 `images/001.<ext>`…，按第一次出现的次序，
//!   同一个地址只存一份；对应关系在 `meta.json`；
//! - `meta.json`：公众号名和 id、作者、发布时间、原创标记、转载的出处、文章自己的各种标识、封面和每张图的地址与文件。
//!
//! 不下载页面里的视频和音频：视频要走带签名的接口（客户端算参数，不在「公开页面、不算任何东西」之内）；
//! 音频的地址虽是 `res.wx.qq.com/voice/getvoice?mediaid=…`，没有测过。两种都把地址记进 `meta.json`、写进正文的链接，
//! 并在 `missing` 里写 `media:video` / `media:audio`，宿主据此找能补的方法。

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use scraper::Html;
use serde_json::{Value, json};
use usuyo_plugin_kit::protocol::{
    Access, Author, CallFailure, Completeness, ErrorCode, Evidence, FetchOutput, ItemKind,
    OutputKind, RelPath, Rfc3339, TextFormat,
};

use crate::api::{ApiError, Wechat, progress};
use crate::link::{self, Article};
use crate::markdown::{self, Audio, Converted};
use crate::page::{self, Body, Post, Shape};

/// 本方法能给的输出，和清单里的 `outputs` 一致。
pub const OUTPUTS: [OutputKind; 4] = [
    OutputKind::Text,
    OutputKind::Html,
    OutputKind::Images,
    OutputKind::Meta,
];

const IMAGE_BYTES: u64 = 40 << 20;
const IMAGES_TOTAL_BYTES: u64 = 1 << 30;
/// 图片下载的总时间预算；超了的不再下，记进 `missing`。
const IMAGES_BUDGET: Duration = Duration::from_secs(8 * 60);
/// 一篇最多存几张图：图片分享最多 20 张，长文几十张，再多的不取。
const MAX_IMAGES: usize = 300;
const TITLE_CHARS: usize = 100;

pub fn fetch(
    api: &Wechat,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
) -> Result<FetchOutput, CallFailure> {
    let wanted: BTreeSet<OutputKind> =
        want.map_or_else(|| OUTPUTS.into(), |w| w.iter().copied().collect());
    if let Some(other) = wanted.iter().find(|w| !OUTPUTS.contains(w)) {
        return Err(CallFailure::new(
            ErrorCode::InvalidArgs,
            format!("cannot give `{other}`"),
        ));
    }
    let article = link::parse_input(input).ok_or_else(|| {
        CallFailure::new(ErrorCode::UnsupportedInput, "not a wechat article link")
    })?;
    let identity = article.identity()?;

    let fetched_at = Rfc3339::from_timestamp(jiff::Timestamp::now());
    let fetched = api.article_page(&article.request_url())?;
    let html = String::from_utf8_lossy(&fetched.body);
    let post = page::parse(&html).map_err(CallFailure::from)?;

    let mut output = FetchOutput {
        identity: Some(identity),
        kind: match post.shape {
            Shape::Article | Shape::Other(_) => ItemKind::Article,
            Shape::Video => ItemKind::Video,
            Shape::ImagePost | Shape::TextPost | Shape::Audio => ItemKind::Post,
        },
        title: None,
        author: None,
        published_at: None,
        text: None,
        text_format: None,
        html: None,
        images: Vec::new(),
        media: Vec::new(),
        platform_tags: Vec::new(),
        comments: None,
        completeness: Completeness::Partial,
        missing: Vec::new(),
        missing_reason: None,
        evidence: Evidence {
            fetched_at,
            access: Access::PublicWeb,
            site: None,
            upstream: None,
            status: Some(fetched.status),
        },
        credentials_update: None,
    };
    let mut reasons: Vec<String> = Vec::new();
    // 这一篇本来没有的部分不算缺（协议 §3.7）；交来一部分、还缺一部分的照样记缺
    let mut absent: BTreeSet<OutputKind> = BTreeSet::new();
    let mut partial: BTreeSet<OutputKind> = BTreeSet::new();

    let converted = convert(api, &post);

    if wanted.contains(&OutputKind::Html) {
        write(out, "page.html", &fetched.body)?;
        output.html = Some(rel("page.html")?);
    }

    let text = text_of(&post, &converted);
    if text.is_none() {
        absent.insert(OutputKind::Text);
    } else if wanted.contains(&OutputKind::Text) {
        output.text = text;
        output.text_format = Some(TextFormat::Markdown);
    }

    // 页面里的图片：封面在前，正文里的按次序；封面和正文某张同一个地址时只存一份
    let mut sources: Vec<(String, String)> = Vec::new();
    if let Some(cover) = post
        .cover
        .as_deref()
        .filter(|c| api.endpoints().is_article_image(c))
        && !converted.images.iter().any(|i| i == cover)
    {
        sources.push(("cover".into(), cover.to_owned()));
    }
    for (i, url) in converted.images.iter().enumerate().take(MAX_IMAGES) {
        sources.push((format!("{:03}", i + 1), url.clone()));
    }
    let over = converted.images.len().saturating_sub(MAX_IMAGES);
    let mut records: Vec<Value> = Vec::new();
    if sources.is_empty() {
        absent.insert(OutputKind::Images);
    } else if wanted.contains(&OutputKind::Images) {
        let failed = download_images(api, out, &sources, &mut output, &mut records)?;
        let total = sources.len() + over;
        let lost = failed + over;
        if lost > 0 {
            partial.insert(OutputKind::Images);
            reasons.push(if output.images.is_empty() {
                format!("图片都没有下载下来（共 {total} 张）")
            } else {
                format!("{lost} 张图片没有下载下来（共 {total} 张）")
            });
        }
    }

    // 视频和音频：只记地址，不下载
    let mut extra_missing: Vec<OutputKind> = Vec::new();
    if post.has_page_video || !converted.videos.is_empty() {
        extra_missing.push(OutputKind::Video);
        reasons.push(
            "文章里有视频：这种方法只记下页面给的地址，不下载（视频要走带签名的接口）".into(),
        );
    }
    if post.has_page_audio || !converted.audios.is_empty() {
        extra_missing.push(OutputKind::Audio);
        reasons.push("文章里有音频：这种方法只记下页面给的地址，不下载".into());
    }

    let meta_wanted = wanted.contains(&OutputKind::Meta);
    if meta_wanted {
        output.title = title_of(&post);
        output.author = author_of(&post);
        output.published_at = post
            .published_at
            .and_then(|t| jiff::Timestamp::from_second(t).ok())
            .map(Rfc3339::from_timestamp);
        if output.title.is_none() && output.author.is_none() && output.published_at.is_none() {
            absent.insert(OutputKind::Meta);
        }
        write(
            out,
            "meta.json",
            &serde_json::to_vec_pretty(&meta(&article, &post, &converted, &records, &output))
                .map_err(|e| CallFailure::new(ErrorCode::Internal, format!("meta.json: {e}")))?,
        )?;
    }

    let obtained = output.obtained();
    output.missing = wanted
        .iter()
        .filter(|kind| {
            !absent.contains(kind) && (partial.contains(kind) || !obtained.contains(kind))
        })
        .copied()
        .chain(extra_missing)
        .collect();
    output.completeness = if output.missing.is_empty() {
        Completeness::Full
    } else {
        Completeness::Partial
    };
    if !output.missing.is_empty() {
        output.missing_reason = Some(reasons.join("；"));
    }
    Ok(output)
}

fn convert(api: &Wechat, post: &Post) -> Converted {
    let is_image = |u: &str| api.endpoints().is_article_image(u);
    let mut converted = match &post.body {
        Body::Html(html) => {
            let doc = Html::parse_fragment(html);
            markdown::convert(doc.root_element(), is_image)
        }
        Body::Text(text) => markdown::convert_text(text, is_image),
        Body::None => Converted::default(),
    };
    // 图片分享：正文是文字，图片另给，接在文字后面。（普通图文的 `picture_page_info_list` 是正文图片的又一份记录，不再取）
    let pictures = if post.shape == Shape::ImagePost {
        post.pictures.as_slice()
    } else {
        &[]
    };
    for url in pictures {
        if api.endpoints().is_article_image(url) && !converted.images.contains(url) {
            converted.images.push(url.clone());
            let n = converted.images.len();
            if !converted.markdown.is_empty() {
                converted.markdown.push_str("\n\n");
            }
            converted.markdown.push_str(&format!("![图{n}]({url})"));
        }
    }
    converted
}

/// 正文；没有正文的（视频、音频页）用页面给的摘要。
fn text_of(post: &Post, converted: &Converted) -> Option<String> {
    let text = converted.markdown.trim();
    if !text.is_empty() {
        return Some(text.to_owned());
    }
    let description = post.description.as_deref()?;
    let text = markdown::convert_text(description, |_| false).markdown;
    (!text.trim().is_empty()).then_some(text)
}

fn title_of(post: &Post) -> Option<String> {
    let line = post
        .title
        .as_deref()?
        .lines()
        .find(|l| !l.trim().is_empty())?;
    Some(line.trim().chars().take(TITLE_CHARS).collect())
}

/// 作者栏写公众号（内容在平台上归属的账号）；文章署名的人在 `meta.json`。
fn author_of(post: &Post) -> Option<Author> {
    let name = post.account.clone().or_else(|| post.author.clone())?;
    Some(Author {
        name,
        platform_id: post.account_id.clone().or_else(|| post.biz.clone()),
        url: None,
    })
}

/// 下载并存好图片；返回没存下来的张数。限流是硬错误，别的只记进 `records`。
fn download_images(
    api: &Wechat,
    out: &Path,
    sources: &[(String, String)],
    output: &mut FetchOutput,
    records: &mut Vec<Value>,
) -> Result<usize, CallFailure> {
    let started = Instant::now();
    let mut total_bytes = 0u64;
    let mut failed = 0usize;
    for (done, (stem, raw)) in sources.iter().enumerate() {
        progress(done, sources.len(), "图片");
        let mut record = json!({"name": stem, "url": raw});
        let saved = if started.elapsed() > IMAGES_BUDGET {
            Err("超出下载图片的时间预算".to_owned())
        } else if total_bytes > IMAGES_TOTAL_BYTES {
            Err("超出图片的总大小上限".to_owned())
        } else {
            match fetch_image(api, stem, raw) {
                Ok((name, bytes)) => {
                    // 写盘失败不是图片的问题，整次失败
                    write(out, &name, &bytes)?;
                    total_bytes += bytes.len() as u64;
                    Ok(rel(&name)?)
                }
                Err(e) if e.is_hard() => return Err(e.into()),
                Err(e) => Err(e.to_string()),
            }
        };
        match saved {
            Ok(path) => {
                record["path"] = path.as_str().into();
                output.images.push(path);
            }
            Err(why) => {
                failed += 1;
                record["why"] = why.into();
            }
        }
        records.push(record);
    }
    Ok(failed)
}

/// 下载一张图，返回要存的文件名（输出目录里的相对路径）和内容。
fn fetch_image(api: &Wechat, stem: &str, raw: &str) -> Result<(String, Vec<u8>), ApiError> {
    let url = api.endpoints().image_url(raw)?;
    let (bytes, content_type) = api.image(&url, IMAGE_BYTES)?;
    let name = format!("images/{stem}.{}", extension(content_type.as_deref(), raw));
    Ok((name, bytes))
}

/// 文件扩展名：先看响应的 `Content-Type`，再看地址里的 `wx_fmt`（路径本身以 `/0`、`/640` 结尾，没有扩展名），都没有写 `jpg`。
fn extension(content_type: Option<&str>, url: &str) -> &'static str {
    let by_mime = content_type
        .and_then(|m| m.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase())
        .and_then(|m| mime_extension(m.strip_prefix("image/")?));
    let by_format = url::Url::parse(url).ok().and_then(|u| {
        u.query_pairs()
            .find(|(k, _)| k == "wx_fmt")
            .and_then(|(_, v)| mime_extension(&v.to_ascii_lowercase()))
    });
    by_mime.or(by_format).unwrap_or("jpg")
}

fn mime_extension(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "jpeg" | "jpg" => "jpg",
        "png" => "png",
        "gif" => "gif",
        "webp" => "webp",
        "avif" => "avif",
        "bmp" => "bmp",
        "svg" | "svg+xml" => "svg",
        _ => return None,
    })
}

fn meta(
    article: &Article,
    post: &Post,
    converted: &Converted,
    images: &[Value],
    output: &FetchOutput,
) -> Value {
    let short_id = match article {
        Article::Short { id } => Some(id.clone()),
        Article::Long { .. } => post
            .link
            .as_deref()
            .and_then(|l| match link::parse_input(l) {
                Some(Article::Short { id }) => Some(id),
                _ => None,
            }),
    };
    json!({
        "kind": post.shape.name(),
        "itemShowType": post.item_show_type,
        "title": post.title,
        "description": post.description,
        "account": {
            "name": post.account,
            "id": post.account_id,
            "alias": post.alias,
            "biz": post.biz,
            "avatar": post.avatar,
        },
        "author": post.author,
        "publishedAt": output.published_at,
        "original": post.is_original(),
        "copyrightStat": post.copyright_stat,
        "repostedFrom": post.reprint.as_ref().map(|r| json!({"url": r.url, "account": r.account})),
        "readMoreUrl": post.read_more_url,
        "ids": {
            "inputForm": match article { Article::Short { .. } => "short", Article::Long { .. } => "long" },
            "shortId": short_id,
            "biz": post.biz,
            "mid": post.mid,
            "idx": post.idx,
            "sn": post.sn,
            "link": post.link,
            "commentId": post.comment_id,
        },
        "ipLocation": post.ip_location,
        "images": images,
        "embeds": {
            "videos": converted.videos,
            "audios": converted.audios.iter().map(|a: &Audio| json!({
                "name": a.name, "mediaId": a.media_id, "url": a.url(),
            })).collect::<Vec<_>>(),
            "pageVideo": post.has_page_video,
            "pageAudio": post.has_page_audio,
        },
    })
}

fn rel(path: &str) -> Result<RelPath, CallFailure> {
    RelPath::try_from(path).map_err(|e| CallFailure::new(ErrorCode::Internal, e))
}

fn write(out: &Path, name: &str, bytes: &[u8]) -> Result<(), CallFailure> {
    let dest = out.join(name);
    let io = |e: std::io::Error| {
        CallFailure::new(ErrorCode::Internal, format!("cannot write `{name}`: {e}"))
    };
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(&dest, bytes).map_err(io)
}
