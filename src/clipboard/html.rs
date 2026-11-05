//! CF_HTML（注册格式 "HTML Format"）解析/生成，以及 HTML/RTF 清洗。
//!
//! 设计要点（spec §12.1 / §12.2，B1 富文本工单）：
//! - CF_HTML 的 header 里 `StartHTML/EndHTML/StartFragment/EndFragment` 都是
//!   **UTF-8 字节偏移**（不是字符偏移），本模块统一按字节切片，切片前校验字符边界；
//! - 生成 CF_HTML 时偏移用固定 10 位补零，header 长度恒定，末尾补一个 `\0`；
//! - 清洗 HTML 不引入第三方解析器：按标签扫描，丢弃 `<script>/<style>/<iframe>/<object>`
//!   等危险标签（含内容），过滤 `on*` 事件属性、`javascript:`/`vbscript:` 协议、
//!   外部 http(s) 图片与链接；保留 `data:` 图片与文本结构；
//! - RTF 只做最基本的控制字过滤（去掉内嵌对象/图片组），不做完整解析；
//! - 所有函数都不 panic：输入损坏时返回 `None`，由调用方降级为纯文本行为。

/// 注册剪贴板格式名：HTML Format（CF_HTML，UTF-8）。
pub(crate) const HTML_FORMAT: &str = "HTML Format";
/// 注册剪贴板格式名：Rich Text Format（RTF，ASCII/ANSI 字节流）。
pub(crate) const RTF_FORMAT: &str = "Rich Text Format";

/// 读取 / 存储 HTML 的硬上限（字节）。超出部分按字符边界截断。
pub(crate) const MAX_HTML_BYTES: usize = 2 * 1024 * 1024;
/// 读取 / 存储 RTF 的硬上限（字节）。
pub(crate) const MAX_RTF_BYTES: usize = 2 * 1024 * 1024;
/// 从 HTML 提取纯文本回退时的上限（与 preview 量级一致）。
const MAX_PLAIN_BYTES: usize = 8 * 1024;

/// 危险标签：整段（含内容）丢弃。
const DROP_WITH_CONTENT: &[&str] = &[
    "script",
    "style",
    "iframe",
    "object",
    "applet",
    "frameset",
    "noscript",
    "template",
];
/// 只丢弃标签本身、保留其中文本的标签。
const DROP_TAG_ONLY: &[&str] = &[
    "embed", "frame", "link", "meta", "base", "form", "input", "button", "select", "textarea",
    "option", "param",
];
/// HTML void 元素（没有闭合标签）。
const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

// ---------------------------------------------------------------------------
// CF_HTML 解析
// ---------------------------------------------------------------------------

/// CF_HTML header 中的偏移字段（字节偏移，-1 / 缺失记为 None）。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CfHtmlHeader {
    pub start_html: Option<usize>,
    pub end_html: Option<usize>,
    pub start_fragment: Option<usize>,
    pub end_fragment: Option<usize>,
}

/// 解析 CF_HTML 数据，取出 fragment 片段；无法解析时返回 `None`。
///
/// 兼容策略：
/// 1. 优先用 `StartFragment/EndFragment` 字节偏移（校验范围与 UTF-8 字符边界）；
/// 2. 偏移损坏（部分程序按字符/UTF-16 计偏移）时退化为查找 `<!--StartFragment-->` 标记；
/// 3. 再退化为 `StartHTML/EndHTML` 的整段文档；
/// 4. 最后把全部内容当作 HTML 片段（交给 sanitizer 兜底）。
pub(crate) fn parse_cf_html(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    // CF_HTML 数据通常以 NUL 结尾；header 偏移不含这个 NUL
    let text = text.trim_end_matches('\0');
    if text.trim().is_empty() {
        return None;
    }
    let header = parse_cf_html_header(text);

    // 1) fragment 字节偏移
    if let (Some(start), Some(end)) = (header.start_fragment, header.end_fragment) {
        if let Some(fragment) = byte_range(text, start, end) {
            let fragment = fragment.trim_matches(['\r', '\n']);
            if !fragment.trim().is_empty() {
                return Some(fragment.to_string());
            }
        }
    }

    // 2) 标记回退
    if let Some(fragment) = fragment_between_markers(text) {
        return Some(fragment);
    }

    // 3) 整段文档
    if let (Some(start), Some(end)) = (header.start_html, header.end_html) {
        if let Some(html) = byte_range(text, start, end) {
            if !html.trim().is_empty() {
                return Some(html.to_string());
            }
        }
    }

    // 4) 全部内容
    let all = text.trim();
    if all.is_empty() {
        None
    } else {
        Some(all.to_string())
    }
}

