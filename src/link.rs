//! 认链接：一条公众号文章链接是哪一篇。不发请求。
//!
//! 认的两种形状（主机 `mp.weixin.qq.com`，http 或 https）：
//! - `/s/<短 id>`：App 里「复制链接」给的那种；
//! - `/s?__biz=…&mid=…&idx=…&sn=…`：长链接。身份和规范链接只留认得出这一篇的四个参数（`sn` 缺了也认），
//!   `chksm`、`scene`、`srcid`、`#wechat_redirect` 一类的参数和片段都去掉。`__biz` 在链接里常被百分号编码
//!   （`%3D%3D`），读出来是解码后的，规范链接里再编码一次，同一篇只有一种写法。
//!   请求页面时例外：链接里带 `chksm`（文章的校验串）就一并带上（[`Article::request_url`]）。2026-11-12 经宿主实测：
//!   不带 `chksm` 的长链接请求了两次（插件一次，通用网页插件一次），两次都回验证页；同一条链接带着用户给的 `chksm`
//!   （值是假的）和 `scene` 请求了一次，回了完整的页面。这只有三个样本，`scene` 有没有作用分不出来。
//!
//! 身份（2026-11-12 对着真实页面量过）：
//! - 短链接：`contentId` 是短 id 本身；
//! - 长链接：`contentId` 是 `<__biz>:<mid>:<idx>`。
//!
//! 同一篇的两种链接给两个身份，`canonicalize` 无法把它们并起来：它只看链接，只有展开短链才许发请求，
//! 而公众号没有要展开的短链。文章页自己内嵌的数据里有 `bizuin`、`mid`、`idx`、`sn`（取 `/s/<id>` 时量到了），
//! 所以取回短链接的页面以后能写出长链接的身份；但取长链接页面时，页面里的 `link` 仍是长链接的写法
//! （上游样例 `c01`–`c05` 就是），拿不到短 id。所以两个方向不对称，不能用页面把两个身份并起来；
//! 页面里读到的另一种写法记进 `meta.json`，以后要并时有现成的数据。

use url::Url;
use usuyo_plugin_kit::protocol::{CallFailure, Canonicalized, ErrorCode, Identity, PlatformId};

pub const PLATFORM: &str = "wechat";
pub const HOST: &str = "mp.weixin.qq.com";

/// 短 id 的长度范围：真实的是 22 个字符（Base64 的网址安全写法），留足余量。
const SHORT_ID_LEN: std::ops::RangeInclusive<usize> = 16..=64;
/// 粘贴的文字最多看这么多字节。
const MAX_TEXT: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Article {
    /// `/s/<id>`
    Short { id: String },
    /// `/s?__biz=…&mid=…&idx=…&sn=…`
    Long {
        biz: String,
        mid: String,
        idx: String,
        sn: Option<String>,
        /// 只用于请求页面，不进身份和规范链接。
        chksm: Option<String>,
    },
}

impl Article {
    pub fn content_id(&self) -> String {
        match self {
            Article::Short { id } => id.clone(),
            Article::Long { biz, mid, idx, .. } => format!("{biz}:{mid}:{idx}"),
        }
    }

    /// 规范链接，也是取页面时请求的路径和查询（只含认得出这一篇的参数）。
    pub fn canonical_url(&self) -> String {
        match self {
            Article::Short { id } => format!("https://{HOST}/s/{id}"),
            Article::Long {
                biz, mid, idx, sn, ..
            } => long_url(biz, mid, idx, sn.as_deref(), None),
        }
    }

    /// 取页面时请求的链接：规范链接，长链接带着 `chksm`（有的话）。
    pub fn request_url(&self) -> String {
        match self {
            Article::Long {
                biz,
                mid,
                idx,
                sn,
                chksm,
            } => long_url(biz, mid, idx, sn.as_deref(), chksm.as_deref()),
            Article::Short { .. } => self.canonical_url(),
        }
    }

