use comrak::plugins::syntect::SyntectAdapter;
use comrak::{Options, Plugins, markdown_to_html_with_plugins};
use inkstone_workspace::WorkspacePath;
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

pub struct LinkInfo {
    pub kind: LinkKind,
    pub href: String,
    pub resolved_path: Option<String>,
}

pub enum LinkKind {
    InternalWorkspace,
    ExternalHttp,
    FragmentOnly,
    Unsafe,
    Other,
}

pub fn render_to_html(text: &str, current_path: &str) -> String {
    let mut options = Options::default();
    options.render.unsafe_ = false;
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.tagfilter = true;
    options.render.escaped_char_spans = true;

    let html = markdown_to_preview_html(text, &options);
    process_html(&process_obsidian_html(&html), current_path, None)
}

pub fn render_with_knowledge(
    text: &str,
    current_path: &str,
    knowledge: &crate::knowledge::KnowledgeIndex,
) -> String {
    let mut options = Options::default();
    options.render.unsafe_ = false;
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.tagfilter = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.render.escaped_char_spans = true;

    let html = markdown_to_preview_html(text, &options);
    process_html(&process_obsidian_html(&html), current_path, Some(knowledge))
}

pub fn render_with_knowledge_and_counts(
    text: &str,
    current_path: &str,
    knowledge: &crate::knowledge::KnowledgeIndex,
    heading_counts: &mut HashMap<String, usize>,
) -> String {
    let mut options = Options::default();
    options.render.unsafe_ = false;
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.tagfilter = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.render.escaped_char_spans = true;

    let html = markdown_to_preview_html(text, &options);
    process_html_with_counts(
        &process_obsidian_html(&html),
        current_path,
        Some(knowledge),
        heading_counts,
    )
}

fn markdown_to_preview_html(text: &str, options: &Options<'_>) -> String {
    static SYNTAX_HIGHLIGHTER: LazyLock<SyntectAdapter> =
        LazyLock::new(|| SyntectAdapter::new(None));

    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&*SYNTAX_HIGHLIGHTER);
    markdown_to_html_with_plugins(text, options, &plugins)
}

fn process_obsidian_html(html: &str) -> String {
    strip_escaped_char_spans(&render_highlights(&render_colors(&render_callouts(html))))
}

fn color_class(attribute: &str, color: &str) -> Option<&'static str> {
    match (attribute, color) {
        ("color", "red") => Some("md-color-red"),
        ("color", "orange") => Some("md-color-orange"),
        ("color", "yellow") => Some("md-color-yellow"),
        ("color", "green") => Some("md-color-green"),
        ("color", "blue") => Some("md-color-blue"),
        ("color", "purple") => Some("md-color-purple"),
        ("color", "pink") => Some("md-color-pink"),
        ("color", "gray") => Some("md-color-gray"),
        ("background", "red") => Some("md-bg-red"),
        ("background", "orange") => Some("md-bg-orange"),
        ("background", "yellow") => Some("md-bg-yellow"),
        ("background", "green") => Some("md-bg-green"),
        ("background", "blue") => Some("md-bg-blue"),
        ("background", "purple") => Some("md-bg-purple"),
        ("background", "pink") => Some("md-bg-pink"),
        ("background", "gray") => Some("md-bg-gray"),
        _ => None,
    }
}

fn color_attribute(html: &str, cursor: usize) -> Option<(&'static str, usize)> {
    let rest = html.get(cursor..)?;
    let end = rest.find('}')?;
    let attribute = rest.get(1..end)?;
    let (name, color) = attribute.split_once('=')?;
    let class = color_class(name, color)?;
    Some((class, cursor + end + 1))
}

fn html_tag_name(tag: &str) -> (&str, bool) {
    let tag = tag.trim_start();
    let closing = tag.starts_with('/');
    let tag = tag.strip_prefix('/').unwrap_or(tag).trim_start();
    let end = tag
        .find(|ch: char| ch.is_ascii_whitespace() || ch == '/')
        .unwrap_or(tag.len());
    (&tag[..end], closing)
}

fn render_colors(html: &str) -> String {
    #[derive(Clone, Copy)]
    struct Opener {
        output_index: usize,
        has_content: bool,
        valid: bool,
    }

    let mut output = String::with_capacity(html.len());
    let mut openers: Vec<Opener> = Vec::new();
    let mut cursor = 0;
    let mut excluded_depth = 0usize;
    let mut escaped_depth = 0usize;

    while cursor < html.len() {
        if html[cursor..].starts_with('<') {
            let Some(relative_end) = html[cursor..].find('>') else {
                output.push_str(&html[cursor..]);
                break;
            };
            let end = cursor + relative_end + 1;
            let tag = &html[cursor + 1..end - 1];
            let (name, closing) = html_tag_name(tag);

            if closing
                && matches!(
                    name,
                    "p" | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "li"
                        | "td"
                        | "th"
                        | "div"
                        | "aside"
                )
            {
                openers.clear();
            }
            if matches!(name, "code" | "pre") {
                if closing {
                    excluded_depth = excluded_depth.saturating_sub(1);
                } else {
                    for opener in &mut openers {
                        opener.valid = false;
                    }
                    excluded_depth += 1;
                }
            } else if name == "span" {
                if closing && escaped_depth > 0 {
                    escaped_depth = escaped_depth.saturating_sub(1);
                } else if !closing && tag.contains("data-escaped-char") {
                    escaped_depth += 1;
                }
            }

            output.push_str(&html[cursor..end]);
            cursor = end;
        } else if excluded_depth == 0 && escaped_depth == 0 && html[cursor..].starts_with('[') {
            for opener in &mut openers {
                opener.has_content = true;
            }
            openers.push(Opener {
                output_index: output.len(),
                has_content: false,
                valid: true,
            });
            output.push('[');
            cursor += 1;
        } else if excluded_depth == 0 && escaped_depth == 0 && html[cursor..].starts_with(']') {
            let opener = openers.pop();
            if let (Some(opener), Some((class, end))) = (opener, color_attribute(html, cursor + 1))
            {
                if opener.valid && opener.has_content {
                    output.replace_range(
                        opener.output_index..opener.output_index + 1,
                        &format!("<span class=\"{class}\">"),
                    );
                    output.push_str("</span>");
                    cursor = end;
                    continue;
                }
            }
            output.push(']');
            cursor += 1;
        } else {
            let ch = html[cursor..].chars().next().expect("valid UTF-8");
            if !ch.is_whitespace() {
                for opener in &mut openers {
                    opener.has_content = true;
                }
            }
            output.push(ch);
            cursor += ch.len_utf8();
        }
    }

    output
}