/// 解析 header 行：`Version:`、`StartHTML:`、`EndHTML:`、`StartFragment:`、`EndFragment:`。
///
/// 只扫描开头最多 4KB，遇到非 `key:value` 行即停止，避免把正文误当 header。
pub(crate) fn parse_cf_html_header(text: &str) -> CfHtmlHeader {
    let mut header = CfHtmlHeader::default();
    let limit = text.len().min(4096);
    let head = &text[..limit];
    for line in head.split_inclusive(['\r', '\n']) {
        let line = line.trim_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            break;
        };
        let key = key.trim_start_matches('\u{feff}').trim();
        let parsed = value.trim().parse::<i64>().ok().filter(|&v| v >= 0);
        let slot = match key.to_ascii_lowercase().as_str() {
            "starthtml" => &mut header.start_html,
            "endhtml" => &mut header.end_html,
            "startfragment" => &mut header.start_fragment,
            "endfragment" => &mut header.end_fragment,
            "version" | "startselection" | "endselection" | "sourceurl" => continue,
            _ => break,
        };
        // 偏移超过 usize 范围视为无效
        if let Some(v) = parsed {
            *slot = usize::try_from(v).ok();
        }
    }
    header
}

/// 按字节区间切片，并保证落在 UTF-8 字符边界上（偏移偏 1~3 字节时自动修正）。
fn byte_range(text: &str, start: usize, end: usize) -> Option<&str> {
    if start >= end || end > text.len() {
        return None;
    }
    let mut s = start;
    while s < end && !text.is_char_boundary(s) {
        s += 1;
    }
    let mut e = end;
    while e > s && !text.is_char_boundary(e) {
        e -= 1;
    }
    if s >= e {
        return None;
    }
    Some(&text[s..e])
}

/// `<!--StartFragment-->` / `<!--EndFragment-->` 之间的内容（大小写不敏感）。
fn fragment_between_markers(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start_marker = "<!--startfragment-->";
    let end_marker = "<!--endfragment-->";
    let start = lower.find(start_marker)? + start_marker.len();
    let end = lower[start..].find(end_marker)? + start;
    let fragment = text[start..end].trim_matches(['\r', '\n']);
    if fragment.trim().is_empty() {
        None
    } else {
        Some(fragment.to_string())
    }
}

// ---------------------------------------------------------------------------
// CF_HTML 生成
// ---------------------------------------------------------------------------

/// 生成合法的 CF_HTML：header + `<html><body><!--StartFragment-->…` + 末尾 NUL。
///
/// 所有偏移都是 UTF-8 字节偏移，固定 10 位补零（因此 header 长度恒定，可先算长度）。
pub(crate) fn build_cf_html(fragment: &str) -> Vec<u8> {
    const HTML_PREFIX: &str = "<html><body><!--StartFragment-->";
    const HTML_SUFFIX: &str = "<!--EndFragment--></body></html>";

    let header = |start_html: usize, end_html: usize, start_frag: usize, end_frag: usize| {
        format!(
            "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
             StartFragment:{start_frag:010}\r\nEndFragment:{end_frag:010}\r\n"
        )
    };
    // 先按 0 生成一次拿 header 的固定长度，再代入真实偏移重新生成
    let header_len = header(0, 0, 0, 0).len();
    let start_html = header_len;
    let start_fragment = start_html + HTML_PREFIX.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + HTML_SUFFIX.len();

    let mut out = Vec::with_capacity(end_html + 1);
    out.extend_from_slice(
        header(start_html, end_html, start_fragment, end_fragment).as_bytes(),
    );
    out.extend_from_slice(HTML_PREFIX.as_bytes());
    out.extend_from_slice(fragment.as_bytes());
    out.extend_from_slice(HTML_SUFFIX.as_bytes());
    // CF_HTML 约定末尾 NUL（不计入 EndHTML 偏移）
    out.push(0);
    out
}

