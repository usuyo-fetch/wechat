//! 公众号的 HTTP 请求：文章页、文章里的图片。都不带 cookie，不读宿主交来的凭据，不带任何客户端算出来的参数。
//! 请求都经 `usuyo_plugin_kit::http`：页面只连 `mp.weixin.qq.com`，图片只连微信的图片域名，只连公网地址。
//!
//! 2026-11-12 实测（这台机器，不带 cookie，桌面浏览器 UA）：
//! - 三条公开文章链接各用 curl 请求一次：普通图文、图片分享各回 200 和完整的页面（4.1 MB、2.6 MB），一条被作者删除的回 200 和
//!   「该内容已被发布者删除」的页面（32 KB）；
//! - 经宿主 `usuyo add` 跑了四次（UTC）：
//!   1. 20:44:15 短链接的普通图文：51 张正文图加封面全部下载，完整；
//!   2. 紧接着一条图片分享的长链接（输入带 `chksm=abc`、`scene=21`），宿主按节奏（间隔 30 秒）排到下一轮，没有发请求；
//!   3. 同一条链接重试，20:45:09 插件请求了四个参数（没有 `chksm`），回了验证页，插件回 `blocked`（距第 1 次结束 54 秒）；
//!      宿主换通用网页插件请求用户给的整条链接（带 `chksm=abc`、`scene=21`），31 秒后（宿主的间隔）拿到了完整的页面，
//!      页面里的 `link` 是长链接的写法（`s?__biz=…`），没有短 id；
//!   4. 20:46:18 同一条链接（这次输入不带 `chksm`）：宿主因为第 3 次的 `blocked` 暂停了本方法（`jobs.db` 的 `pauses` 到 21:45:09），
//!      用了通用网页插件，它请求的是四个参数，回了验证页（18 KB，存在 `raw/` 里）。
//!
//! 所以：不带 `chksm` 的长链接请求了两次（插件一次、通用网页插件一次），都回验证页；带着 `chksm`（值是假的）和 `scene` 的
//! 一次回完整页面；第 1 次的短链接没有参数也回了完整页面。样本太少，分不出 `scene`、请求间隔有没有作用；所以请求带上链接里的
//! `chksm`（见 `src/link.rs`），这一改动没有再对着平台验证。
//!
//! - 图片：`mmbiz.qpic.cn` 不带 Referer 就能下，响应的 Content-Type 都对（52 个文件里 51 个 JPEG、1 个 PNG，逐个 `file` 核对过）。

use std::time::Duration;

use url::Url;
use usuyo_plugin_kit::http::{self, Client, Hosts, Policy};
use usuyo_plugin_kit::protocol::{CallFailure, ErrorCode, NotFoundReason, PluginEvent};

use crate::link::HOST;

/// 普通桌面浏览器的 UA（2026-11-12 实测：这个 UA 不带 cookie 取文章页，回 200 和完整的页面）。
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
/// 文章页上限：实测 2.6–4.1 MB（页面自带几百 KB 的数据和几百 KB 的脚本）。
const PAGE_BYTES: u64 = 32 << 20;
const PAGE_TIMEOUT: Duration = Duration::from_secs(30);
/// 页面请求最多跟几跳，每一跳都要在 `mp.weixin.qq.com`。
const PAGE_REDIRECTS: u32 = 3;
const IMAGE_TIMEOUT: Duration = Duration::from_secs(30);
/// 文章图片只从这些域名的子域下载，和清单的 `network.hosts` 一致。
const IMAGE_HOST_SUFFIXES: [&str; 2] = [".qpic.cn", ".qlogo.cn"];

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("network: {0}")]
    Network(String),
    #[error("HTTP {status}")]
    Http {
        status: u16,
        retry_after: Option<u64>,
        /// 3xx 的 `Location`。
        location: Option<String>,
    },
    #[error("a response is larger than {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("the host `{0}` is not one of the allowed image hosts")]
    ForbiddenHost(String),
    #[error("the address is not an image address: {0}")]
    BadAddress(String),
    /// 连之前的检查没过：主机不在这种请求的名单里，或地址不是公网。
    #[error("refused to connect: {0}")]
    Refused(String),
    /// 平台回了，但不是能用的回答：带着回答的开头，宿主记进失败日志。
    #[error("{error}")]
    Answered {
        error: Box<ApiError>,
        answer: String,
    },
}

