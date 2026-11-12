//! 文章正文的 HTML 换成 Markdown：标题、段落、列表、引用、链接、代码、表格，图片留在原处。
//!
//! 来源（MIT 许可证）：
//! - 移植 wechat-article-exporter `shared/utils/renderer.ts` 的 `renderContent_0` 对正文的处理：
//!   懒加载的 `<img data-src>` 换回真实地址（这里不是写回 `src`，而是直接取 `data-src`）。MIT License，Copyright (c) 2024 Jock。
//! - 对照 fengxxc/wechatmp2markdown 的元素对应（`code-snippet` 一行一个 `<code>`、`mpvoice`、视频 iframe）。
//!   MIT License，Copyright (c) 2021 fengxxc。
//! - 对照 z-mio/ParseHub `provider_api/weixin.py` 的 `WXConverter`：图片取 `data-src`，缺了才取 `src`。MIT License，Copyright (c) 2024 梓澪。
//!
//! 输出为了能被宿主原样当 Markdown 写进 `content.md`（SPEC §1.2.3）：正文里没有 `<`（文字里的 `<` 写成 `&lt;`），
//! 链接和图片的目标都是 `http(s)://` 或 `mailto:`。代码块里的 `<` 不改（改了就不是代码），含 `<` 的代码块会让宿主把
//! 整篇当纯文字转义，原样在 `raw/`。
//!
//! 图片的目标写页面给的原地址（`https://mmbiz.qpic.cn/…`），不写下载后的本地路径：相对路径过不了上面那条。
//! 下载下来的文件按同样的次序在输出的 `images` 里，对应关系记在 `meta.json`。

use scraper::{ElementRef, Node};

/// 换出来的正文，加上正文里按出现次序的东西。
#[derive(Debug, Default)]
pub struct Converted {
    pub markdown: String,
    /// 能下载的图片地址，按第一次出现的次序，去重。
    pub images: Vec<String>,
    /// 内嵌视频的页面地址（`<iframe>`）。
    pub videos: Vec<String>,
    pub audios: Vec<Audio>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Audio {
    pub name: String,
    pub media_id: String,
}

impl Audio {
    pub fn url(&self) -> String {
        format!(
            "https://res.wx.qq.com/voice/getvoice?mediaid={}",
            self.media_id
        )
    }
}

/// `is_article_image` 说哪些地址算文章的图片（要下载、记进 `images`）；别的图片地址照样写进 Markdown，不下载。
pub fn convert(root: ElementRef, is_article_image: impl Fn(&str) -> bool) -> Converted {
    let mut walker = Walker {
        is_article_image: &is_article_image,
        out: Converted::default(),
    };
    let mut blocks = Vec::new();
    walker.blocks(root, &mut blocks);
    walker.out.markdown = blocks.join("\n\n");
    walker.out
}

/// 一段纯文字（图片分享、文本分享的正文）：空行分段，段内的换行是硬换行；里面残留的标签（`<a>` 一类）当 HTML 读。
pub fn convert_text(text: &str, is_article_image: impl Fn(&str) -> bool) -> Converted {
    let mut walker = Walker {
        is_article_image: &is_article_image,
        out: Converted::default(),
    };
    let mut blocks = Vec::new();
    for paragraph in text.replace("\r\n", "\n").split("\n\n") {
        let html = scraper::Html::parse_fragment(&paragraph.trim().replace('\n', "<br>"));
        walker.blocks(html.root_element(), &mut blocks);
    }
    walker.out.markdown = blocks.join("\n\n");
    walker.out
}

struct Walker<'a> {
    is_article_image: &'a dyn Fn(&str) -> bool,
    out: Converted,
}

const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "svg", "template", "head", "link", "meta", "title", "canvas",
];

const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "header",
    "footer",
    "aside",
    "nav",
    "center",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "address",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
];

const MORE_BLOCKS: &[&str] = &["blockquote", "pre", "hr", "table", "li", "details"];

const INLINE: &[&str] = &[
    "span", "a", "strong", "b", "em", "i", "u", "s", "del", "strike", "code", "br", "img", "sup",
    "sub", "small", "font", "label", "mark", "abbr", "cite",
];

fn name<'a>(e: ElementRef<'a>) -> &'a str {
    e.value().name()
}

fn is_block(tag: &str) -> bool {
    BLOCKS.contains(&tag) || MORE_BLOCKS.contains(&tag)
}

fn hidden(e: ElementRef) -> bool {
    e.value().attr("style").is_some_and(|s| {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        s.contains("display:none")
    })
}