// ---------------------------------------------------------------------------
// HTML 清洗
// ---------------------------------------------------------------------------

/// 清洗 HTML fragment：丢弃脚本/事件/外链等危险内容，保留文本结构与 `data:` 图片。
///
/// 返回 `None` 表示清洗后没有任何有效内容（调用方应降级为纯文本行为）。
/// 输出长度不超过 [`MAX_HTML_BYTES`]（按字符边界截断）。
pub(crate) fn sanitize_html(input: &str) -> Option<String> {
    let input = input.trim_end_matches('\0');
    if input.trim().is_empty() {
        return None;
    }
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len().min(MAX_HTML_BYTES));
    let mut i = 0usize;
    let mut text_start = 0usize;
    let mut truncated = false;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // 先落地文本段
        if i > text_start {
            let chunk = &input[text_start..i];
            if !push_capped(&mut out, chunk) {
                truncated = true;
                break;
            }
        }

        // 注释整段丢弃
        if bytes[i..].starts_with(b"<!--") {
            i = match find_sub(bytes, i + 4, b"-->") {
                Some(pos) => pos + 3,
                None => bytes.len(),
            };
            text_start = i;
            continue;
        }
        // <!DOCTYPE ...> / <?xml ...>
        if bytes[i..].starts_with(b"<!") || bytes[i..].starts_with(b"<?") {
            i = match find_byte(bytes, i + 2, b'>') {
                Some(pos) => pos + 1,
                None => bytes.len(),
            };
            text_start = i;
            continue;
        }
        // 闭合标签
        if bytes[i..].starts_with(b"</") {
            let (name, after) = parse_tag_name(bytes, i + 2);
            i = after;
            text_start = i;
            if let Some(name) = name {
                if !is_dropped_tag(&name) && !DROP_WITH_CONTENT.contains(&name.as_str()) {
                    if !push_capped(&mut out, &format!("</{name}>")) {
                        truncated = true;
                        break;
                    }
                }
            }
            continue;
        }

        // 开始标签
        let Some(tag_end) = find_tag_end(bytes, i + 1) else {
            // 不完整的 `<`：当普通文本保留
            if !push_capped(&mut out, "<") {
                truncated = true;
            }
            i += 1;
            text_start = i;
            continue;
        };
        let raw = &input[i + 1..tag_end];
        let self_closing = raw.trim_end().ends_with('/');
        let raw = raw.trim_end().trim_end_matches('/');
        let (name, attrs) = match split_tag(raw) {
            Some(parts) => parts,
            None => {
                i = tag_end + 1;
                text_start = i;
                continue;
            }
        };
        i = tag_end + 1;
        text_start = i;

        if name.is_empty() {
            continue;
        }
        // 危险标签：整段（含内容）跳过
        if DROP_WITH_CONTENT.contains(&name.as_str()) {
            i = skip_element_content(input, bytes, i, &name);
            text_start = i;
            continue;
        }
        if DROP_TAG_ONLY.contains(&name.as_str()) {
            continue;
        }

        // 属性过滤
        let attrs = filter_attrs(&name, attrs);
        // 外部图片：整张图丢弃（不加载外链资源）
        if name == "img" && attrs.dropped_external_src {
            continue;
        }
        let mut tag = String::with_capacity(name.len() + 16);
        tag.push('<');
        tag.push_str(&name);
        for (attr_name, value) in &attrs.attrs {
            tag.push(' ');
            tag.push_str(attr_name);
            if let Some(value) = value {
                tag.push_str("=\"");
                tag.push_str(&escape_attr(value));
                tag.push('"');
            }
        }
        tag.push('>');
        if !push_capped(&mut out, &tag) {
            truncated = true;
            break;
        }
        let _ = self_closing;
    }

    let _ = truncated;
    // 末尾没有 `<` 的剩余文本段
    if text_start < bytes.len() {
        let _ = push_capped(&mut out, &input[text_start..]);
    }
    let out = out.trim();
    if out.is_empty() {
        return None;
    }
    Some(out.to_string())
}

