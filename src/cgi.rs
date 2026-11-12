//! 读文章页里内嵌的数据 `window.cgiDataNew = {…}`：一段 JavaScript 对象字面量，不运行任何脚本。
//!
//! 来源（MIT 许可证）：
//! - 移植 wechat-article-exporter `shared/utils/html.ts` 的做法：找到 `window.cgiDataNew = {` 所在的脚本，取出这个对象。
//!   上游在 QuickJS 沙箱（或 iframe）里运行脚本取值，这里改成直接读字面量。MIT License，Copyright (c) 2024 Jock。
//! - 对照 z-mio/ParseHub `provider_api/weixin.py`：它去掉 `'0' * 1` 这类隐式类型转换，再按 JSON5 读。MIT License，Copyright (c) 2024 梓澪。
//!
//! 2026-11-12 对着真实的页面量过（三条公开文章链接各请求一次，不带 cookie）：
//! - 普通图文：对象里的字符串是 `'…\x3c…'` 这样的单引号字面量，数字写成 `'0' * 1`；
//! - 上游样例里较新的页面把字符串包在 `JsDecode('…')` 里，另有 `'true' === 'true' ? 1 : 0`、`"" * 1 || 0`、
//!   `[…].slice(0, 20)` 这类尾巴、重复的键和尾随逗号；
//! - 页面里另有别的脚本也提到 `cgiDataNew`、`nick_name`（例如广告位的 `'微信广告'`），所以只读这一个对象的顶层键。
//!
//! 读法：一个值读完它开头的字面量（字符串、数字、布尔、对象、数组、`JsDecode(…)`），其后直到同一层的下一个
//! `,`、`}`、`]` 之间的东西（`* 1`、三元式、`.slice(…)`）整段跳过。认不出开头的值记成空，不当作错误；
//! 括号对不上、字符串没有结尾才是读不出来。

use serde_json::{Map, Value};

const MARKER: &str = "window.cgiDataNew = {";

#[derive(Debug, thiserror::Error)]
pub enum CgiError {
    #[error("the page has no `window.cgiDataNew` object")]
    Missing,
    #[error("cannot read `window.cgiDataNew` at byte {0}: {1}")]
    Syntax(usize, &'static str),
}

/// 页面里的 `window.cgiDataNew` 对象。没有这个对象是 `Missing`。
pub fn parse(html: &str) -> Result<Map<String, Value>, CgiError> {
    let start = html.find(MARKER).ok_or(CgiError::Missing)? + MARKER.len() - 1;
    let mut reader = Reader {
        src: html,
        pos: start,
    };
    match reader.value()? {
        Value::Object(map) => Ok(map),
        _ => Err(CgiError::Syntax(start, "not an object")),
    }
}

struct Reader<'a> {
    src: &'a str,
    pos: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn rest(&self) -> &str {
        &self.src[self.pos..]
    }

