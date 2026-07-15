use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownIndex {
    pub wikilinks: Vec<WikiLink>,
    pub embeds: Vec<EmbedRef>,
    pub tags: Vec<TagRef>,
    pub headings: Vec<HeadingRef>,
    pub has_code_block: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiLink {
    pub target: String,
    pub alias: Option<String>,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedRef {
    pub kind: String,
    pub id: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagRef {
    pub tag: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadingRef {
    pub level: u8,
    pub text: String,
    pub line: usize,
}

pub fn parse_markdown(text: &str) -> MarkdownIndex {
    let mut wikilinks = Vec::new();
    let mut embeds = Vec::new();
    let mut tags = Vec::new();
    let mut headings = Vec::new();
    let mut in_code_block = false;
    let mut has_code_block = false;

    for (line_idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            has_code_block = true;
            continue;
        }

        if in_code_block {
            continue;
        }

        let level = heading_level(line);
        if level > 0 {
            let heading_text = line
                .chars()
                .skip_while(|c| *c == '#')
                .collect::<String>()
                .trim()
                .to_string();
            headings.push(HeadingRef {
                level,
                text: heading_text,
                line: line_idx,
            });
        }

        let mut pos = 0;
        let chars: Vec<char> = line.chars().collect();

        while pos < chars.len() {
            if chars[pos] == '`' {
                let end = chars[pos + 1..]
                    .iter()
                    .position(|c| *c == '`')
                    .map(|i| pos + 1 + i + 1)
                    .unwrap_or(chars.len());
                pos = end;
                continue;
            }

            if let Some(end) = try_parse_embed(&chars, pos, &mut embeds, line_idx) {
                pos = end;
                continue;
            }

            if let Some(end) = try_parse_wikilink(&chars, pos, &mut wikilinks, line_idx) {
                pos = end;
                continue;
            }

            if let Some(end) = try_parse_tag(&chars, pos, &mut tags, line_idx) {
                pos = end;
                continue;
            }

            pos += 1;
        }
    }

    MarkdownIndex {
        wikilinks,
        embeds,
        tags,
        headings,
        has_code_block,
    }
}

fn heading_level(line: &str) -> u8 {
    let trimmed = line.trim_start();
    if trimmed.starts_with("###### ") {
        return 6;
    }
    if trimmed.starts_with("##### ") {
        return 5;
    }
    if trimmed.starts_with("#### ") {
        return 4;
    }
    if trimmed.starts_with("### ") {
        return 3;
    }
    if trimmed.starts_with("## ") {
        return 2;
    }
    if trimmed.starts_with("# ") {
        return 1;
    }
    0
}

fn try_parse_embed(
    chars: &[char],
    pos: usize,
    embeds: &mut Vec<EmbedRef>,
    line: usize,
) -> Option<usize> {
    if pos + 2 < chars.len() && chars[pos] == '!' && chars[pos + 1] == '[' && chars[pos + 2] == '['
    {
        if let Some(end) = find_closing_brackets(chars, pos + 3) {
            let content: String = chars[pos + 3..end].iter().collect();
            if let Some(colon) = content.find(':') {
                embeds.push(EmbedRef {
                    kind: content[..colon].to_string(),
                    id: content[colon + 1..].to_string(),
                    line,
                    col: pos,
                });
            }
            return Some(end + 2);
        }
    }
    None
}

fn try_parse_wikilink(
    chars: &[char],
    pos: usize,
    wikilinks: &mut Vec<WikiLink>,
    line: usize,
) -> Option<usize> {
    if pos + 1 < chars.len() && chars[pos] == '[' && chars[pos + 1] == '[' {
        if let Some(end) = find_closing_brackets(chars, pos + 2) {
            let content: String = chars[pos + 2..end].iter().collect();
            if let Some(pipe) = content.find('|') {
                wikilinks.push(WikiLink {
                    target: content[..pipe].to_string(),
                    alias: Some(content[pipe + 1..].to_string()),
                    line,
                    col: pos,
                });
            } else {
                wikilinks.push(WikiLink {
                    target: content,
                    alias: None,
                    line,
                    col: pos,
                });
            }
            return Some(end + 2);
        }
    }
    None
}

fn try_parse_tag(
    chars: &[char],
    pos: usize,
    tags: &mut Vec<TagRef>,
    line: usize,
) -> Option<usize> {
    if chars[pos] != '#' {
        return None;
    }

    if pos > 0 {
        let prev = chars[pos - 1];
        if prev.is_alphanumeric() || prev == '_' {
            return None;
        }
    }

    if pos + 1 >= chars.len() {
        return None;
    }

    if !chars[pos + 1].is_alphanumeric() && chars[pos + 1] != '_' {
        return None;
    }

    let tag_start = pos + 1;
    let mut tag_end = tag_start;
    while tag_end < chars.len() {
        let c = chars[tag_end];
        if c.is_alphanumeric() || c == '_' || c == '-' {
            tag_end += 1;
        } else {
            break;
        }
    }

    if tag_end > tag_start {
        let tag: String = chars[tag_start..tag_end].iter().collect();
        tags.push(TagRef {
            tag,
            line,
            col: pos,
        });
        return Some(tag_end);
    }

    None
}

fn find_closing_brackets(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == ']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub fn extract_backlinks(index: &MarkdownIndex) -> Vec<(String, String)> {
    let mut backlinks = Vec::new();
    for wl in &index.wikilinks {
        backlinks.push((wl.target.clone(), wl.alias.clone().unwrap_or_default()));
    }
    backlinks
}

pub fn all_tags(index: &MarkdownIndex) -> HashSet<String> {
    index.tags.iter().map(|t| t.tag.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wikilinks() {
        let text = "Hello [[Page]] and [[Other Page|Alias]]";
        let idx = parse_markdown(text);
        assert_eq!(idx.wikilinks.len(), 2);
        assert_eq!(idx.wikilinks[0].target, "Page");
        assert!(idx.wikilinks[0].alias.is_none());
        assert_eq!(idx.wikilinks[1].target, "Other Page");
        assert_eq!(idx.wikilinks[1].alias, Some("Alias".to_string()));
    }

    #[test]
    fn test_parse_embeds() {
        let text = "Here is ink ![[ink:abc123]]";
        let idx = parse_markdown(text);
        assert_eq!(idx.embeds.len(), 1);
        assert_eq!(idx.embeds[0].kind, "ink");
        assert_eq!(idx.embeds[0].id, "abc123");
    }

    #[test]
    fn test_parse_tags() {
        let text = "Note about #physics and #study";
        let idx = parse_markdown(text);
        assert_eq!(idx.tags.len(), 2);
        assert_eq!(idx.tags[0].tag, "physics");
        assert_eq!(idx.tags[1].tag, "study");
    }

    #[test]
    fn test_skip_code_block() {
        let text = "# Title\n```\n[[not a link]]\n```\n#real-tag";
        let idx = parse_markdown(text);
        assert_eq!(idx.wikilinks.len(), 0);
        assert_eq!(idx.tags.len(), 1);
        assert_eq!(idx.tags[0].tag, "real-tag");
        assert!(idx.has_code_block);
    }

    #[test]
    fn test_parse_headings() {
        let text = "# H1\n## H2\n### H3";
        let idx = parse_markdown(text);
        assert_eq!(idx.headings.len(), 3);
        assert_eq!(idx.headings[0].level, 1);
        assert_eq!(idx.headings[0].text, "H1");
        assert_eq!(idx.headings[1].level, 2);
        assert_eq!(idx.headings[2].level, 3);
    }

    #[test]
    fn test_no_false_tags_in_heading() {
        let text = "## Heading with #tag";
        let idx = parse_markdown(text);
        assert_eq!(idx.tags.len(), 1);
        assert_eq!(idx.tags[0].tag, "tag");
        assert_eq!(idx.headings.len(), 1);
    }

    #[test]
    fn test_inline_code_skips_wikilinks() {
        let text = "`[[not a link]]` and [[real link]]";
        let idx = parse_markdown(text);
        assert_eq!(idx.wikilinks.len(), 1);
        assert_eq!(idx.wikilinks[0].target, "real link");
    }
}