/// 属性过滤结果。
struct FilteredAttrs {
    attrs: Vec<(String, Option<String>)>,
    /// `<img>` 的 src 是外部 http(s) 资源 → 调用方应整张图丢弃。
    dropped_external_src: bool,
}

/// 过滤单个标签的属性：去掉 `on*` 事件、脚本协议、外部链接。
fn filter_attrs(tag: &str, attrs: Vec<(String, Option<String>)>) -> FilteredAttrs {
    let mut kept = Vec::with_capacity(attrs.len());
    let mut dropped_external_src = false;
    for (name, value) in attrs {
        // on* 事件属性一律丢弃
        if name.starts_with("on") {
            continue;
        }
        if name == "srcdoc" {
            continue;
        }
        let Some(value) = value else {
            kept.push((name, None));
            continue;
        };
        let probe = value.trim().trim_start_matches(['\u{0}', ' ', '\t', '\r', '\n']);
        let lower = probe.to_ascii_lowercase();
        // 脚本协议
        if lower.starts_with("javascript:") || lower.starts_with("vbscript:") {
            continue;
        }
        // data:text/html 可执行脚本，直接丢弃
        if lower.starts_with("data:text/html") {
            continue;
        }
        // 内联样式里藏脚本
        if name == "style"
            && (lower.contains("javascript:")
                || lower.contains("vbscript:")
                || lower.contains("expression("))
        {
            continue;
        }
        // 外链资源：http(s) / 协议相对 URL
        let external = lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("//");
        if external {
            let resource_attr = matches!(
                name.as_str(),
                "src" | "href" | "xlink:href" | "background" | "poster" | "action" | "formaction"
            );
            if resource_attr {
                if tag == "img" && name == "src" {
                    dropped_external_src = true;
                }
                // 外部链接只删地址，保留锚文本/结构
                continue;
            }
        }
        kept.push((name, Some(value)));
    }
    FilteredAttrs {
        attrs: kept,
        dropped_external_src,
    }
}

/// 是否属于"丢弃标签本身"（保留内容）的闭合标签。
fn is_dropped_tag(name: &str) -> bool {
    DROP_TAG_ONLY.contains(&name) || DROP_WITH_CONTENT.contains(&name)
}

/// 跳过危险元素的内容：从 `from`（开始标签之后）找 `</name ...>`，返回其后的位置。
fn skip_element_content(input: &str, bytes: &[u8], from: usize, name: &str) -> usize {
    // 大小写不敏感查找闭合标签起始
    let lower = input.to_ascii_lowercase();
    let needle = format!("</{name}");
    let mut search = from;
    while let Some(pos) = lower[search..].find(&needle) {
        let abs = search + pos;
        // 确认后面是空白或 '>'
        let after = abs + needle.len();
        match bytes.get(after) {
            Some(b) if b.is_ascii_whitespace() || *b == b'>' => {
                return match find_byte(bytes, after, b'>') {
                    Some(gt) => gt + 1,
                    None => bytes.len(),
                };
            }
            _ => search = abs + 1,
        }
    }
    bytes.len()
}

/// 解析开始标签的标签名与小写属性列表。
fn split_tag(raw: &str) -> Option<(String, Vec<(String, Option<String>)>)> {
    let raw = raw.trim_start();
    if raw.is_empty() {
        return None;
    }
    let name_end = raw
        .find(|c: char| c.is_ascii_whitespace())
        .unwrap_or(raw.len());
    let name = raw[..name_end].to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':') {
        return None;
    }
    let attrs = parse_attrs(&raw[name_end..]);
    Some((name, attrs))
}