fn strip_escaped_char_spans(html: &str) -> String {
    const OPEN: &str = "<span data-escaped-char>";
    const CLOSE: &str = "</span>";

    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(relative_start) = html[cursor..].find(OPEN) {
        let start = cursor + relative_start;
        let content_start = start + OPEN.len();
        let Some(relative_end) = html[content_start..].find(CLOSE) else {
            break;
        };
        let end = content_start + relative_end;
        output.push_str(&html[cursor..start]);
        output.push_str(&html[content_start..end]);
        cursor = end + CLOSE.len();
    }
    output.push_str(&html[cursor..]);
    output
}

fn callout_kind(callout_type: &str) -> (&'static str, &'static str) {
    match callout_type.to_ascii_lowercase().as_str() {
        "note" => ("note", "Note"),
        "abstract" | "summary" | "tldr" => ("abstract", "Abstract"),
        "info" => ("info", "Info"),
        "todo" => ("todo", "Todo"),
        "tip" | "hint" | "important" => ("tip", "Tip"),
        "success" | "check" | "done" => ("success", "Success"),
        "question" | "help" | "faq" => ("question", "Question"),
        "warning" | "caution" | "attention" => ("warning", "Warning"),
        "failure" | "fail" | "missing" => ("failure", "Failure"),
        "danger" | "error" => ("danger", "Danger"),
        "bug" => ("bug", "Bug"),
        "example" => ("example", "Example"),
        "quote" | "cite" => ("quote", "Quote"),
        _ => ("neutral", "Callout"),
    }
}

fn find_blockquote_end(html: &str, start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut cursor = start;
    while let Some(relative) = html[cursor..].find("blockquote>") {
        let end = cursor + relative + "blockquote>".len();
        let tag_start = html[..end].rfind('<')?;
        if html[tag_start..end].starts_with("</") {
            depth -= 1;
            if depth == 0 {
                return Some(tag_start);
            }
        } else {
            depth += 1;
        }
        cursor = end;
    }
    None
}

fn render_callouts(html: &str) -> String {
    const PREFIX: &str = "<blockquote>\n<p>[!";

    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(relative_start) = html[cursor..].find(PREFIX) {
        let start = cursor + relative_start;
        let type_start = start + PREFIX.len();
        let Some(relative_type_end) = html[type_start..].find(']') else {
            break;
        };
        let type_end = type_start + relative_type_end;
        let callout_type = &html[type_start..type_end];
        if callout_type.is_empty()
            || callout_type.len() > 32
            || !callout_type
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            output.push_str(&html[cursor..type_end + 1]);
            cursor = type_end + 1;
            continue;
        }

        let header_start = type_end + 1;
        let line_end = html[header_start..]
            .find('\n')
            .map(|offset| header_start + offset);
        let paragraph_end = html[header_start..]
            .find("</p>")
            .map(|offset| header_start + offset);
        let header_end = match (line_end, paragraph_end) {
            (Some(line), Some(paragraph)) => line.min(paragraph),
            (Some(line), None) => line,
            (None, Some(paragraph)) => paragraph,
            (None, None) => break,
        };
        let raw_title = &html[header_start..header_end];
        if !raw_title.is_empty() && !raw_title.starts_with(' ') {
            output.push_str(&html[cursor..header_start]);
            cursor = header_start;
            continue;
        }

        let Some(blockquote_end) = find_blockquote_end(html, header_end) else {
            break;
        };
        let title = raw_title.strip_prefix(' ').unwrap_or(raw_title);
        let title = title.strip_suffix("<br />").unwrap_or(title);
        let header_closes_paragraph = html[header_end..].starts_with("</p>");
        let body_start = if header_closes_paragraph {
            let after_paragraph = header_end + "</p>".len();
            after_paragraph + usize::from(html[after_paragraph..blockquote_end].starts_with('\n'))
        } else {
            header_end + 1
        };
        let (kind, label) = callout_kind(callout_type);

        output.push_str(&html[cursor..start]);
        output.push_str("<aside class=\"md-callout md-callout-");
        output.push_str(kind);
        output.push_str("\">\n<div class=\"md-callout-title\"><span class=\"md-callout-type\">");
        output.push_str(label);
        output.push_str("</span>");
        if kind == "neutral" {
            output.push_str("<span class=\"md-callout-title-text\">");
            output.push_str(callout_type);
            output.push_str("</span>");
        }
        if !title.is_empty() {
            output.push_str("<span class=\"md-callout-title-text\">");
            output.push_str(title);
            output.push_str("</span>");
        }
        output.push_str("</div>\n<div class=\"md-callout-content\">\n");
        if !header_closes_paragraph && body_start < blockquote_end {
            output.push_str("<p>");
        }
        output.push_str(&html[body_start..blockquote_end]);
        output.push_str("</div>\n</aside>");
        cursor = blockquote_end + "</blockquote>".len();
    }
    output.push_str(&html[cursor..]);
    output
}