impl ApiError {
    /// 图片那一步要停下来报错的：限流。别的错（网络、状态码、太大、主机不在名单上）只记进 `missing`，正文照样交出去。
    pub fn is_hard(&self) -> bool {
        match self {
            ApiError::Http { status, .. } => *status == 429,
            ApiError::Answered { error, .. } => error.is_hard(),
            ApiError::Network(_)
            | ApiError::TooLarge { .. }
            | ApiError::ForbiddenHost(_)
            | ApiError::BadAddress(_)
            | ApiError::Refused(_) => false,
        }
    }

    fn answered(self, raw: &[u8]) -> ApiError {
        let end = raw.len().min(4096);
        ApiError::Answered {
            error: Box::new(self),
            answer: String::from_utf8_lossy(&raw[..end]).into_owned(),
        }
    }
}

impl From<ApiError> for CallFailure {
    fn from(err: ApiError) -> CallFailure {
        failure(&err)
    }
}

fn failure(err: &ApiError) -> CallFailure {
    let message = match err {
        ApiError::Http {
            status,
            location: Some(l),
            ..
        } => format!("HTTP {status} redirecting to {l}"),
        _ => err.to_string(),
    };
    match err {
        ApiError::Network(_) => CallFailure::new(ErrorCode::Network, message),
        ApiError::Http {
            status,
            retry_after,
            location,
        } => match status {
            403 | 412 => blocked(message),
            // 跳去验证页的算被拦；别的跳转是页面改了
            300..=399
                if location.as_deref().is_some_and(|l| {
                    let l = l.to_ascii_lowercase();
                    l.contains("captcha") || l.contains("verify")
                }) =>
            {
                blocked(message)
            }
            404 => CallFailure::new(ErrorCode::NotFound, message),
            410 => CallFailure {
                reason: Some(NotFoundReason::Deleted),
                ..CallFailure::new(ErrorCode::NotFound, message)
            },
            429 => CallFailure {
                retry_after: *retry_after,
                hint: Some("微信提示请求太频繁，请过一会儿再试".into()),
                ..CallFailure::new(ErrorCode::RateLimited, message)
            },
            500..=599 => CallFailure::new(ErrorCode::Network, message),
            _ => CallFailure::new(ErrorCode::UpstreamChanged, message),
        },
        ApiError::TooLarge { .. }
        | ApiError::ForbiddenHost(_)
        | ApiError::BadAddress(_)
        | ApiError::Refused(_) => CallFailure::new(ErrorCode::UpstreamChanged, message),
        ApiError::Answered { error, answer } => failure(error).with_detail(answer),
    }
}

pub fn blocked(message: String) -> CallFailure {
    CallFailure {
        hint: Some(
            "微信出现了验证页（环境异常）；匿名取文章太频繁时会这样，请稍后再试，也可以先在浏览器里打开这篇文章完成验证"
                .into(),
        ),
        ..CallFailure::new(ErrorCode::Blocked, message)
    }
}

/// 要连的主机和地址策略。正式运行用 `production`；测试把它指到本机的假服务器。
#[derive(Debug, Clone)]
pub struct Endpoints {
    page_base: Url,
    /// 测试用的本机主机：算页面主机，也算允许的图片主机，图片链接不升级成 https。
    local_host: Option<String>,
    policy: Policy,
}

impl Endpoints {
    pub fn production() -> Self {
        Self {
            page_base: Url::parse(&format!("https://{HOST}/")).expect("constant URL"),
            local_host: None,
            policy: Policy::from_host(),
        }
    }

    /// 测试用：`base` 是 `http://127.0.0.1:<端口>`，`policy` 让这个本机地址算公网。
    pub fn local(base: &str, policy: Policy) -> Self {
        let page_base = Url::parse(base).expect("local base URL");
        let local_host = page_base.host_str().map(str::to_owned);
        Self {
            page_base,
            local_host,
            policy,
        }
    }