/// 解析属性串（`key` / `key=value` / `key="value"` / `key='value'`）。
fn parse_attrs(s: &str) -> Vec<(String, Option<String>)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut attrs = Vec::new();
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'/' {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = None;
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i];
                i += 1;
                let vstart = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                value = Some(s[vstart..i].to_string());
                if i < b.len() {
                    i += 1;
                }
            } else {
                let vstart = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = Some(s[vstart..i].to_string());
            }
        }
        attrs.push((name, value));
    }
    attrs
}

/// 读取标签名（`<` 或 `</` 之后），返回 (小写标签名, 标签结束后的位置)。
fn parse_tag_name(bytes: &[u8], from: usize) -> (Option<String>, usize) {
    let mut i = from;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let start = i;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b':')
    {
        i += 1;
    }
    let name = if start < i {
        Some(String::from_utf8_lossy(&bytes[start..i]).to_ascii_lowercase())
    } else {
        None
    };
    // 跳到 '>' 之后
    let end = find_byte(bytes, i, b'>').map(|pos| pos + 1).unwrap_or(bytes.len());
    (name, end)
}

/// 找 `from` 之后第一个 `needle`（简单字节子串匹配）。
fn find_sub(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= bytes.len() {
        return None;
    }
    let last = bytes.len().saturating_sub(needle.len());
    (from..=last).find(|&idx| &bytes[idx..idx + needle.len()] == needle)
}

/// 找 `from` 之后第一个字节 `byte`。
fn find_byte(bytes: &[u8], from: usize, byte: u8) -> Option<usize> {
    (from..bytes.len()).find(|&idx| bytes[idx] == byte)
}

/// 找标签的 `>`，跳过引号内的内容。
fn find_tag_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(i),
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// 追加文本，超过上限时按字符边界截断；返回 false 表示已截断。
fn push_capped(out: &mut String, chunk: &str) -> bool {
    let room = MAX_HTML_BYTES.saturating_sub(out.len());
    if chunk.len() <= room {
        out.push_str(chunk);
        return true;
    }
    let mut cut = room.min(chunk.len());
    while cut > 0 && !chunk.is_char_boundary(cut) {
        cut -= 1;
    }
    out.push_str(&chunk[..cut]);
    false
}

/// 属性值转义：只处理会破坏标签结构的字符。
fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// HTML → 纯文本（text 为空时的回退）
// ---------------------------------------------------------------------------

/// 从 HTML 提取纯文本（去标签、解码常见实体、块级标签转换行）。
pub(crate) fn plain_text_from_html(html: &str) -> String {
    let html = html.trim_end_matches('\0');
    if html.trim().is_empty() {
        return String::new();
    }
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len().min(MAX_PLAIN_BYTES));
    let mut i = 0usize;
    let mut text_start = 0usize;
    let mut truncated = false;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        if i > text_start {
            let decoded = decode_entities(&html[text_start..i]);
            if !push_plain(&mut out, &decoded) {
                truncated = true;
                break;
            }
        }
        if bytes[i..].starts_with(b"<!--") {
            i = find_sub(bytes, i + 4, b"-->").map(|p| p + 3).unwrap_or(bytes.len());
            text_start = i;
            continue;
        }
        if bytes[i..].starts_with(b"<!") || bytes[i..].starts_with(b"<?") {
            i = find_byte(bytes, i + 2, b'>').map(|p| p + 1).unwrap_or(bytes.len());
            text_start = i;
            continue;
        }
        let closing = bytes[i..].starts_with(b"</");
        let name_start = if closing { i + 2 } else { i + 1 };
        let Some(tag_end) = find_tag_end(bytes, name_start) else {
            i += 1;
            text_start = i;
            continue;
        };
        let (name, _) = parse_tag_name(bytes, name_start);
        i = tag_end + 1;
        text_start = i;
        let Some(name) = name else { continue };
        if DROP_WITH_CONTENT.contains(&name.as_str()) {
            i = skip_element_content(html, bytes, i, &name);
            text_start = i;
            continue;
        }
        if is_break_tag(&name) {
            if !push_plain(&mut out, "\n") {
                truncated = true;
                break;
            }
        }
    }
    if !truncated && text_start < html.len() {
        let decoded = decode_entities(&html[text_start..]);
        let _ = push_plain(&mut out, &decoded);
    }
    collapse_lines(&out)
}

