use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Materialize the canonical document as portable Markdown.
///
/// Non-Markdown blocks use stable fenced representations so no canonical block
/// silently disappears from an export. Derived indexes can be rebuilt by
/// parsing the resulting Markdown blocks from the source document again.
pub fn materialize_document_markdown(doc: &crate::document::InkstoneDocument) -> String {
    use crate::document::BlockContent;

    let mut output = String::new();
    if !doc.title.is_empty() {
        output.push_str("# ");
        output.push_str(&doc.title);
        output.push_str("\n\n");
    }
    for (index, block) in doc.blocks.iter().enumerate() {
        if index > 0 && !output.ends_with("\n\n") {
            output.push('\n');
        }
        match &block.content {
            BlockContent::Markdown { text } => output.push_str(text),
            BlockContent::Math { source } => {
                output.push_str("```math\n");
                output.push_str(source);
                output.push_str("\n```");
            }
            BlockContent::Ink { stonepen_json, .. } => {
                output.push_str("```inkstone-ink\n");
                output.push_str(stonepen_json);
                output.push_str("\n```");
            }
            BlockContent::Divider => output.push_str("---"),
        }
        if !output.ends_with('\n') {
            output.push('\n');
        }
        output.push('\n');
    }
    while output.ends_with("\n\n\n") {
        output.pop();
    }
    output
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownIndex {
    pub wikilinks: Vec<WikiLink>,
    pub md_links: Vec<MdLink>,
    pub embeds: Vec<EmbedRef>,
    pub tags: Vec<TagRef>,
    pub headings: Vec<HeadingRef>,
    pub has_code_block: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MdLink {
    pub target: String,
    pub label: String,
    pub title: Option<String>,
    pub context: String,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiLink {
    pub target: String,
    pub alias: Option<String>,
    pub context: String,
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
    let mut md_links = Vec::new();
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

            if let Some(end) = try_parse_md_link(&chars, pos, &mut md_links, line_idx) {
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
        md_links,
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
                    context: chars.iter().collect::<String>().trim().to_string(),
                    line,
                    col: pos,
                });
            } else {
                wikilinks.push(WikiLink {
                    target: content,
                    alias: None,
                    context: chars.iter().collect::<String>().trim().to_string(),
                    line,
                    col: pos,
                });
            }
            return Some(end + 2);
        }
    }
    None
}