    pub fn identity(&self) -> Result<Identity, CallFailure> {
        let platform =
            PlatformId::try_from(PLATFORM).map_err(|e| CallFailure::new(ErrorCode::Internal, e))?;
        Ok(Identity::Platform {
            platform,
            content_id: self.content_id(),
            url: Some(self.canonical_url()),
        })
    }
}

/// 输入是一条链接，或带链接的分享文字。`None` 是「不是公众号文章的链接」，不是错误。
pub fn parse_input(input: &str) -> Option<Article> {
    let url = find_url(input)?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if url.host_str()?.to_ascii_lowercase() != HOST {
        return None;
    }
    let segments: Vec<&str> = url.path().trim_matches('/').split('/').collect();
    match segments.as_slice() {
        ["s", id] if is_short_id(id) => Some(Article::Short {
            id: (*id).to_owned(),
        }),
        ["s"] => long(&url),
        _ => None,
    }
}

fn long_url(biz: &str, mid: &str, idx: &str, sn: Option<&str>, chksm: Option<&str>) -> String {
    let mut url = Url::parse(&format!("https://{HOST}/s")).expect("constant URL");
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("__biz", biz)
            .append_pair("mid", mid)
            .append_pair("idx", idx);
        if let Some(sn) = sn {
            query.append_pair("sn", sn);
        }
        if let Some(chksm) = chksm {
            query.append_pair("chksm", chksm);
        }
    }
    url.into()
}

fn long(url: &Url) -> Option<Article> {
    let get = |key: &str| {
        url.query_pairs()
            // 链接被当成 HTML 复制过（`&amp;mid=`）时，键带着 `amp;`
            .find(|(k, _)| k.trim_start_matches("amp;") == key)
            .map(|(_, v)| v.trim().to_owned())
    };
    let biz = get("__biz").filter(|b| is_biz(b))?;
    let mid = get("mid").filter(|m| is_number(m))?;
    let idx = get("idx").filter(|i| is_number(i))?;
    let token = |s: &String| s.len() <= 128 && s.chars().all(|c| c.is_ascii_alphanumeric());
    let sn = get("sn").filter(token);
    let chksm = get("chksm").filter(token);
    Some(Article::Long {
        biz,
        mid,
        idx,
        sn,
        chksm,
    })
}

fn is_short_id(s: &str) -> bool {
    SHORT_ID_LEN.contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_biz(s: &str) -> bool {
    (4..=64).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
}

fn is_number(s: &str) -> bool {
    (1..=20).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit())
}

/// 整个输入是一条链接就用它；不然取文字里第一个 `http(s)://` 起到空白或非 ASCII 字符为止的一段。
fn find_url(input: &str) -> Option<Url> {
    let end = (0..=MAX_TEXT.min(input.len()))
        .rev()
        .find(|&i| input.is_char_boundary(i))?;
    let text = input[..end].trim().replace("&amp;", "&");
    if let Ok(url) = Url::parse(&text)
        && matches!(url.scheme(), "http" | "https")
    {
        return Some(url);
    }
    let start = ["https://", "http://"]
        .iter()
        .filter_map(|p| text.find(p))
        .min()?;
    let rest = &text[start..];
    let stop = rest
        .find(|c: char| !c.is_ascii_graphic() || matches!(c, '"' | '\'' | '<' | '>'))
        .unwrap_or(rest.len());
    let candidate = rest[..stop].trim_end_matches(['.', ',', ';', ')', ']', '!']);
    Url::parse(candidate).ok()
}