    fn page_host(&self) -> &str {
        self.page_base.host_str().unwrap_or_default()
    }

    fn is_image_host(&self, host: &str) -> bool {
        self.local_host.as_deref() == Some(host)
            || IMAGE_HOST_SUFFIXES
                .iter()
                .any(|s| host.len() > s.len() && host.ends_with(s))
    }

    /// 页面给的图片地址是不是文章的图片：主机在名单上。名单外的（表情、图标）照样写进 Markdown，不下载、不算缺。
    pub fn is_article_image(&self, raw: &str) -> bool {
        Url::parse(raw)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .is_some_and(|h| self.is_image_host(&h))
    }

    fn client(&self, hosts: &[&str]) -> Client {
        let local = self.local_host.as_deref();
        Client::new(USER_AGENT, Hosts::only(hosts.iter().copied().chain(local)))
            .with_policy(self.policy)
    }

    /// 页面给的图片地址：补 scheme、升级成 https、核对主机和端口。
    pub fn image_url(&self, raw: &str) -> Result<Url, ApiError> {
        let raw = raw.trim();
        let full = if raw.starts_with("//") {
            format!("https:{raw}")
        } else {
            raw.to_owned()
        };
        let mut url = Url::parse(&full).map_err(|_| ApiError::BadAddress(raw.to_owned()))?;
        let host = url
            .host_str()
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if !self.is_image_host(&host) {
            return Err(ApiError::ForbiddenHost(host));
        }
        let local = self.local_host.as_deref() == Some(host.as_str());
        // 清单的 `network.hosts` 没写端口，只放行默认端口
        if !local && url.port().is_some() {
            return Err(ApiError::ForbiddenHost(format!(
                "{host}:{}",
                url.port().unwrap_or_default()
            )));
        }
        if url.scheme() == "http" && !local {
            url.set_scheme("https")
                .map_err(|()| ApiError::BadAddress(raw.to_owned()))?;
        }
        if url.scheme() != "https" && !local {
            return Err(ApiError::BadAddress(raw.to_owned()));
        }
        Ok(url)
    }
}

fn transport(e: http::Error) -> ApiError {
    match e {
        http::Error::TooLarge(limit) => ApiError::TooLarge { limit },
        e if e.is_refusal() => ApiError::Refused(e.to_string()),
        e => ApiError::Network(e.to_string()),
    }
}

fn status_error(response: &http::Response) -> ApiError {
    ApiError::Http {
        status: response.status(),
        retry_after: response
            .header("retry-after")
            .and_then(|v| v.trim().parse().ok()),
        location: response.header("location").map(str::to_owned),
    }
}

/// 取回的一页。
pub struct Fetched {
    pub status: u16,
    pub body: Vec<u8>,
}

pub struct Wechat {
    endpoints: Endpoints,
}

impl Wechat {
    pub fn new(endpoints: Endpoints) -> Self {
        Self { endpoints }
    }

    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// 取文章页。`canonical` 是规范链接，请求它的路径和查询（只含认得出这一篇的参数）。状态不是 2xx 就报错，带着回答的开头。
    pub fn article_page(&self, canonical: &str) -> Result<Fetched, ApiError> {
        let canonical =
            Url::parse(canonical).map_err(|_| ApiError::BadAddress(canonical.into()))?;
        let mut url = self.endpoints.page_base.clone();
        url.set_path(canonical.path());
        url.set_query(canonical.query());
        step("文章页");
        let mut response = self
            .endpoints
            .client(&[self.endpoints.page_host()])
            .get(&url)
            .header("Accept", "text/html,application/xhtml+xml")
            .header("Accept-Language", "zh-CN,zh;q=0.9")
            .follow_redirects(PAGE_REDIRECTS)
            .timeout(PAGE_TIMEOUT)
            .send()
            .map_err(transport)?;
        let status = response.status();
        if !(200..300).contains(&status) {
            let e = status_error(&response);
            let head = response.read_to_vec(4096).unwrap_or_default();
            return Err(e.answered(&head));
        }
        let body = response.read_to_vec(PAGE_BYTES).map_err(transport)?;
        Ok(Fetched { status, body })
    }