fn is_highlight_boundary(tag: &str) -> bool {
    tag.starts_with("/p")
        || tag.starts_with("/h")
        || tag.starts_with("/li")
        || tag.starts_with("/td")
        || tag.starts_with("/th")
        || tag.starts_with("/div")
        || tag.starts_with("/aside")
}

fn has_highlight_end(html: &str, mut cursor: usize) -> bool {
    let mut excluded_depth = 0usize;
    let mut has_content = false;
    while cursor < html.len() {
        if html[cursor..].starts_with('<') {
            let Some(relative_end) = html[cursor..].find('>') else {
                return false;
            };
            let end = cursor + relative_end + 1;
            let tag = html[cursor + 1..end - 1].to_ascii_lowercase();
            if is_highlight_boundary(&tag) {
                return false;
            }
            if tag.starts_with("code") || tag.starts_with("pre") {
                excluded_depth += 1;
            } else if tag.starts_with("/code") || tag.starts_with("/pre") {
                excluded_depth = excluded_depth.saturating_sub(1);
            }
            cursor = end;
        } else if excluded_depth == 0 && html[cursor..].starts_with("==") {
            return has_content;
        } else {
            let ch = html[cursor..].chars().next().expect("valid UTF-8");
            if excluded_depth == 0 && !ch.is_whitespace() {
                has_content = true;
            }
            cursor += ch.len_utf8();
        }
    }
    false
}

fn render_highlights(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    let mut excluded_depth = 0usize;
    let mut highlighted = false;

    while cursor < html.len() {
        if html[cursor..].starts_with('<') {
            let Some(relative_end) = html[cursor..].find('>') else {
                output.push_str(&html[cursor..]);
                break;
            };
            let end = cursor + relative_end + 1;
            let tag = html[cursor + 1..end - 1].to_ascii_lowercase();
            if tag.starts_with("code") || tag.starts_with("pre") {
                excluded_depth += 1;
            } else if tag.starts_with("/code") || tag.starts_with("/pre") {
                excluded_depth = excluded_depth.saturating_sub(1);
            }
            output.push_str(&html[cursor..end]);
            cursor = end;
        } else if excluded_depth == 0 && html[cursor..].starts_with("==") {
            if highlighted {
                output.push_str("</mark>");
                highlighted = false;
                cursor += 2;
            } else if has_highlight_end(html, cursor + 2) {
                output.push_str("<mark class=\"md-highlight\">");
                highlighted = true;
                cursor += 2;
            } else {
                output.push_str("==");
                cursor += 2;
            }
        } else {
            let ch = html[cursor..].chars().next().expect("valid UTF-8");
            output.push(ch);
            cursor += ch.len_utf8();
        }
    }
    output
}