pub fn canonicalize(input: &str) -> Result<Canonicalized, CallFailure> {
    Ok(match parse_input(input) {
        Some(article) => Canonicalized::Match {
            identity: article.identity()?,
            resolved_url: None,
        },
        None => Canonicalized::NoMatch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "tr38EieqyftubIx2FMWUWw";

    #[test]
    fn short_links_drop_tracking() {
        for input in [
            format!("https://mp.weixin.qq.com/s/{ID}"),
            format!("http://mp.weixin.qq.com/s/{ID}?scene=1&chksm=ab#wechat_redirect"),
            format!("https://MP.WEIXIN.QQ.COM/s/{ID}/"),
            format!("文章《标题》 https://mp.weixin.qq.com/s/{ID}，复制链接"),
        ] {
            let a = parse_input(&input).unwrap_or_else(|| panic!("{input}"));
            assert_eq!(a.content_id(), ID, "{input}");
            assert_eq!(
                a.canonical_url(),
                format!("https://mp.weixin.qq.com/s/{ID}")
            );
        }
    }

    #[test]
    fn long_links_keep_the_four_identifying_parameters() {
        let want = "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw%3D%3D&mid=2650242275&idx=1&sn=522b4a6728fcde7b55e73a5568e2c028";
        for input in [
            "http://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&mid=2650242275&idx=1&sn=522b4a6728fcde7b55e73a5568e2c028&chksm=f3e0&scene=21#wechat_redirect",
            "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw%3D%3D&mid=2650242275&idx=1&sn=522b4a6728fcde7b55e73a5568e2c028",
            "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&amp;mid=2650242275&amp;idx=1&amp;sn=522b4a6728fcde7b55e73a5568e2c028",
        ] {
            let a = parse_input(input).unwrap_or_else(|| panic!("{input}"));
            assert_eq!(a.content_id(), "MzI4NjAxNjY4Nw==:2650242275:1", "{input}");
            assert_eq!(a.canonical_url(), want, "{input}");
        }
    }

    #[test]
    fn the_request_keeps_chksm_but_the_identity_does_not() {
        let with = parse_input(
            "http://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&mid=2650242275&idx=1&sn=522b&chksm=f3e0db37&scene=21#wechat_redirect",
        )
        .unwrap();
        assert_eq!(
            with.request_url(),
            "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw%3D%3D&mid=2650242275&idx=1&sn=522b&chksm=f3e0db37"
        );
        assert!(!with.canonical_url().contains("chksm"));
        let without = parse_input(
            "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&mid=2650242275&idx=1&sn=522b",
        )
        .unwrap();
        assert_eq!(without.request_url(), without.canonical_url());
        assert_eq!(with.content_id(), without.content_id());
        let short =
            parse_input("https://mp.weixin.qq.com/s/tr38EieqyftubIx2FMWUWw?chksm=ab").unwrap();
        assert_eq!(
            short.request_url(),
            "https://mp.weixin.qq.com/s/tr38EieqyftubIx2FMWUWw"
        );
    }

    #[test]
    fn the_two_forms_of_one_article_are_two_identities() {
        let short = parse_input(&format!("https://mp.weixin.qq.com/s/{ID}")).unwrap();
        let long = parse_input(
            "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&mid=2650242275&idx=1&sn=522b4a6728fcde7b55e73a5568e2c028",
        )
        .unwrap();
        assert_ne!(short.content_id(), long.content_id());
    }

    #[test]
    fn what_is_not_an_article_is_turned_away() {
        for input in [
            "https://example.com/s/tr38EieqyftubIx2FMWUWw",
            "https://mp.weixin.qq.com/s/short",
            "https://mp.weixin.qq.com/s/tr38EieqyftubIx2FMWUWw/extra",
            "https://mp.weixin.qq.com/mp/profile_ext?action=home&__biz=MzI4NjAxNjY4Nw==",
            "https://mp.weixin.qq.com/s?mid=1&idx=1",
            "https://mp.weixin.qq.com/s?__biz=MzI4NjAxNjY4Nw==&mid=abc&idx=1",
            "https://mp.weixin.qq.com.evil.test/s/tr38EieqyftubIx2FMWUWw",
            "ftp://mp.weixin.qq.com/s/tr38EieqyftubIx2FMWUWw",
            "nonsense",
            "",
        ] {
            assert_eq!(parse_input(input), None, "{input}");
        }
    }
}