fn has_block(e: ElementRef) -> bool {
    e.descendants()
        .filter_map(ElementRef::wrap)
        .any(|d| d != e && (is_block(name(d)) || special(name(d))))
}

fn special(tag: &str) -> bool {
    matches!(tag, "iframe" | "mpvoice" | "mp-common-mpaudio")
}

impl Walker<'_> {
    /// `e` 的孩子换成若干块，追加到 `out`。
    fn blocks(&mut self, e: ElementRef, out: &mut Vec<String>) {
        let mut inline = String::new();
        for child in e.children() {
            match child.value() {
                Node::Text(t) => inline.push_str(&escape(t)),
                Node::Element(_) => {
                    let Some(c) = ElementRef::wrap(child) else {
                        continue;
                    };
                    let tag = name(c);
                    if SKIPPED.contains(&tag) || hidden(c) {
                        continue;
                    }
                    if INLINE.contains(&tag) && !has_block(c) {
                        inline.push_str(&self.inline(c));
                        continue;
                    }
                    flush(&mut inline, out);
                    self.block(c, out);
                }
                _ => {}
            }
        }
        flush(&mut inline, out);
    }

    fn block(&mut self, e: ElementRef, out: &mut Vec<String>) {
        let tag = name(e);
        match tag {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = usize::from(tag.as_bytes()[1] - b'0');
                let text = self.inline_all(e).replace(HARD_BREAK, " ");
                let text = text.trim();
                if !text.is_empty() {
                    out.push(format!("{} {text}", "#".repeat(level)));
                }
            }
            "ul" | "ol" => {
                let list = self.list(e, tag == "ol");
                if !list.is_empty() {
                    out.push(list);
                }
            }
            "blockquote" => {
                let mut inner = Vec::new();
                self.blocks(e, &mut inner);
                if !inner.is_empty() {
                    out.push(quote(&inner.join("\n\n")));
                }
            }
            "pre" => {
                if let Some(code) = fenced(e) {
                    out.push(code);
                }
            }
            "hr" => out.push("---".into()),
            "table" => {
                if let Some(table) = self.table(e) {
                    out.push(table);
                }
            }
            "iframe" => {
                let src = e
                    .value()
                    .attr("data-src")
                    .or_else(|| e.value().attr("src"))
                    .unwrap_or_default();
                if is_web_url(src) {
                    self.out.videos.push(src.to_owned());
                    out.push(format!("[视频]({})", link_target(src)));
                }
            }
            "mpvoice" | "mp-common-mpaudio" => {
                let id = e.value().attr("voice_encode_fileid").unwrap_or_default();
                if !id.is_empty() {
                    let audio = Audio {
                        name: e.value().attr("name").unwrap_or_default().trim().to_owned(),
                        media_id: id.to_owned(),
                    };
                    let label = if audio.name.is_empty() {
                        "音频".to_owned()
                    } else {
                        format!("音频：{}", escape(&audio.name).trim())
                    };
                    out.push(format!("[{label}]({})", audio.url()));
                    self.out.audios.push(audio);
                }
            }
            // 容器、`li` 单独出现、没认出的标签：只取里面的东西
            _ => self.blocks(e, out),
        }
    }

    /// `e` 的孩子换成一行里的文字（硬换行除外）。
    fn inline_all(&mut self, e: ElementRef) -> String {
        let mut s = String::new();
        for child in e.children() {
            match child.value() {
                Node::Text(t) => s.push_str(&escape(t)),
                Node::Element(_) => {
                    if let Some(c) = ElementRef::wrap(child)
                        && !SKIPPED.contains(&name(c))
                        && !hidden(c)
                    {
                        s.push_str(&self.inline(c));
                    }
                }
                _ => {}
            }
        }
        s
    }

    fn inline(&mut self, e: ElementRef) -> String {
        match name(e) {
            "br" => HARD_BREAK.to_owned(),
            "img" => self.image(e),
            "a" => {
                let text = self.inline_all(e).replace(HARD_BREAK, " ");
                match e.value().attr("href").map(str::trim) {
                    Some(href) if is_web_url(href) || href.starts_with("mailto:") => {
                        if text.trim().is_empty() {
                            String::new()
                        } else {
                            format!("[{}]({})", text.trim(), link_target(href))
                        }
                    }
                    _ => text,
                }
            }
            "strong" | "b" => wrap(&self.inline_all(e), "**"),
            "em" | "i" => wrap(&self.inline_all(e), "*"),
            "del" | "s" | "strike" => wrap(&self.inline_all(e), "~~"),
            "code" => {
                let text = plain_text(e);
                let text = text.trim();
                if text.is_empty() {
                    String::new()
                } else {
                    inline_code(text)
                }
            }
            _ => self.inline_all(e),
        }
    }

    fn image(&mut self, e: ElementRef) -> String {
        let v = e.value();
        let url = [v.attr("data-src"), v.attr("src")]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|u| is_web_url(u) || u.starts_with("//"));
        let Some(url) = url else {
            return String::new();
        };
        let url = decode_amp(&if url.starts_with("//") {
            format!("https:{url}")
        } else {
            url.to_owned()
        });
        if (self.is_article_image)(&url) && !self.out.images.contains(&url) {
            self.out.images.push(url.clone());
        }
        let alt = v.attr("alt").unwrap_or_default().replace(['[', ']'], "");
        format!("![{}]({})", alt.trim(), link_target(&url))
    }

    fn list(&mut self, e: ElementRef, ordered: bool) -> String {
        let start: usize = e
            .value()
            .attr("start")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let mut items = Vec::new();
        for li in e.children().filter_map(ElementRef::wrap) {
            if name(li) != "li" || hidden(li) {
                continue;
            }
            let mut inner = Vec::new();
            self.blocks(li, &mut inner);
            if inner.is_empty() {
                continue;
            }
            let marker = if ordered {
                format!("{}. ", start + items.len())
            } else {
                "- ".to_owned()
            };
            let pad = " ".repeat(marker.len());
            let body = inner.join("\n");
            let mut lines = body.lines();
            let mut item = format!("{marker}{}", lines.next().unwrap_or_default());
            for line in lines {
                item.push('\n');
                if !line.is_empty() {
                    item.push_str(&pad);
                    item.push_str(line);
                }
            }
            items.push(item);
        }
        items.join("\n")
    }

    fn table(&mut self, e: ElementRef) -> Option<String> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for tr in e
            .descendants()
            .filter_map(ElementRef::wrap)
            .filter(|d| name(*d) == "tr")
        {
            let cells: Vec<String> = tr
                .children()
                .filter_map(ElementRef::wrap)
                .filter(|c| matches!(name(*c), "td" | "th"))
                .map(|c| {
                    self.inline_all(c)
                        .replace(HARD_BREAK, " ")
                        .replace('|', "\\|")
                        .trim()
                        .to_owned()
                })
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
        let width = rows.iter().map(Vec::len).max()?;
        let line = |cells: &[String]| {
            let mut cells = cells.to_vec();
            cells.resize(width, String::new());
            format!("| {} |", cells.join(" | "))
        };
        let mut out = vec![line(&rows[0]), format!("|{}", " --- |".repeat(width))];
        out.extend(rows[1..].iter().map(|r| line(r)));
        Some(out.join("\n"))
    }
}

