use comrak::{markdown_to_html, Options};
use inkstone_workspace::WorkspacePath;
use regex::Regex;

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

    let html = markdown_to_html(text, &options);
    process_links(&html, current_path)
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

fn classify_link(href: &str) -> LinkKind {
    let trimmed = href.trim();
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
    // Absolute path is never a valid workspace path
    if href.starts_with('/') {
        return None;
    }
    let parent = parent_folder(current_path);
    let segments: Vec<&str> = href.split('/').collect();
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

/// Regex groups for link processing:
/// 1: everything before href value (e.g. `<a href="`)
/// 2: href value
/// 3: closing `"` of href attribute
/// 4: any attributes between `"` and `>`
/// 5: closing `>`
fn process_links(html: &str, current_path: &str) -> String {
    let re =
        Regex::new(r#"(<a[^>]*?href\s*=\s*")([^"]*)(")([^>]*)(>)"#).expect("invalid link regex");
    re.replace_all(html, |caps: &regex::Captures| {
        let prefix = &caps[1];
        let href = &caps[2];
        let href_close = &caps[3];
        let suffix_rest = &caps[4];
        let gt = &caps[5];
        let kind = classify_link(href);
        match kind {
            LinkKind::InternalWorkspace => {
                if let Some(resolved) = resolve_workspace_path(href, current_path) {
                    format!(
                        "{}#{} data-workspace-path=\"{}\"{}{}",
                        prefix, href_close, resolved, suffix_rest, gt
                    )
                } else {
                    // Resolution failed — render as plain link without workspace attributes
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
    })
    .to_string()
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
        assert!(html.contains("<h1>"));
        assert!(html.contains("Hello"));
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
        let html = process_links("<a href=\"Roadmap.md\">Roadmap</a>", "Projects/Inkstone.md");
        assert_eq!(
            html,
            "<a href=\"#\" data-workspace-path=\"Projects/Roadmap.md\">Roadmap</a>"
        );
    }

    #[test]
    fn test_external_link_exact_anchor() {
        let html = process_links("<a href=\"https://example.com\">Example</a>", "note.md");
        assert_eq!(
            html,
            "<a href=\"https://example.com\" target=\"_blank\" rel=\"noopener noreferrer\">Example</a>"
        );
    }

    #[test]
    fn test_unsafe_lowercase_javascript_link() {
        let html = process_links("<a href=\"javascript:alert(1)\">x</a>", "note.md");
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_unsafe_mixed_case_javascript_link() {
        let html = process_links("<a href=\"JaVaScRiPt:alert(1)\">x</a>", "note.md");
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_unsafe_data_scheme() {
        let html = process_links(
            "<a href=\"data:text/html,<script>alert(1)</script>\">x</a>",
            "note.md",
        );
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_unsafe_vbscript_scheme() {
        let html = process_links("<a href=\"vbscript:msgbox(1)\">x</a>", "note.md");
        assert_eq!(html, "<a href=\"#\">x</a>");
    }

    #[test]
    fn test_fragment_link_exact() {
        let html = process_links("<a href=\"#section\">jump</a>", "note.md");
        assert_eq!(html, "<a href=\"#section\">jump</a>");
    }

    // ── Internal attributes are actual DOM attributes ──

    #[test]
    fn test_internal_workspace_path_is_anchor_attribute() {
        let html = process_links("<a href=\"Roadmap.md\">Roadmap</a>", "Projects/Inkstone.md");
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
        let html = process_links("<a href=\"https://example.com\">Example</a>", "note.md");
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
        let html = process_links(
            "<a href=\"../../../Outside.md\">outside</a>",
            "Projects/Inkstone.md",
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
        let html = process_links("<a href=\"/etc/passwd\">abs</a>", "Projects/Inkstone.md");
        // Absolute path rejected by WorkspacePath — no data-workspace-path
        assert!(!html.contains("data-workspace-path"));
        assert!(html.contains("href=\"/etc/passwd\""));
    }

    // ── Non-Markdown relative links ───────────────────

    #[test]
    fn test_non_markdown_link_not_internal() {
        let html = process_links("<a href=\"image.png\">img</a>", "Projects/note.md");
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
        let html = process_links("<a href=\"doc.pdf\">pdf</a>", "note.md");
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
        let html = process_links("<a href=\"custom-scheme:Note.md\">link</a>", "note.md");
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_foo_plus_bar_scheme_not_internal() {
        let info = classify_and_resolve("foo+bar:Note.md", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        let html = process_links("<a href=\"foo+bar:Note.md\">link</a>", "note.md");
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_foo_dot_bar_scheme_not_internal() {
        let info = classify_and_resolve("foo.bar:Note.md", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        let html = process_links("<a href=\"foo.bar:Note.md\">link</a>", "note.md");
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_single_char_scheme_x_not_internal() {
        let info = classify_and_resolve("x:Note.md", "Projects/Current.md");
        assert!(matches!(info.kind, LinkKind::Other));
        assert!(info.resolved_path.is_none());
        let html = process_links("<a href=\"x:Note.md\">link</a>", "Projects/Current.md");
        assert!(!html.contains("data-workspace-path"));
        assert!(html.contains("href=\"x:Note.md\""));
    }

    #[test]
    fn test_single_char_scheme_a_not_internal() {
        let info = classify_and_resolve("a:Roadmap.md", "note.md");
        assert!(matches!(info.kind, LinkKind::Other));
        assert!(info.resolved_path.is_none());
        let html = process_links("<a href=\"a:Roadmap.md\">link</a>", "note.md");
        assert!(!html.contains("data-workspace-path"));
    }

    // ── Workspace path resolution ─────────────────────

    #[test]
    fn test_resolve_workspace_path_sibling() {
        let result = resolve_workspace_path("Roadmap.md", "Projects/Inkstone.md");
        assert_eq!(result.as_deref(), Some("Projects/Roadmap.md"));
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
        let html = process_links("<a href=\"HTTPS://EXAMPLE.COM\">x</a>", "note.md");
        assert!(html.contains("target=\"_blank\""));
        assert!(html.contains("rel=\"noopener noreferrer\""));
        assert!(!html.contains("data-workspace-path"));
    }

    #[test]
    fn test_mixed_case_http_external() {
        let info = classify_and_resolve("Http://Example.Com/Page.md", "note.md");
        assert!(matches!(info.kind, LinkKind::ExternalHttp));
        let html = process_links("<a href=\"Http://Example.Com/Page.md\">x</a>", "note.md");
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