/// 块级标签 → 换行。
fn is_break_tag(name: &str) -> bool {
    matches!(
        name,
        "br" | "p" | "div" | "li" | "tr" | "table" | "ul" | "ol" | "h1" | "h2" | "h3" | "h4"
            | "h5" | "h6" | "blockquote" | "pre" | "hr"
    )
}

/// 追加纯文本，超过上限按字符边界截断；返回 false 表示已截断。
fn push_plain(out: &mut String, chunk: &str) -> bool {
    let room = MAX_PLAIN_BYTES.saturating_sub(out.len());
    if chunk.len() <= room {
        out.push_str(chunk);
        return true;
    }
    let mut cut = room.min(chunk.len());
    while cut > 0 && !chunk.is_char_boundary(cut) {
        cut -= 1;
    }
    out.push_str(&chunk[..cut]);
    false
}

/// 解码常见 HTML 实体（命名 + 十进制/十六进制数字实体）。
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            // 按 UTF-8 字符边界整体拷贝
            let start = i;
            i += 1;
            while i < bytes.len() && !bytes[i].is_ascii() && !is_char_start(bytes[i]) {
                i += 1;
            }
            out.push_str(&s[start..i]);
            continue;
        }
        let Some(end) = find_byte(bytes, i + 1, b';').filter(|&e| e - i <= 10) else {
            out.push('&');
            i += 1;
            continue;
        };
        let entity = &s[i + 1..end];
        let decoded: Option<char> = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            "hellip" => Some('…'),
            _ => {
                if let Some(hex) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                } else {
                    entity
                        .strip_prefix('#')
                        .and_then(|dec| dec.parse::<u32>().ok())
                        .and_then(char::from_u32)
                }
            }
        };
        match decoded {
            Some(c) => {
                out.push(c);
                i = end + 1;
            }
            None => {
                out.push('&');
                i += 1;
            }
        }
    }
    out
}

/// 判断字节是否是 UTF-8 字符起始字节。
fn is_char_start(b: u8) -> bool {
    b & 0xC0 != 0x80
}

/// 合并行：去行尾空白、压缩连续空行、整体 trim。
fn collapse_lines(s: &str) -> String {
    let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = Vec::new();
    for line in normalized.lines() {
        let line = line.trim_end();
        if line.is_empty() && lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim().to_string()
}

// ---------------------------------------------------------------------------
// RTF 基本清洗（不解析完整 RTF，只去掉内嵌对象/图片组）
// ---------------------------------------------------------------------------

/// RTF 清洗：去掉 `\object` / `\pict` / `\objdata` 等内嵌内容组，保留文本控制字。
///
/// 返回 `None` 表示没有有效内容。只做基本过滤：不保证生成"合法 RTF"，
/// 但保证不会把内嵌可执行对象/二进制数据写回剪贴板。
pub(crate) fn sanitize_rtf(input: &str) -> Option<String> {
    let input = input.trim_end_matches('\0');
    if input.trim().is_empty() {
        return None;
    }
    let cleaned = strip_rtf_groups(input, &["object", "objdata", "pict", "datastore", "htmltag"]);
    let cleaned = cleaned.trim_matches(['\0', '\r', '\n', ' ']);
    if cleaned.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(cleaned.len().min(MAX_RTF_BYTES));
    let truncated = !push_rtf_capped(&mut out, cleaned);
    let _ = truncated;
    // 去掉首尾可能残留的空白组
    let out = out.trim().to_string();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// 追加 RTF 文本，超过上限按字符边界截断。
fn push_rtf_capped(out: &mut String, chunk: &str) -> bool {
    let room = MAX_RTF_BYTES.saturating_sub(out.len());
    if chunk.len() <= room {
        out.push_str(chunk);
        return true;
    }
    let mut cut = room.min(chunk.len());
    while cut > 0 && !chunk.is_char_boundary(cut) {
        cut -= 1;
    }
    out.push_str(&chunk[..cut]);
    false
}

/// 去掉首控制字命中 `drop` 的整组 `{...}`（正确处理 `\{` `\}` 转义与嵌套）。
fn strip_rtf_groups(input: &str, drop: &[&str]) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some(word) = group_head_word(&chars, i) {
                if drop.contains(&word.as_str()) {
                    i = skip_braced_group(&chars, i);
                    continue;
                }
            }
        }
        let c = chars[i];
        out.push(c);
        if c == '\\' && i + 1 < chars.len() {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        i += 1;
    }
    out
}