/// 行内的硬换行占位：`flush` 换成 Markdown 的硬换行。
const HARD_BREAK: &str = "\u{1}";

/// 一段行内文字收成一个块。空的不收。
fn flush(inline: &mut String, out: &mut Vec<String>) {
    let text = std::mem::take(inline);
    let mut lines: Vec<String> = text
        .split(HARD_BREAK)
        .map(|l| merge_bold(l.trim()))
        .map(|l| escape_line_start(&l))
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    if !lines.is_empty() {
        out.push(lines.join("  \n"));
    }
}

/// 相邻的两段加粗（`**a****b**`）合成一段。
fn merge_bold(line: &str) -> String {
    line.replace("****", "")
}

fn wrap(inner: &str, mark: &str) -> String {
    let trimmed = inner.trim();
    if trimmed.is_empty() || trimmed.contains(HARD_BREAK) {
        return inner.to_owned();
    }
    let lead = &inner[..inner.len() - inner.trim_start().len()];
    let trail = &inner[inner.trim_end().len()..];
    format!("{lead}{mark}{trimmed}{mark}{trail}")
}

fn quote(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                ">".to_owned()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 行首的 `#`、`>`、`-`、`+`、`1.` 会被当成块标记，前面加反斜杠。
fn escape_line_start(line: &str) -> String {
    let t = line.trim_start();
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && t[digits..]
            .strip_prefix(['.', ')'])
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    {
        return format!("{}\\{}", &t[..digits], &t[digits..]);
    }
    let hashes = t.chars().take_while(|&c| c == '#').count();
    let heading = (1..=6).contains(&hashes) && t[hashes..].starts_with([' ', '\u{1}']) || t == "#";
    let bullet =
        matches!(t.chars().next(), Some('-' | '+')) && (t.len() == 1 || t[1..].starts_with(' '));
    let rule = !t.is_empty() && (t.chars().all(|c| c == '=') || t.chars().all(|c| c == '-'));
    if heading || bullet || rule || t.starts_with('>') {
        return format!("\\{t}");
    }
    t.to_owned()
}

/// 文字里的空白收成一个空格，Markdown 的记号加反斜杠，`<` 写成实体。
fn escape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
            continue;
        }
        last_space = false;
        match c {
            '\\' | '`' | '*' | '[' | ']' => {
                out.push('\\');
                out.push(c);
            }
            // 两边都是字母数字的 `_`（`snake_case`）不当强调
            '_' => {
                let inside = i > 0
                    && chars[i - 1].is_alphanumeric()
                    && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
                if !inside {
                    out.push('\\');
                }
                out.push('_');
            }
            '~' if chars.get(i + 1) == Some(&'~') || (i > 0 && chars[i - 1] == '~') => {
                out.push_str("\\~");
            }
            '<' => out.push_str("&lt;"),
            _ => out.push(c),
        }
    }
    out
}