fn has_url_scheme(s: &str) -> bool {
    // Match RFC 3986 scheme: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) followed by ":"
    // Minimum 1 char to catch single-letter explicit schemes (x:Note.md).
    if let Some(pos) = s.find(':') {
        if pos >= 1 {
            let scheme = &s[..pos];
            if scheme
                .chars()
                .next()
                .map_or(false, |c| c.is_ascii_alphabetic())
                && scheme[1..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
            {
                return true;
            }
        }
    }
    // Also catch :// patterns like ftp://
    if s.contains("://") {
        return true;
    }
    false
}

fn percent_decode(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                result.push(hex);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

fn classify_link(href: &str) -> LinkKind {
    let decoded = percent_decode(href);
    let trimmed = decoded.trim();
    if trimmed.is_empty() {
        return LinkKind::FragmentOnly;
    }
    // Check unsafe schemes case-insensitively
    let lowercase = trimmed.to_lowercase();
    if lowercase.starts_with("javascript:")
        || lowercase.starts_with("data:")
        || lowercase.starts_with("vbscript:")
    {
        return LinkKind::Unsafe;
    }
    if trimmed.starts_with('#') {
        return LinkKind::FragmentOnly;
    }
    if lowercase.starts_with("http://") || lowercase.starts_with("https://") {
        return LinkKind::ExternalHttp;
    }
    // Reject any explicit URL scheme (mailto:, tel:, ftp:, etc.)
    if has_url_scheme(trimmed) {
        return LinkKind::Other;
    }
    // Only treat relative .md paths as internal workspace links
    let path_part = trimmed
        .split(|c| c == '#' || c == '?')
        .next()
        .unwrap_or(trimmed);
    if path_part.to_lowercase().ends_with(".md") {
        LinkKind::InternalWorkspace
    } else {
        LinkKind::Other
    }
}

pub fn resolve_workspace_path(href: &str, current_path: &str) -> Option<String> {
    let decoded = percent_decode(href);
    // Absolute path is never a valid workspace path
    if decoded.starts_with('/') {
        return None;
    }
    let parent = parent_folder(current_path);
    let segments: Vec<&str> = decoded.split('/').collect();
    let mut path_segments: Vec<&str> = if let Some(ref p) = parent {
        if p.is_empty() {
            Vec::new()
        } else {
            p.split('/').collect()
        }
    } else {
        Vec::new()
    };
    let mut depth = path_segments.len() as i32;
    for seg in segments {
        match seg {
            "." | "" => {}
            ".." => {
                depth -= 1;
                if depth < 0 || path_segments.pop().is_none() {
                    return None; // Traversal above workspace root
                }
            }
            _ => {
                depth += 1;
                path_segments.push(seg);
            }
        }
    }
    if path_segments.is_empty() {
        return None;
    }
    let candidate = path_segments.join("/");
    // Validate through WorkspacePath
    WorkspacePath::new(&candidate)
        .ok()
        .map(|p| p.as_str().to_string())
}

fn parent_folder(path: &str) -> Option<String> {
    let idx = path.rfind('/')?;
    if idx == 0 {
        Some(String::new())
    } else {
        Some(path[..idx].to_string())
    }
}

fn process_html(
    html_input: &str,
    current_path: &str,
    knowledge: Option<&crate::knowledge::KnowledgeIndex>,
) -> String {
    let mut heading_counts = HashMap::new();
    process_html_with_counts(html_input, current_path, knowledge, &mut heading_counts)
}

fn process_html_with_counts(
    html_input: &str,
    current_path: &str,
    knowledge: Option<&crate::knowledge::KnowledgeIndex>,
    heading_counts: &mut HashMap<String, usize>,
) -> String {
    // 1. Process headings for deterministic anchors
    let re_headings =
        Regex::new(r#"(?i)<h([1-6])([^>]*)>(.*?)</h[1-6]>"#).expect("invalid heading regex");
    let html_headings = re_headings
        .replace_all(html_input, |caps: &regex::Captures| {
            let level = &caps[1];
            let mut attrs = caps[2].to_string();
            let inner_html = &caps[3];

            let text_re = Regex::new(r#"<[^>]+>"#).unwrap();
            let plain_text = text_re.replace_all(inner_html, "").to_string();
            let count = heading_counts.entry(plain_text.to_lowercase()).or_insert(0);
            *count += 1;
            let anchor = crate::knowledge::heading_anchor(&plain_text, *count);

            let id_re = Regex::new(r#"(?i)id\s*=\s*"[^"]*""#).unwrap();
            attrs = id_re.replace_all(&attrs, "").to_string();

            format!(
                "<h{} id=\"{}\"{}>{}</h{}>",
                level, anchor, attrs, inner_html, level
            )
        })
        .to_string();

    // 2. Process links
    let re_links =
        Regex::new(r#"(<a[^>]*?href\s*=\s*")([^"]*)(")([^>]*)(>)"#).expect("invalid link regex");
    let linked = re_links.replace_all(&html_headings, |caps: &regex::Captures| {
        let prefix = &caps[1];
        let href = &caps[2];
        let href_close = &caps[3];
        let suffix_rest = &caps[4];
        let gt = &caps[5];

        let is_wikilink = suffix_rest.contains("data-wikilink=\"true\"");

        if is_wikilink {
            if let Some(kn) = knowledge {
                let resolved = kn.resolve_wikilink(current_path, href);
                match resolved {
                    crate::knowledge::WikilinkResolution::Resolved(target_path) => {
                        format!("{}#{} class=\"wikilink\" data-workspace-path=\"{}\"{}{}", 
                            prefix, href_close, escape_attr(&target_path), suffix_rest, gt)
                    }
                    crate::knowledge::WikilinkResolution::Ambiguous(_) => {
                        format!("{}#{} class=\"wikilink ambiguous\" title=\"Ambiguous wikilink\"{}{}", 
                            prefix, href_close, suffix_rest, gt)
                    }
                    crate::knowledge::WikilinkResolution::Unresolved => {
                        format!("{}#{} class=\"wikilink missing\" data-missing-target=\"{}\"{}{}", 
                            prefix, href_close, escape_attr(href), suffix_rest, gt)
                    }
                }
            } else {
                format!("{}#{} class=\"wikilink missing\" data-missing-target=\"{}\"{}{}", 
                    prefix, href_close, escape_attr(href), suffix_rest, gt)
            }
        } else {
            let kind = classify_link(href);
            match kind {
                LinkKind::InternalWorkspace => {
                    if let Some(resolved) = resolve_workspace_path(href, current_path) {
                        format!(
                            "{}#{} data-workspace-path=\"{}\"{}{}",
                            prefix, href_close, escape_attr(&resolved), suffix_rest, gt
                        )
                    } else {
                        format!("{}{}{}{}{}", prefix, href, href_close, suffix_rest, gt)
                    }
                }
                LinkKind::Unsafe => {
                    format!("{}#{}{}{}", prefix, href_close, suffix_rest, gt)
                }
                LinkKind::ExternalHttp => {
                    format!(
                        "{}{}{} target=\"_blank\" rel=\"noopener noreferrer\"{}{}",
                        prefix, href, href_close, suffix_rest, gt
                    )
                }
                LinkKind::FragmentOnly => {
                    format!("{}{}{}{}{}", prefix, href, href_close, suffix_rest, gt)
                }
                LinkKind::Other => {
                    format!("{}{}{}{}{}", prefix, href, href_close, suffix_rest, gt)
                }
            }
        }
    }).to_string();

    // Relative images must never trigger a plaintext network request. The UI
    // resolves this validated path to a short-lived blob URL from local cache.
    let images = Regex::new(r#"(?i)(<img[^>]*?)\s+src\s*=\s*\"([^\"]*)\"([^>]*>)"#)
        .expect("invalid image regex");
    images
        .replace_all(&linked, |caps: &regex::Captures| {
            let source = &caps[2];
            match classify_link(source) {
                LinkKind::Other => resolve_workspace_path(source, current_path).map_or_else(
                    || format!("{}{}", &caps[1], &caps[3]),
                    |path| {
                        format!(
                            "{} data-asset-path=\"{}\"{}",
                            &caps[1],
                            escape_attr(&path),
                            &caps[3]
                        )
                    },
                ),
                LinkKind::Unsafe => format!("{}{}", &caps[1], &caps[3]),
                _ => caps[0].to_string(),
            }
        })
        .to_string()
}

fn escape_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn classify_and_resolve(href: &str, current_path: &str) -> LinkInfo {
    let kind = classify_link(href);
    let resolved_path = match kind {
        LinkKind::InternalWorkspace => resolve_workspace_path(href, current_path),
        _ => None,
    };
    LinkInfo {
        kind,
        href: href.to_string(),
        resolved_path,
    }
}

/// Generate all ancestor folder paths for a given workspace path.
/// Example: "A/B/C/D/Note.md" → ["A", "A/B", "A/B/C", "A/B/C/D"]
pub fn ancestor_folders(path: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut accum = String::new();
    for segment in path.split('/') {
        if !accum.is_empty() {
            accum.push('/');
        }
        accum.push_str(segment);
        result.push(accum.clone());
    }
    // Remove the file leaf (last segment if it has an extension)
    if let Some(last) = path.rsplit('/').next() {
        if last.contains('.') {
            result.pop();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic rendering smoke tests ─────────────────────

    #[test]
    fn test_render_heading() {
        let html = render_to_html("# Hello\n", "note.md");
        assert!(html.contains("<h1 id=\"hello\">"));
        assert!(html.contains("Hello"));
    }

    #[test]
    fn test_duplicate_heading_anchors_use_shared_algorithm() {
        let html = render_to_html("## Road Map\n## Road Map\n", "note.md");
        assert!(html.contains("id=\"road-map\""));
        assert!(html.contains("id=\"road-map-2\""));
    }

    #[test]
    fn test_wikilink_render_states_and_alias() {
        let mut knowledge = crate::knowledge::KnowledgeIndex::new();
        knowledge.register_path("Notes/Target.md");
        let resolved = render_with_knowledge("[[Target|Alias]]", "Notes/Source.md", &knowledge);
        assert!(resolved.contains(">Alias</a>"));
        assert!(resolved.contains("data-workspace-path=\"Notes/Target.md\""));
        let missing = render_with_knowledge("[[Missing]]", "Notes/Source.md", &knowledge);
        assert!(missing.contains("wikilink missing"));
        knowledge.register_path("A/Dup.md");
        knowledge.register_path("B/Dup.md");
        let ambiguous = render_with_knowledge("[[Dup]]", "Notes/Source.md", &knowledge);
        assert!(ambiguous.contains("wikilink ambiguous"));
    }

    #[test]
    fn test_render_paragraph() {
        let html = render_to_html("Hello world.\n", "note.md");
        assert!(html.contains("<p>"));
        assert!(html.contains("Hello world"));
    }

    #[test]
    fn test_render_emphasis() {
        let html = render_to_html("*italic* **bold**\n", "note.md");
        assert!(html.contains("<em>"));
        assert!(html.contains("<strong>"));
    }

    #[test]
    fn test_render_list() {
        let html = render_to_html("* item one\n* item two\n", "note.md");
        assert!(html.contains("<ul>"));
        assert!(html.contains("<li>"));
    }

    #[test]
    fn test_render_blockquote() {
        let html = render_to_html("> quoted\n", "note.md");
        assert!(html.contains("<blockquote>"));
    }

    #[test]
    fn test_render_code_block() {
        let html = render_to_html("```rust\nfn main() {}\n```\n", "note.md");
        assert!(html.contains("<code"));
        assert!(html.contains("language-rust"));
    }

    #[test]
    fn test_render_inline_code() {
        let html = render_to_html("Use `let`.\n", "note.md");
        assert!(html.contains("<code>"));
    }

    #[test]
    fn test_inline_colors_emit_only_allowlisted_classes() {
        let markdown = "[r]{color=red} [o]{color=orange} [y]{color=yellow} [g]{color=green} [b]{color=blue} [p]{color=purple} [pink]{color=pink} [gray]{color=gray}\n\n[r]{background=red} [o]{background=orange} [y]{background=yellow} [g]{background=green} [b]{background=blue} [p]{background=purple} [pink]{background=pink} [gray]{background=gray}";
        let html = render_to_html(markdown, "note.md");

        for color in [
            "red", "orange", "yellow", "green", "blue", "purple", "pink", "gray",
        ] {
            assert!(html.contains(&format!("class=\"md-color-{color}\"")));
            assert!(html.contains(&format!("class=\"md-bg-{color}\"")));
        }
        assert_eq!(html.matches("class=\"md-color-").count(), 8);
        assert_eq!(html.matches("class=\"md-bg-").count(), 8);
        assert!(!html.contains("style="));
    }

    #[test]
    fn test_inline_colors_support_nested_emphasis_links_and_colors() {
        let html = render_to_html(
            "[**bold** and [safe](https://example.com) [nested]{background=yellow}]{color=purple}",
            "note.md",
        );

        assert!(html.contains("<span class=\"md-color-purple\"><strong>bold</strong> and <a href=\"https://example.com\" target=\"_blank\" rel=\"noopener noreferrer\">safe</a> <span class=\"md-bg-yellow\">nested</span></span>"));
    }

    #[test]
    fn test_inline_colors_leave_unknown_and_malformed_attributes_literal() {
        let html = render_to_html(
            "[unknown]{color=chartreuse} [case]{color=Red} [extra]{color=red onclick=alert(1)} [missing]{color=red [empty]{} []{color=red}",
            "note.md",
        );

        assert!(!html.contains("md-color-"));
        assert!(html.contains("[unknown]{color=chartreuse}"));
        assert!(html.contains("[case]{color=Red}"));
        assert!(html.contains("[extra]{color=red onclick=alert(1)}"));
        assert!(html.contains("[missing]{color=red"));
        assert!(html.contains("[empty]{}"));
        assert!(html.contains("[]{color=red}"));
    }

    #[test]
    fn test_inline_colors_exclude_escaped_syntax_and_code() {
        let html = render_to_html(
            "\\[escaped\\]\\{color=red\\} and `[inline]{color=blue}`\n\n```markdown\n[fenced]{background=yellow}\n```",
            "note.md",
        );

        assert!(html.contains("[escaped]{color=red}"));
        assert!(html.contains("<code>[inline]{color=blue}</code>"));
        assert!(html.contains("[fenced]{background=yellow}"));
        assert!(!html.contains("md-color-"));
        assert!(!html.contains("md-bg-"));
        assert!(!html.contains("data-escaped-char"));
    }

    #[test]
    fn test_inline_colors_do_not_accept_css_html_or_unsafe_urls() {
        let html = render_to_html(
            "[x]{color=red;position:fixed} [x]{background=url(javascript:alert(1))} [<img src=x onerror=alert(1)>safe]{color=blue} [[bad](javascript:alert(1))]{background=pink}",
            "note.md",
        );

        assert!(html.contains("[x]{color=red;position:fixed}"));
        assert!(html.contains("[x]{background=url(javascript:alert(1))}"));
        assert!(html.contains("<span class=\"md-color-blue\">"));
        assert!(html.contains("safe</span>"));
        assert!(html.contains("<span class=\"md-bg-pink\"><a href=\"\">bad</a></span>"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("onerror"));
        assert!(!html.contains("href=\"javascript:"));
        assert!(!html.contains("style="));
    }

    #[test]
    fn test_fenced_code_highlighting_uses_classes_and_escapes_code() {
        let html = render_to_html(
            "```rust\nfn main() { let value = \"<script>alert(1)</script>\"; }\n```",
            "note.md",
        );

        assert!(html.contains("<pre class=\"syntax-highlighting\">"));
        assert!(html.contains("<code class=\"language-rust\">"));
        assert!(html.contains("<span class="));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("style="));
    }

    #[test]
    fn test_obsidian_highlight_renders_fixed_mark() {
        let html = render_to_html("Before ==bright **and bold**== after.", "note.md");
        assert_eq!(
            html,
            "<p>Before <mark class=\"md-highlight\">bright <strong>and bold</strong></mark> after.</p>\n"
        );
    }

    #[test]
    fn test_obsidian_highlight_escapes_content_and_excludes_code() {
        let html = render_to_html(
            "==&lt;img onerror=alert(1)&gt;== and `==not highlighted==`\n\n```\n==also not==\n```",
            "note.md",
        );
        assert!(html.contains("<mark class=\"md-highlight\">&lt;img onerror=alert(1)&gt;</mark>"));
        assert!(html.contains("<code>==not highlighted==</code>"));
        assert!(html.contains("<pre class=\"syntax-highlighting\"><code>"));
        assert!(html.contains("==also not=="));
        assert_eq!(html.matches("class=\"md-highlight\"").count(), 1);
    }

    #[test]
    fn test_obsidian_highlight_leaves_malformed_delimiters_literal() {
        let html = render_to_html("Unclosed ==mark.\n\nEmpty ==== remains.", "note.md");
        assert!(!html.contains("<mark"));
        assert!(html.contains("Unclosed ==mark."));
        assert!(html.contains("Empty ==== remains."));
    }

    #[test]
    fn test_obsidian_callout_renders_allowlisted_fixed_classes() {
        let html = render_to_html(
            "> [!warning] Read [the guide](https://example.com)\n> Keep ==safe==.",
            "note.md",
        );
        assert!(html.contains("<aside class=\"md-callout md-callout-warning\">"));
        assert!(html.contains("<span class=\"md-callout-type\">Warning</span>"));
        assert!(html.contains("<span class=\"md-callout-title-text\">Read <a href=\"https://example.com\" target=\"_blank\" rel=\"noopener noreferrer\">the guide</a></span>"));
        assert!(html.contains("<p>Keep <mark class=\"md-highlight\">safe</mark>.</p>"));
        assert!(!html.contains("[!warning]"));
    }

    #[test]
    fn test_obsidian_callout_alias_is_canonical_and_default_title_is_visible() {
        let html = render_to_html("> [!caution]\n> Check this.", "note.md");
        assert!(html.contains("class=\"md-callout md-callout-warning\""));
        assert!(html.contains("class=\"md-callout-type\">Warning</span>"));
        assert!(!html.contains("md-callout-caution"));
    }

    #[test]
    fn test_unknown_callout_type_is_neutral_and_cannot_add_classes() {
        let html = render_to_html(
            "> [!evil-onclick] <script>alert(1)</script>\n> [unsafe](javascript:alert(1))",
            "note.md",
        );
        assert!(html.contains("class=\"md-callout md-callout-neutral\""));
        assert!(html.contains("class=\"md-callout-type\">Callout</span>"));
        assert!(html.contains("class=\"md-callout-title-text\">evil-onclick</span>"));
        assert!(!html.contains("md-callout-evil"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("alert(1)</script>"));
        assert!(html.contains("<a href=\"\">unsafe</a>"));
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn test_malformed_callout_stays_an_ordinary_blockquote() {
        let html = render_to_html("> [!not a type] title\n> body", "note.md");
        assert!(html.starts_with("<blockquote>"));
        assert!(html.contains("[!not a type] title"));
        assert!(!html.contains("md-callout"));
    }

    #[test]
    fn test_render_horizontal_rule() {
        let html = render_to_html("---\n", "note.md");
        assert!(html.contains("<hr"));
    }

    // ── Raw HTML safety ────────────────────────────────

    #[test]
    fn test_raw_html_stripped() {
        let html = render_to_html("<script>alert(1)</script>\n", "note.md");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("alert"));
    }

    #[test]
    fn test_raw_html_event_handler_stripped() {
        let html = render_to_html("<img onerror=\"alert(1)\">\n", "note.md");
        assert!(!html.contains("onerror"));
    }

    // ── Exact anchor output ────────────────────────────

    #[test]
    fn test_internal_link_exact_anchor() {
        let html = process_html(
            "<a href=\"Roadmap.md\">Roadmap</a>",
            "Projects/Inkstone.md",
            None,
        );
        assert_eq!(
            html,
            "<a href=\"#\" data-workspace-path=\"Projects/Roadmap.md\">Roadmap</a>"
        );
    }

    #[test]
    fn test_external_link_exact_anchor() {
        let html = process_html(
            "<a href=\"https://example.com\">Example</a>",
            "note.md",
            None,
        );
        assert_eq!(
            html,
            "<a href=\"https://example.com\" target=\"_blank\" rel=\"noopener noreferrer\">Example</a>"
        );
    }

    #[test]
    fn test_unsafe_lowercase_javascript_link() {
        let html = process_html("<a href=\"javascript:alert(1)\">x</a>", "note.md", None);
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_unsafe_mixed_case_javascript_link() {
        let html = process_html("<a href=\"JaVaScRiPt:alert(1)\">x</a>", "note.md", None);
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_unsafe_data_scheme() {
        let html = process_html(
            "<a href=\"data:text/html, <script>alert(1, None)</script>\">x</a>",
            "note.md",
            None,
        );
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_unsafe_vbscript_scheme() {
        let html = process_html("<a href=\"vbscript:msgbox(1)\">x</a>", "note.md", None);
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_fragment_link_exact() {
        let html = process_html("<a href=\"#section\">jump</a>", "note.md", None);
        assert_eq!(html, "<a href=\"#section\">jump</a>");
    }

    // ── Internal attributes are actual DOM attributes ──

    #[test]
    fn test_internal_workspace_path_is_anchor_attribute() {
        let html = process_html(
            "<a href=\"Roadmap.md\">Roadmap</a>",
            "Projects/Inkstone.md",
            None,
        );
        // data-workspace-path must be inside the opening <a> tag before >
        assert!(html.contains("data-workspace-path=\"Projects/Roadmap.md\""));
        // The text content must NOT contain the attribute value
        assert!(!html.contains(">Projects/Roadmap.md<"));
        // Must appear before closing >
        assert!(html.contains("data-workspace-path=\"Projects/Roadmap.md\">"));
        // Must be inside <a ...>
        assert!(html.starts_with("<a "));
    }

    #[test]
    fn test_external_target_rel_are_anchor_attributes() {
        let html = process_html(
            "<a href=\"https://example.com\">Example</a>",
            "note.md",
            None,
        );
        assert!(html.contains("target=\"_blank\""));
        assert!(html.contains("rel=\"noopener noreferrer\""));
        // target and rel must be before closing >
        assert!(html.contains("rel=\"noopener noreferrer\">"));
        // Link text must not contain target/rel text
        assert!(!html.contains(">target"));
        assert!(!html.contains(">rel"));
    }

    // ── Invalid resolution never emits data-workspace-path ──

    #[test]
    fn test_invalid_traversal_no_workspace_path() {
        let html = process_html(
            "<a href=\"../../../Outside.md\">outside</a>",
            "Projects/Inkstone.md",
            None,
        );
        // Must NOT have data-workspace-path — traversal above root was rejected
        assert!(!html.contains("data-workspace-path"));
        // href must remain unchanged (not replaced with workspace path)
        assert!(html.contains("href=\"../../../Outside.md\""));
        // No target/rel either
        assert!(!html.contains("target=\"_blank\""));
    }

    #[test]
    fn test_invalid_workspace_path_no_data_attribute() {
        let html = process_html(
            "<a href=\"/etc/passwd\">abs</a>",
            "Projects/Inkstone.md",
            None,
        );
        // Absolute path rejected by WorkspacePath — no data-workspace-path
        assert!(!html.contains("data-workspace-path"));
        assert!(html.contains("href=\"/etc/passwd\""));
    }

    // ── Non-Markdown relative links ───────────────────

    #[test]
    fn test_non_markdown_link_not_internal() {
        let html = process_html("<a href=\"image.png\">img</a>", "Projects/note.md", None);
        // Must NOT have data-workspace-path
        assert!(!html.contains("data-workspace-path"));
        // href must remain unchanged
        assert!(html.contains("href=\"image.png\""));
        // No target/rel added
        assert!(!html.contains("target=\"_blank\""));
        assert_eq!(html, "<a href=\"image.png\">img</a>");
    }

    #[test]
    fn test_pdf_link_not_internal() {
        let html = process_html("<a href=\"doc.pdf\">pdf</a>", "note.md", None);
        assert!(!html.contains("data-workspace-path"));
        assert_eq!(html, "<a href=\"doc.pdf\">pdf</a>");
    }

    // ── URL scheme rejection ──────────────────────────

    #[test]
    fn test_mailto_not_internal() {
        let info = classify_and_resolve("mailto:user@example.com", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        assert!(info.resolved_path.is_none());
    }

    #[test]
    fn test_tel_not_internal() {
        let info = classify_and_resolve("tel:123", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
    }

    #[test]
    fn test_ftp_not_internal() {
        let info = classify_and_resolve("ftp://example.com/file", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
    }

    #[test]
    fn test_unknown_scheme_not_internal() {
        let info = classify_and_resolve("unknown-scheme:value", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
    }

    #[test]
    fn test_custom_scheme_not_internal() {
        let info = classify_and_resolve("custom-scheme:Note.md", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        assert!(info.resolved_path.is_none());
        // process_links must not emit data-workspace-path
        let html = process_html(
            "<a href=\"custom-scheme:Note.md\">link</a>",
            "note.md",
            None,
        );
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_foo_plus_bar_scheme_not_internal() {
        let info = classify_and_resolve("foo+bar:Note.md", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        let html = process_html("<a href=\"foo+bar:Note.md\">link</a>", "note.md", None);
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_foo_dot_bar_scheme_not_internal() {
        let info = classify_and_resolve("foo.bar:Note.md", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        let html = process_html("<a href=\"foo.bar:Note.md\">link</a>", "note.md", None);
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_single_char_scheme_x_not_internal() {
        let info = classify_and_resolve("x:Note.md", "Projects/Current.md");
        assert!(matches!(info.kind, LinkKind::Other));
        assert!(info.resolved_path.is_none());
        let html = process_html(
            "<a href=\"x:Note.md\">link</a>",
            "Projects/Current.md",
            None,
        );
        assert!(!html.contains("data-workspace-path"));
        assert!(html.contains("href=\"x:Note.md\""));
    }

    #[test]
    fn test_single_char_scheme_a_not_internal() {
        let info = classify_and_resolve("a:Roadmap.md", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        assert!(info.resolved_path.is_none());
        let html = process_html("<a href=\"a:Roadmap.md\">link</a>", "note.md", None);
        assert!(!html.contains("data-workspace-path"));
    }

    // ── Workspace path resolution ─────────────────────

    #[test]
    fn test_resolve_workspace_path_sibling() {
        let result = resolve_workspace_path("Roadmap.md", "Projects/Inkstone.md");
        assert_eq!(result.as_deref(), Some("Projects/Roadmap.md"));
    }

    #[test]
    fn relative_image_uses_validated_asset_path_without_plaintext_src() {
        let html = process_html(
            "<img src=\"../Assets/private.png\" alt=\"x\">",
            "Notes/One.md",
            None,
        );
        assert!(html.contains("data-asset-path=\"Assets/private.png\""));
        assert!(!html.contains("src="));
    }

    #[test]
    fn test_resolve_workspace_path_parent() {
        let result = resolve_workspace_path("../Notes.md", "Projects/Roadmap.md");
        assert_eq!(result.as_deref(), Some("Notes.md"));
    }

    #[test]
    fn test_resolve_workspace_path_deep_nested() {
        let result = resolve_workspace_path("Archive/2026/Notes.md", "Projects/Inkstone.md");
        assert_eq!(result.as_deref(), Some("Projects/Archive/2026/Notes.md"));
    }

    #[test]
    fn test_resolve_workspace_path_from_root() {
        let result = resolve_workspace_path("notes/other.md", "");
        assert_eq!(result.as_deref(), Some("notes/other.md"));
    }

    #[test]
    fn test_resolve_workspace_path_empty() {
        let result = resolve_workspace_path("", "note.md");
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_traversal_above_root_rejected() {
        let result = resolve_workspace_path("../../../Outside.md", "Projects/Inkstone.md");
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_invalid_workspace_path_rejected() {
        // Absolute paths are rejected by WorkspacePath
        let result = resolve_workspace_path("/etc/passwd", "note.md");
        assert!(result.is_none());
    }

    // ── Internal link classification ──────────────────

    #[test]
    fn test_internal_sibling_link() {
        let info = classify_and_resolve("Roadmap.md", "Projects/Inkstone.md");
        assert!(matches!(info.kind, LinkKind::InternalWorkspace));
        assert_eq!(info.resolved_path.as_deref(), Some("Projects/Roadmap.md"));
    }

    #[test]
    fn test_internal_parent_relative_link() {
        let info = classify_and_resolve("../Notes.md", "Projects/Roadmap.md");
        assert!(matches!(info.kind, LinkKind::InternalWorkspace));
        assert_eq!(info.resolved_path.as_deref(), Some("Notes.md"));
    }

    #[test]
    fn test_internal_deep_link() {
        let info = classify_and_resolve("Archive/2026/Notes.md", "Projects/Inkstone.md");
        assert!(matches!(info.kind, LinkKind::InternalWorkspace));
        assert_eq!(
            info.resolved_path.as_deref(),
            Some("Projects/Archive/2026/Notes.md")
        );
    }

    #[test]
    fn test_internal_root_link_from_deep() {
        let info = classify_and_resolve("../../Notes.md", "Archive/2026/Notes.md");
        assert!(matches!(info.kind, LinkKind::InternalWorkspace));
        assert_eq!(info.resolved_path.as_deref(), Some("Notes.md"));
    }

    #[test]
    fn test_fragment_link() {
        let info = classify_and_resolve("#section", "note.md");
        assert!(matches!(info.kind, LinkKind::FragmentOnly));
        assert!(info.resolved_path.is_none());
    }

    #[test]
    fn test_external_link_classification() {
        let info = classify_and_resolve("https://example.com", "note.md");
        assert!(matches!(info.kind, LinkKind::ExternalHttp));
    }

    #[test]
    fn test_uppercase_https_external() {
        let info = classify_and_resolve("HTTPS://EXAMPLE.COM", "note.md");
        assert!(matches!(info.kind, LinkKind::ExternalHttp));
        let html = process_html("<a href=\"HTTPS://EXAMPLE.COM\">x</a>", "note.md", None);
        assert!(html.contains("target=\"_blank\""));
        assert!(html.contains("rel=\"noopener noreferrer\""));
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_mixed_case_http_external() {
        let info = classify_and_resolve("Http://Example.Com/Page.md", "note.md");
        assert!(matches!(info.kind, LinkKind::ExternalHttp));
        let html = process_html(
            "<a href=\"Http://Example.Com/Page.md\">x</a>",
            "note.md",
            None,
        );
        assert!(html.contains("target=\"_blank\""));
        assert!(html.contains("rel=\"noopener noreferrer\""));
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_unsafe_js_link() {
        let info = classify_and_resolve("javascript:alert(1)", "note.md");
        assert!(matches!(info.kind, LinkKind::Unsafe));
    }

    #[test]
    fn test_unsafe_data_link() {
        let info = classify_and_resolve("data:text/html,<script>alert(1)</script>", "note.md");
        assert!(matches!(info.kind, LinkKind::Unsafe));
    }

    // ── Ancestor folders ──────────────────────────────

    #[test]
    fn test_ancestor_folders_deep_path() {
        let result = ancestor_folders("A/B/C/D/Note.md");
        assert_eq!(result, vec!["A", "A/B", "A/B/C", "A/B/C/D"]);
    }

    #[test]
    fn test_ancestor_folders_root_file() {
        let result = ancestor_folders("Note.md");
        assert!(result.is_empty());
    }

    #[test]
    fn test_ancestor_folders_one_level() {
        let result = ancestor_folders("A/Note.md");
        assert_eq!(result, vec!["A"]);
    }
}