    fn skip_space(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            if self.rest().starts_with("//") {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.bump();
                }
            } else if self.rest().starts_with("/*") {
                match self.rest().find("*/") {
                    Some(end) => self.pos += end + 2,
                    None => self.pos = self.src.len(),
                }
            } else {
                return;
            }
        }
    }

    fn error<T>(&self, what: &'static str) -> Result<T, CgiError> {
        Err(CgiError::Syntax(self.pos, what))
    }

    /// 一个值：开头的字面量，加上同一层里跟在它后面的尾巴。
    fn value(&mut self) -> Result<Value, CgiError> {
        self.skip_space();
        let value = match self.peek() {
            Some('{') => self.object()?,
            Some('[') => self.array()?,
            Some(q @ ('\'' | '"')) => Value::String(self.string(q)?),
            Some(c) if c.is_ascii_digit() || c == '-' => self.number(),
            Some(c) if c.is_alphabetic() || c == '_' || c == '$' => self.word()?,
            Some(_) => Value::Null,
            None => return self.error("unexpected end"),
        };
        self.skip_tail()?;
        Ok(value)
    }

    fn object(&mut self) -> Result<Value, CgiError> {
        self.bump();
        let mut map = Map::new();
        loop {
            self.skip_space();
            match self.peek() {
                Some('}') => {
                    self.bump();
                    return Ok(Value::Object(map));
                }
                Some(',') => {
                    self.bump();
                    continue;
                }
                None => return self.error("object is not closed"),
                _ => {}
            }
            let key = match self.peek() {
                Some(q @ ('\'' | '"')) => self.string(q)?,
                _ => self.ident(),
            };
            if key.is_empty() {
                return self.error("object key expected");
            }
            self.skip_space();
            if self.peek() != Some(':') {
                return self.error("`:` expected after an object key");
            }
            self.bump();
            let value = self.value()?;
            // 重复的键（`share_cover`）取第一个
            map.entry(key).or_insert(value);
        }
    }

    fn array(&mut self) -> Result<Value, CgiError> {
        self.bump();
        let mut items = Vec::new();
        loop {
            self.skip_space();
            match self.peek() {
                Some(']') => {
                    self.bump();
                    return Ok(Value::Array(items));
                }
                Some(',') => {
                    self.bump();
                    continue;
                }
                None => return self.error("array is not closed"),
                _ => {}
            }
            items.push(self.value()?);
        }
    }

    fn ident(&mut self) -> String {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            self.bump();
        }
        self.src[start..self.pos].to_owned()
    }

    fn number(&mut self) -> Value {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '.' | 'e' | 'E' | '+'))
        {
            self.bump();
        }
        serde_json::from_str(&self.src[start..self.pos]).unwrap_or(Value::Null)
    }

    /// `true`、`false`、`null`、`JsDecode(…)`；别的名字（`window.x`、函数调用）整个跳过，记成空。
    fn word(&mut self) -> Result<Value, CgiError> {
        let name = self.ident();
        match name.as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            "null" | "undefined" => Ok(Value::Null),
            "JsDecode" => {
                self.skip_space();
                if self.peek() != Some('(') {
                    return Ok(Value::Null);
                }
                self.bump();
                let inner = self.value()?;
                self.skip_space();
                if self.peek() == Some(')') {
                    self.bump();
                }
                Ok(match inner {
                    Value::String(s) => Value::String(js_decode(&s)),
                    other => other,
                })
            }
            _ => Ok(Value::Null),
        }
    }

    /// JavaScript 字符串字面量：`\xNN`、`\uNNNN`、`\u{…}`、常见转义和行续接。
    fn string(&mut self, quote: char) -> Result<String, CgiError> {
        self.bump();
        let mut out = String::new();
        loop {
            let Some(c) = self.bump() else {
                return self.error("string is not closed");
            };
            if c == quote {
                return Ok(out);
            }
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(e) = self.bump() else {
                return self.error("string is not closed");
            };
            match e {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'v' => out.push('\u{b}'),
                '0' => out.push('\0'),
                'x' => out.push(self.hex(2)?),
                'u' => {
                    if self.peek() == Some('{') {
                        self.bump();
                        let start = self.pos;
                        while self.peek().is_some_and(|c| c != '}') {
                            self.bump();
                        }
                        let code = u32::from_str_radix(&self.src[start..self.pos], 16)
                            .ok()
                            .and_then(char::from_u32);
                        self.bump();
                        out.push(code.unwrap_or('\u{fffd}'));
                    } else {
                        let first = self.hex_code(4)?;
                        if (0xd800..0xdc00).contains(&first) && self.rest().starts_with("\\u") {
                            self.pos += 2;
                            let second = self.hex_code(4)?;
                            let combined = 0x10000 + ((first - 0xd800) << 10) + (second & 0x3ff);
                            out.push(char::from_u32(combined).unwrap_or('\u{fffd}'));
                        } else {
                            out.push(char::from_u32(first).unwrap_or('\u{fffd}'));
                        }
                    }
                }
                '\r' => {
                    if self.peek() == Some('\n') {
                        self.bump();
                    }
                }
                '\n' => {}
                other => out.push(other),
            }
        }
    }

    fn hex_code(&mut self, digits: usize) -> Result<u32, CgiError> {
        let end = self
            .rest()
            .char_indices()
            .nth(digits)
            .map_or(self.src.len(), |(i, _)| self.pos + i);
        let code = u32::from_str_radix(&self.src[self.pos..end], 16)
            .or_else(|_| self.error("bad escape"))?;
        self.pos = end;
        Ok(code)
    }

    fn hex(&mut self, digits: usize) -> Result<char, CgiError> {
        Ok(char::from_u32(self.hex_code(digits)?).unwrap_or('\u{fffd}'))
    }

    /// 跳到同一层的下一个 `,`、`}`、`]`（不吃掉它）；中间的括号和字符串成对跳过。
    fn skip_tail(&mut self) -> Result<(), CgiError> {
        let mut depth = 0usize;
        loop {
            self.skip_space();
            match self.peek() {
                None => return Ok(()),
                Some(',' | '}' | ']') if depth == 0 => return Ok(()),
                Some('(' | '[' | '{') => {
                    depth += 1;
                    self.bump();
                }
                Some(')' | ']' | '}') => {
                    depth = depth.saturating_sub(1);
                    self.bump();
                }
                Some(q @ ('\'' | '"')) => {
                    self.string(q)?;
                }
                Some(_) => {
                    self.bump();
                }
            }
        }
    }
}