    /// 下载一张图到内存，最多 `limit` 字节。返回内容和响应的 `Content-Type`。不带 cookie、不带 Referer。
    pub fn image(&self, url: &Url, limit: u64) -> Result<(Vec<u8>, Option<String>), ApiError> {
        let mut response = self
            .endpoints
            .client(&IMAGE_HOST_SUFFIXES)
            .get(url)
            .header("Accept", "image/*")
            .follow_redirects(2)
            .timeout(IMAGE_TIMEOUT)
            .send()
            .map_err(transport)?;
        if !(200..300).contains(&response.status()) {
            return Err(status_error(&response));
        }
        let content_type = response.header("content-type").map(str::to_owned);
        let body = response.read_to_vec(limit).map_err(transport)?;
        Ok((body, content_type))
    }
}

/// 开始一步之前告诉宿主：一步一步慢下来时，宿主照样收得到 progress，不按「没进度」杀掉进程。
pub(crate) fn step(label: &str) {
    let event = PluginEvent::Progress {
        done: None,
        total: None,
        unit: None,
        message: Some(label.into()),
    };
    eprintln!("{}", event.stderr_line());
}

pub(crate) fn progress(done: usize, total: usize, label: &str) {
    let event = PluginEvent::Progress {
        done: Some(done as f64),
        total: Some(total as f64),
        unit: Some("张".into()),
        message: Some(label.into()),
    };
    eprintln!("{}", event.stderr_line());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_hosts() {
        let e = Endpoints::production();
        let ok = |s: &str| e.image_url(s).map(|u| u.to_string());
        assert_eq!(
            ok("http://mmbiz.qpic.cn/mmbiz_jpg/AAA/0?wx_fmt=jpeg").unwrap(),
            "https://mmbiz.qpic.cn/mmbiz_jpg/AAA/0?wx_fmt=jpeg"
        );
        assert!(ok("https://mmsns.qpic.cn/mmsns/x/0").is_ok());
        assert!(ok("//wx.qlogo.cn/mmhead/x/132").is_ok());
        assert!(matches!(
            ok("https://mmbiz.qpic.cn:8443/a"),
            Err(ApiError::ForbiddenHost(_))
        ));
        for bad in [
            "https://qpic.cn/a",
            "https://evilqpic.cn/a",
            "https://mmbiz.qpic.cn.evil.test/a",
            "https://res.wx.qq.com/a.png",
            "https://127.0.0.1/a",
        ] {
            assert!(matches!(ok(bad), Err(ApiError::ForbiddenHost(_))), "{bad}");
        }
        assert!(matches!(
            ok("ftp://mmbiz.qpic.cn/a"),
            Err(ApiError::BadAddress(_))
        ));
        assert!(matches!(ok("nonsense"), Err(ApiError::BadAddress(_))));
        assert!(e.is_article_image("https://mmbiz.qpic.cn/a"));
        assert!(!e.is_article_image("https://res.wx.qq.com/a.png"));
    }

    #[test]
    fn failure_mapping() {
        let http = |status, location: Option<&str>| {
            CallFailure::from(ApiError::Http {
                status,
                retry_after: Some(7),
                location: location.map(str::to_owned),
            })
        };
        assert_eq!(http(403, None).code, ErrorCode::Blocked);
        assert_eq!(http(412, None).code, ErrorCode::Blocked);
        assert_eq!(http(429, None).retry_after, Some(7));
        assert_eq!(http(502, None).code, ErrorCode::Network);
        assert_eq!(http(404, None).code, ErrorCode::NotFound);
        assert_eq!(http(410, None).reason, Some(NotFoundReason::Deleted));
        assert_eq!(
            http(302, Some("https://example.com/x")).code,
            ErrorCode::UpstreamChanged
        );
        assert_eq!(
            http(
                302,
                Some("https://mp.weixin.qq.com/mp/wappoc_appmsgcaptcha?x=1")
            )
            .code,
            ErrorCode::Blocked
        );
    }
}