/// 代码块里的文字：`<br>` 是换行，不间断空格是空格，别的空白原样。
fn plain_text(e: ElementRef) -> String {
    let mut s = String::new();
    for node in e.descendants() {
        match node.value() {
            Node::Text(t) => s.push_str(&t.replace('\u{a0}', " ")),
            Node::Element(el) if el.name() == "br" => s.push('\n'),
            _ => {}
        }
    }
    s
}

/// `<pre>`：微信的代码片段一行一个 `<code>`；别的编辑器一个 `<code>` 里用 `<br>` 分行。
fn fenced(pre: ElementRef) -> Option<String> {
    let codes: Vec<ElementRef> = pre
        .children()
        .filter_map(ElementRef::wrap)
        .filter(|c| name(*c) == "code")
        .collect();
    let text = if codes.is_empty() {
        plain_text(pre)
    } else {
        codes
            .iter()
            .map(|c| plain_text(*c))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim_matches('\n');
    if text.trim().is_empty() {
        return None;
    }
    let lang = pre
        .value()
        .attr("data-lang")
        .or_else(|| codes.first().and_then(|c| c.value().attr("data-lang")))
        .map(|l| l.split_whitespace().next().unwrap_or_default())
        .filter(|l| {
            l.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-#._".contains(c))
        })
        .unwrap_or_default();
    let mut fence = "```".to_owned();
    while text.contains(&fence) {
        fence.push('`');
    }
    Some(format!("{fence}{lang}\n{text}\n{fence}"))
}

/// 行内代码：围栏比文字里最长的一串反引号多一个；文字以反引号开头或结尾时两边各垫一个空格
/// （CommonMark 的规则）。这样正文里的反引号关不掉围栏，后面的字不会被当成 Markdown。
fn inline_code(text: &str) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{text}{pad}{fence}")
}