fn try_parse_tag(chars: &[char], pos: usize, tags: &mut Vec<TagRef>, line: usize) -> Option<usize> {
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

pub fn index_document_blocks(
    doc: &crate::document::InkstoneDocument,
) -> Vec<(crate::ids::BlockId, MarkdownIndex)> {
    doc.blocks
        .iter()
        .filter_map(|b| match &b.content {
            crate::document::BlockContent::Markdown { text } => Some((b.id, parse_markdown(text))),
            _ => None,
        })
        .collect()
}

pub fn extract_backlinks(index: &MarkdownIndex) -> Vec<(String, String)> {
    let mut backlinks = Vec::new();
    for wl in &index.wikilinks {
        backlinks.push((wl.target.clone(), wl.alias.clone().unwrap_or_default()));
    }
    backlinks
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundWikiLink {
    pub source_doc_id: crate::ids::DocId,
    pub source_block_id: crate::ids::BlockId,
    pub target: String,
}

pub fn derive_outbound_links(doc: &crate::document::InkstoneDocument) -> Vec<OutboundWikiLink> {
    let doc_id = doc.id;
    let mut links = Vec::new();
    for block in &doc.blocks {
        if let crate::document::BlockContent::Markdown { text } = &block.content {
            let idx = parse_markdown(text);
            for wl in &idx.wikilinks {
                links.push(OutboundWikiLink {
                    source_doc_id: doc_id,
                    source_block_id: block.id,
                    target: wl.target.clone(),
                });
            }
        }
    }
    links
}

pub fn all_tags(index: &MarkdownIndex) -> HashSet<String> {
    index.tags.iter().map(|t| t.tag.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_markdown_materializes_title_and_all_blocks_deterministically() {
        use crate::document::{BlockContent, DocumentBlock, InkstoneDocument};
        use crate::ids::{BlockId, DocId};
        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.title = "Portable".into();
        doc.blocks = vec![
            DocumentBlock::new(
                BlockId::new_v4(),
                BlockContent::Markdown {
                    text: "body".into(),
                },
            ),
            DocumentBlock::new(
                BlockId::new_v4(),
                BlockContent::Math {
                    source: "x^2".into(),
                },
            ),
            DocumentBlock::new(BlockId::new_v4(), BlockContent::Divider),
        ];
        let first = materialize_document_markdown(&doc);
        assert_eq!(first, materialize_document_markdown(&doc));
        assert!(first.starts_with("# Portable\n\nbody"));
        assert!(first.contains("```math\nx^2\n```"));
        assert!(first.contains("---"));
    }

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

    #[test]
    fn test_parse_standard_markdown_links() {
        let idx = parse_markdown(
            "[Label](Target.md) [Up](../Target.md) [Space](<Target With Spaces.md>) [T](Target.md \"Optional title\")",
        );
        assert_eq!(idx.md_links.len(), 4);
        assert_eq!(idx.md_links[0].label, "Label");
        assert_eq!(idx.md_links[2].target, "Target With Spaces.md");
        assert_eq!(idx.md_links[3].title.as_deref(), Some("Optional title"));
    }

    #[test]
    fn test_standard_links_skip_code_and_images() {
        let idx = parse_markdown(
            "`[no](No.md)` ![image](Image.md)\n```\n[no](No.md)\n```\n[yes](Yes.md)",
        );
        assert_eq!(idx.md_links.len(), 1);
        assert_eq!(idx.md_links[0].target, "Yes.md");
        assert_eq!(idx.md_links[0].line, 4);
    }

    #[test]
    fn test_index_document_walks_only_markdown_blocks() {
        use crate::document::{BlockContent, DocumentBlock, InkstoneDocument};
        use crate::ids::{BlockId, DocId};

        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown {
                text: "[[WikiLink]]".into(),
            },
        ));
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Math {
                source: "E=mc^2".into(),
            },
        ));
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Ink {
                stonepen_json: "{}".into(),
                collaboration: None,
            },
        ));

        let indexes = index_document_blocks(&doc);
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].1.wikilinks.len(), 1);
        assert_eq!(indexes[0].1.wikilinks[0].target, "WikiLink");
    }

    #[test]
    fn test_derived_indexes_reproducible_from_document() {
        use crate::document::{BlockContent, DocumentBlock, InkstoneDocument};
        use crate::ids::{BlockId, DocId};

        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown {
                text: "# Heading\n[[Page]] text #tag".into(),
            },
        ));

        let indexes = index_document_blocks(&doc);
        assert_eq!(indexes.len(), 1);
        let idx = &indexes[0].1;
        assert_eq!(idx.headings.len(), 1);
        assert_eq!(idx.wikilinks.len(), 1);
        assert_eq!(idx.tags.len(), 1);

        let indexes2 = index_document_blocks(&doc);
        assert_eq!(
            indexes[0].1.wikilinks[0].target,
            indexes2[0].1.wikilinks[0].target
        );
    }

    #[test]
    fn test_outbound_links_include_source_ids() {
        use crate::document::{BlockContent, DocumentBlock, InkstoneDocument};
        use crate::ids::{BlockId, DocId};

        let mut doc = InkstoneDocument::new(DocId::new_v4());
        let block = DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown {
                text: "[[TargetPage]]".into(),
            },
        );
        let block_id = block.id;
        doc.blocks.push(block);

        let links = derive_outbound_links(&doc);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source_doc_id, doc.id);
        assert_eq!(links[0].source_block_id, block_id);
        assert_eq!(links[0].target, "TargetPage");
    }

    #[test]
    fn test_two_outbound_links_from_markdown() {
        use crate::document::{BlockContent, DocumentBlock, InkstoneDocument};
        use crate::ids::{BlockId, DocId};

        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Markdown {
                text: "[[PageA]] and [[PageB]]".into(),
            },
        ));

        let links = derive_outbound_links(&doc);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "PageA");
        assert_eq!(links[1].target, "PageB");
    }

    #[test]
    fn test_non_markdown_blocks_produce_no_outbound_links() {
        use crate::document::{BlockContent, DocumentBlock, InkstoneDocument};
        use crate::ids::{BlockId, DocId};

        let mut doc = InkstoneDocument::new(DocId::new_v4());
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Math {
                source: "x^2".into(),
            },
        ));
        doc.blocks.push(DocumentBlock::new(
            BlockId::new_v4(),
            BlockContent::Ink {
                stonepen_json: "{}".into(),
                collaboration: None,
            },
        ));
        doc.blocks
            .push(DocumentBlock::new(BlockId::new_v4(), BlockContent::Divider));

        let links = derive_outbound_links(&doc);
        assert_eq!(links.len(), 0);
    }
}

fn try_parse_md_link(
    chars: &[char],
    pos: usize,
    md_links: &mut Vec<MdLink>,
    line: usize,
) -> Option<usize> {
    // Images are not document links, and wikilinks have their own parser.
    if chars.get(pos) != Some(&'[')
        || chars.get(pos + 1) == Some(&'[')
        || pos > 0 && chars[pos - 1] == '!'
    {
        return None;
    }
    let label_end = chars[pos + 1..]
        .iter()
        .position(|c| *c == ']')
        .map(|offset| pos + 1 + offset)?;
    if chars.get(label_end + 1) != Some(&'(') {
        return None;
    }
    let destination_start = label_end + 2;
    let (target, after_destination) = if chars.get(destination_start) == Some(&'<') {
        let end = chars[destination_start + 1..]
            .iter()
            .position(|c| *c == '>')
            .map(|offset| destination_start + 1 + offset)?;
        (
            (chars[destination_start + 1..end].iter().collect::<String>()),
            end + 1,
        )
    } else {
        let end = chars[destination_start..]
            .iter()
            .position(|c| *c == ')' || c.is_whitespace())
            .map(|offset| destination_start + offset)?;
        (
            chars[destination_start..end].iter().collect::<String>(),
            end,
        )
    };
    if target.is_empty() {
        return None;
    }
    let mut cursor = after_destination;
    while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
        cursor += 1;
    }
    let title = if chars.get(cursor) == Some(&')') {
        None
    } else {
        let quote = *chars.get(cursor)?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let end = chars[cursor + 1..]
            .iter()
            .position(|c| *c == quote)
            .map(|offset| cursor + 1 + offset)?;
        let title: String = chars[cursor + 1..end].iter().collect();
        cursor = end + 1;
        while chars.get(cursor).is_some_and(|c| c.is_whitespace()) {
            cursor += 1;
        }
        Some(title)
    };
    if chars.get(cursor) != Some(&')') {
        return None;
    }
    md_links.push(MdLink {
        target,
        label: chars[pos + 1..label_end].iter().collect(),
        title,
        context: chars.iter().collect::<String>().trim().to_string(),
        line,
        col: pos,
    });
    Some(cursor + 1)
}