/// 读取 `{` 之后的首个控制字（跳过空白与 `\*` 可忽略标记）。
fn group_head_word(chars: &[char], open: usize) -> Option<String> {
    let mut i = open + 1;
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i + 1 < chars.len() && chars[i] == '\\' && chars[i + 1] == '*' {
        i += 2;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
    }
    if i >= chars.len() || chars[i] != '\\' {
        return None;
    }
    i += 1;
    let start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    if start == i {
        return None;
    }
    Some(chars[start..i].iter().collect::<String>().to_ascii_lowercase())
}

/// 从 `open` 的 `{` 开始跳过平衡的组，返回 `}` 之后的位置。
fn skip_braced_group(chars: &[char], open: usize) -> usize {
    let mut i = open;
    let mut depth = 0usize;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2;
                continue;
            }
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    chars.len()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_html_build_parse_roundtrip() {
        let fragment = r#"<b>你好</b> <span style="color:red">ClipNest</span> &amp; more"#;
        let data = build_cf_html(fragment);
        // 末尾必须是 NUL
        assert_eq!(data.last().copied(), Some(0));
        let text = String::from_utf8(data.clone()).expect("CF_HTML 是 UTF-8");
        let header = parse_cf_html_header(&text);
        let start_html = header.start_html.expect("StartHTML");
        let end_html = header.end_html.expect("EndHTML");
        let start_fragment = header.start_fragment.expect("StartFragment");
        let end_fragment = header.end_fragment.expect("EndFragment");
        assert!(start_html < start_fragment);
        assert!(end_fragment < end_html);
        assert_eq!(
            &text[start_fragment..end_fragment],
            fragment,
            "fragment 偏移必须精确覆盖原文（UTF-8 字节偏移）"
        );
        assert!(end_html <= data.len() - 1, "EndHTML 不应包含末尾 NUL");
        assert_eq!(parse_cf_html(&data).as_deref(), Some(fragment));
    }

    #[test]
    fn cf_html_offsets_are_utf8_byte_offsets() {
        // 中文 3 字节/字：工具若按字符数算偏移会切错字符
        let fragment = "中文内容";
        let data = build_cf_html(fragment);
        let text = String::from_utf8(data.clone()).unwrap();
        let header = parse_cf_html_header(&text);
        let start = header.start_fragment.unwrap();
        let end = header.end_fragment.unwrap();
        assert_eq!(end - start, fragment.len(), "长度必须是 UTF-8 字节数");
        assert!(text.is_char_boundary(start));
        assert!(text.is_char_boundary(end));
        assert_eq!(parse_cf_html(&data).as_deref(), Some(fragment));
    }

    #[test]
    fn cf_html_parse_falls_back_when_offsets_broken() {
        // 偏移按"字符数"写错，必须靠 StartFragment 标记回退
        let html = "Version:0.9\r\nStartHTML:0000000105\r\nEndHTML:9999999999\r\n\
                    StartFragment:9999999999\r\nEndFragment:9999999999\r\n\
                    <html><body><!--StartFragment--><i>回退</i><!--EndFragment--></body></html>\0";
        assert_eq!(
            parse_cf_html(html.as_bytes()).as_deref(),
            Some("<i>回退</i>")
        );
    }

    #[test]
    fn cf_html_parse_handles_bom_and_plain_fragment() {
        let content = "<p>你好</p>";
        // 带 UTF-8 BOM 的 header，偏移必须是真实字节偏移
        let header_len = "\u{feff}Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\n".len();
        let start = header_len;
        let end = header_len + content.len();
        let with_bom = format!(
            "\u{feff}Version:0.9\r\nStartHTML:{start:010}\r\nEndHTML:{end:010}\r\n{content}"
        );
        assert_eq!(
            parse_cf_html(with_bom.as_bytes()).as_deref(),
            Some(content)
        );
        // 完全没有 header 时按整段 HTML 处理
        assert_eq!(
            parse_cf_html("<b>直接片段</b>".as_bytes()).as_deref(),
            Some("<b>直接片段</b>")
        );
        assert!(parse_cf_html(b"").is_none());
    }

    #[test]
    fn sanitize_removes_script_events_and_external_resources() {
        let input = r#"<div onclick="steal()"><script>alert(1)</script>
            <iframe src="https://evil.example/x"></iframe>
            <object data="https://evil/x"></object>
            <img src="https://evil.example/a.png">
            <img src="data:image/png;base64,AA">
            <a href="javascript:alert(2)">js-link</a>
            <a href="https://evil.example/">ext-link</a>
            <b>保留文本</b></div>"#;
        let out = sanitize_html(input).expect("清洗后应有内容");
        let lower = out.to_ascii_lowercase();
        assert!(!lower.contains("<script"), "script 标签必须被移除: {out}");
        assert!(!lower.contains("alert(1)"), "script 内容必须被移除: {out}");
        assert!(!lower.contains("onclick"), "事件属性必须被移除: {out}");
        assert!(!lower.contains("<iframe"), "iframe 必须被移除: {out}");
        assert!(!lower.contains("<object"), "object 必须被移除: {out}");
        assert!(
            !lower.contains("https://evil.example/a.png"),
            "外部图片必须被移除: {out}"
        );
        assert!(
            !lower.contains("https://evil.example/"),
            "外部链接地址必须被过滤: {out}"
        );
        assert!(!lower.contains("javascript:"), "javascript: 必须被过滤: {out}");
        assert!(out.contains("data:image/png;base64,AA"), "data: 图片保留: {out}");
        assert!(out.contains("保留文本"));
        assert!(out.contains("js-link"));
        assert!(out.contains("ext-link"));
    }

    #[test]
    fn sanitize_keeps_basic_structure_and_handles_unclosed_script() {
        let out = sanitize_html("<p>第一段</p><b>粗体<br>换行</b><script>oops").unwrap();
        assert!(out.contains("<p>第一段</p>"));
        assert!(out.contains("<b>粗体<br>换行</b>"));
        assert!(!out.contains("oops"));
        // 只有脚本 → 无有效内容
        assert!(sanitize_html("<script>alert(1)</script>").is_none());
    }

    #[test]
    fn sanitize_drops_doctype_and_comments() {
        let out = sanitize_html("<!DOCTYPE html><!-- 注释 --><p>正文</p>").unwrap();
        assert_eq!(out, "<p>正文</p>");
    }

    #[test]
    fn plain_text_extraction_falls_back_from_html() {
        let html = "<p>第一行</p><div>第二行&nbsp;&amp;&nbsp;实体</div><script>x()</script>";
        let text = plain_text_from_html(html);
        assert!(text.contains("第一行"));
        assert!(text.contains("第二行"));
        assert!(text.contains("& 实体"));
        assert!(!text.contains("<p>"));
        assert!(!text.contains("x()"));
    }

    #[test]
    fn sanitize_rtf_strips_embedded_objects() {
        let rtf = r"{\rtf1\ansi{\object\objdata 0102}{\pict\pngblip 00}正常文本\par}";
        let out = sanitize_rtf(rtf).expect("应保留文本");
        assert!(!out.contains("objdata"));
        assert!(!out.contains("pict"));
        assert!(out.contains("正常文本"));
        assert!(sanitize_rtf("{\\rtf1}").is_some());
        assert!(sanitize_rtf("   ").is_none());
    }
}