fn is_web_url(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// 页面把地址里的 `&` 写成 `&amp;`（有时写两层）。
pub fn decode_amp(s: &str) -> String {
    let mut s = s.to_owned();
    while s.contains("&amp;") {
        s = s.replace("&amp;", "&");
    }
    s
}

/// 链接目标里空格、括号、尖括号会截断 Markdown 的解析，百分号编码。
fn link_target(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.trim().chars() {
        match c {
            ' ' => out.push_str("%20"),
            '(' => out.push_str("%28"),
            ')' => out.push_str("%29"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '\\' => out.push_str("%5C"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use scraper::Html;

    use super::*;

    fn md(html: &str) -> Converted {
        let doc = Html::parse_fragment(html);
        convert(doc.root_element(), |u| u.contains("qpic.cn"))
    }

    #[test]
    fn headings_paragraphs_emphasis() {
        let c = md(
            "<h2><span>标题</span></h2><p>前 <strong>粗</strong><strong>体</strong> 后<br>下一行</p><p> </p>",
        );
        assert_eq!(c.markdown, "## 标题\n\n前 **粗体** 后  \n下一行");
    }

    #[test]
    fn nested_sections_make_one_paragraph_per_block() {
        let c = md(
            "<section><section><p>甲</p></section><section><span>乙</span><span>丙</span></section></section>",
        );
        assert_eq!(c.markdown, "甲\n\n乙丙");
    }

    #[test]
    fn lists_quotes_links_code() {
        let c = md(
            "<ul><li><p>一</p><ul><li>一之一</li></ul></li><li>二</li></ul>\
             <ol start=\"3\"><li>三</li></ol>\
             <blockquote><p>引</p></blockquote>\
             <p><a href=\"https://example.com/a b(1)\">链接</a><a href=\"javascript:;\">无</a> <code>x_y</code></p>\
             <pre data-lang=\"rust\"><code>let a = 1;</code><code>if a &lt; 2 {}</code></pre>",
        );
        assert_eq!(
            c.markdown,
            "- 一\n  - 一之一\n- 二\n\n3. 三\n\n> 引\n\n[链接](https://example.com/a%20b%281%29)无 `x_y`\n\n```rust\nlet a = 1;\nif a < 2 {}\n```"
        );
    }

    #[test]
    fn lazy_images_come_back_in_place_and_in_order() {
        let c = md(
            "<p>a</p><p><img data-src=\"https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640?wx_fmt=jpeg&amp;from=appmsg\" src=\"data:image/gif;base64,xx\" alt=\"图[1]\"></p>\
             <p><img src=\"https://res.wx.qq.com/e.png\"><img data-src=\"https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640?wx_fmt=jpeg&amp;from=appmsg\"></p>\
             <img data-src=\"https://mmbiz.qpic.cn/mmbiz_png/BBB/0?wx_fmt=png\">",
        );
        assert_eq!(
            c.images,
            [
                "https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640?wx_fmt=jpeg&from=appmsg",
                "https://mmbiz.qpic.cn/mmbiz_png/BBB/0?wx_fmt=png",
            ]
        );
        assert!(c.markdown.starts_with(
            "a\n\n![图1](https://mmbiz.qpic.cn/mmbiz_jpg/AAA/640?wx_fmt=jpeg&from=appmsg)"
        ));
        assert!(c.markdown.contains("![](https://res.wx.qq.com/e.png)"));
    }

    #[test]
    fn text_is_escaped_and_has_no_angle_brackets() {
        let c = md("<p>1 &lt; 2 * 3 _x_ [y] snake_case # a</p><p># 不是标题</p><p>2. 不是列表</p>");
        assert_eq!(
            c.markdown,
            "1 &lt; 2 \\* 3 \\_x\\_ \\[y\\] snake_case # a\n\n\\# 不是标题\n\n2\\. 不是列表"
        );
        assert!(!c.markdown.contains('<'));
    }

    #[test]
    fn audio_video_and_hidden_things() {
        let c = md(
            "<mp-common-mpaudio name=\"一期\" voice_encode_fileid=\"MzIz\"></mp-common-mpaudio>\
             <iframe data-src=\"https://v.qq.com/iframe/preview.html?vid=v1\"></iframe>\
             <p style=\"display: none;\">藏</p><script>var a=1</script><p>见</p>",
        );
        assert_eq!(
            c.markdown,
            "[音频：一期](https://res.wx.qq.com/voice/getvoice?mediaid=MzIz)\n\n[视频](https://v.qq.com/iframe/preview.html?vid=v1)\n\n见"
        );
        assert_eq!(c.audios[0].media_id, "MzIz");
        assert_eq!(c.videos, ["https://v.qq.com/iframe/preview.html?vid=v1"]);
    }

    #[test]
    fn line_breaks_inside_headings_and_link_text_become_spaces() {
        let c = md("<h2>甲<br>乙</h2><p><a href=\"https://x.test/\">丙<br>丁</a></p>");
        assert_eq!(c.markdown, "## 甲 乙\n\n[丙 丁](https://x.test/)");
        assert!(!c.markdown.contains('\u{1}'));
    }

    #[test]
    fn tables() {
        let c = md("<table><tr><th>甲</th><th>乙</th></tr><tr><td>1|2</td><td>3</td></tr></table>");
        assert_eq!(c.markdown, "| 甲 | 乙 |\n| --- | --- |\n| 1\\|2 | 3 |");
    }

    #[test]
    fn inline_code_cannot_be_closed_by_its_own_text() {
        assert_eq!(inline_code("x_y"), "`x_y`");
        assert_eq!(inline_code("a`b"), "``a`b``");
        // 文字里有两个连着的反引号：两个的围栏会被它关掉，要三个
        assert_eq!(
            inline_code("a``b [点这里](https://example.com)"),
            "```a``b [点这里](https://example.com)```"
        );
        assert_eq!(inline_code("`x`"), "`` `x` ``");
    }

    #[test]
    fn plain_text_posts_keep_lines_and_paragraphs() {
        let c = convert_text(
            "第一行\n第二行\n\n第三段 <a class=\"wx_img_refer_link\" data-seq=\"1\">图1</a>\n",
            |_| false,
        );
        assert_eq!(c.markdown, "第一行  \n第二行\n\n第三段 图1");
    }
}