/// 页面里 `JsDecode` 函数做的事：把字面量里被双重转义的几个字符还原。
fn js_decode(s: &str) -> String {
    s.replace("\\x5c", "\\")
        .replace("\\x0d", "\r")
        .replace("\\x22", "\"")
        .replace("\\x26", "&")
        .replace("\\x27", "'")
        .replace("\\x3c", "<")
        .replace("\\x3e", ">")
        .replace("\\x0a", "\n")
}

/// 字段读成字符串：字符串原样，数字写成十进制。
pub fn text<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 字段读成整数：`'8' * 1` 读成字符串 `"8"` 或数字 8 都行。
pub fn int(map: &Map<String, Value>, key: &str) -> Option<i64> {
    match map.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(body: &str) -> String {
        format!("<script>var x = 1;\nwindow.cgiDataNew = {body};\n</script>")
    }

    #[test]
    fn plain_literals_and_tails() {
        let html = page(
            r#"{
                title: '周刊 #1\x3a \x22引号\x22 中文',
                item_show_type: '8' * 1,
                is_original: 'true' === 'true' ? 1 : 0,
                n: "" * 1 || 0,
                list: [ { cdn_url: 'https://a/0?wx_fmt=jpeg', }, ].slice(0, 20),
                share_cover: {x: 1},
                share_cover: {x: 2},
                emoji: '😀',
                after: 'ok',
            }"#,
        );
        let map = parse(&html).unwrap();
        assert_eq!(text(&map, "title"), Some("周刊 #1: \"引号\" 中文"));
        assert_eq!(int(&map, "item_show_type"), Some(8));
        assert_eq!(map["is_original"], "true");
        assert_eq!(map["list"][0]["cdn_url"], "https://a/0?wx_fmt=jpeg");
        assert_eq!(map["share_cover"]["x"], 1);
        assert_eq!(map["emoji"], "😀");
        assert_eq!(map["after"], "ok");
    }

    #[test]
    fn js_decode_wrapper_and_double_escapes() {
        let html = page(r"{ title: JsDecode('a\\x3cb\\x3e'), nick_name: JsDecode('名'), next: 1 }");
        let map = parse(&html).unwrap();
        assert_eq!(map["title"], "a<b>");
        assert_eq!(map["nick_name"], "名");
        assert_eq!(map["next"], 1);
    }

    #[test]
    fn braces_inside_strings_do_not_end_the_object() {
        let html = page(r"{ a: '}; window.cgiDataNew = {', b: [']'], c: 'x' }");
        let map = parse(&html).unwrap();
        assert_eq!(map["a"], "}; window.cgiDataNew = {");
        assert_eq!(map["c"], "x");
    }

    #[test]
    fn unreadable_values_become_null_and_broken_objects_fail() {
        let map = parse(&page("{ a: window.foo, b: fn(1, {x: ')'}), c: 3 }")).unwrap();
        assert_eq!(
            (&map["a"], &map["b"], &map["c"]),
            (&Value::Null, &Value::Null, &Value::from(3))
        );
        assert!(matches!(parse(&page("{ a: 'x")), Err(CgiError::Syntax(..))));
        assert!(matches!(parse("<html></html>"), Err(CgiError::Missing)));
    }
}
