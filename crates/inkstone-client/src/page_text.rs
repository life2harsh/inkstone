//! Block-level text editing rules for the page.
//!
//! A page is a column of small Markdown blocks, roughly one top-level element
//! each, so only the element being edited shows its source. This module holds
//! the platform-independent rules: how a block's text splits into elements,
//! what Enter does at a caret, how a block is classified for styling, and how
//! a click in rendered text maps back to a source offset.
//!
//! Offsets crossing this API are UTF-16 code units, like browser selections.

/// One piece of a block after a split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    /// UTF-16 offset in the original text where this piece's text starts.
    pub source_start: u32,
}

/// A block replaced by several blocks, in order. The original block keeps
/// `pieces[keep]`; the rest become new blocks around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub pieces: Vec<Piece>,
    pub keep: usize,
    /// Piece that receives the caret, and the caret inside it.
    pub caret_piece: usize,
    pub caret: u32,
}

impl Split {
    /// Where original UTF-16 offset `offset` lands: `(piece, local offset)`.
    pub fn locate(&self, offset: u32) -> (usize, u32) {
        let index = self
            .pieces
            .iter()
            .rposition(|piece| piece.source_start <= offset && !piece.text.is_empty())
            .or_else(|| {
                self.pieces
                    .iter()
                    .rposition(|piece| piece.source_start <= offset)
            })
            .unwrap_or(0);
        let piece = &self.pieces[index];
        let len = utf16_len(&piece.text);
        (index, offset.saturating_sub(piece.source_start).min(len))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterAction {
    /// Let the textarea insert a newline.
    Default,
    /// Replace the block text and put the caret at `caret`.
    Replace {
        text: String,
        caret: u32,
    },
    Split(Split),
}

/// What a block's source starts with, for styling its source like its
/// rendered form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Empty,
    Heading(u8),
    Paragraph,
    List,
    Quote,
    Code,
    Math,
    Table,
    Rule,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Empty => "empty",
            SourceKind::Heading(1) => "h1",
            SourceKind::Heading(2) => "h2",
            SourceKind::Heading(3) => "h3",
            SourceKind::Heading(4) => "h4",
            SourceKind::Heading(5) => "h5",
            SourceKind::Heading(_) => "h6",
            SourceKind::Paragraph => "p",
            SourceKind::List => "list",
            SourceKind::Quote => "quote",
            SourceKind::Code => "code",
            SourceKind::Math => "math",
            SourceKind::Table => "table",
            SourceKind::Rule => "rule",
        }
    }
}

pub fn utf16_len(text: &str) -> u32 {
    text.encode_utf16().count() as u32
}

/// Byte index of UTF-16 offset `offset`, clamped and snapped to a char.
pub fn byte_of_utf16(text: &str, offset: u32) -> usize {
    let mut seen = 0u32;
    for (index, ch) in text.char_indices() {
        if seen >= offset {
            return index;
        }
        seen += ch.len_utf16() as u32;
    }
    text.len()
}

pub fn utf16_of_byte(text: &str, byte: usize) -> u32 {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    utf16_len(&text[..byte])
}

/// A line of text: byte range without its newline.
#[derive(Debug, Clone, Copy)]
struct Line {
    start: usize,
    end: usize,
}

fn lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            out.push(Line { start, end: index });
            start = index + 1;
        }
    }
    out.push(Line {
        start,
        end: text.len(),
    });
    out
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Leading indentation in columns (tabs count as 4).
fn indent(line: &str) -> usize {
    let mut columns = 0;
    for ch in line.chars() {
        match ch {
            ' ' => columns += 1,
            '\t' => columns += 4,
            _ => break,
        }
    }
    columns
}

/// `(char, length)` of a code fence opener/closer line.
fn fence(line: &str) -> Option<(char, usize)> {
    if indent(line) > 3 {
        return None;
    }
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let length = trimmed.chars().take_while(|c| *c == ch).count();
    (length >= 3).then_some((ch, length))
}

fn is_math_fence(line: &str) -> bool {
    line.trim() == "$$"
}

pub fn heading_level(line: &str) -> Option<u8> {
    if indent(line) > 3 {
        return None;
    }
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    (rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')).then_some(level as u8)
}

fn is_rule(line: &str) -> bool {
    if indent(line) > 3 {
        return false;
    }
    let compact = line
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    compact.len() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
}

fn is_setext_underline(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty()
        && indent(line) <= 3
        && (trimmed.chars().all(|c| c == '=') || trimmed.chars().all(|c| c == '-'))
}

/// A list item line: `(indent text, marker, content byte offset in line)`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListItem {
    indent: String,
    /// The marker as typed, e.g. `-`, `3.`, `1)`.
    marker: String,
    task: bool,
    /// Byte offset in the line where the item content starts.
    content: usize,
}

fn list_item(line: &str) -> Option<ListItem> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent_len..];
    let marker_len = if rest.starts_with(['-', '*', '+']) {
        1
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 || digits > 9 || !rest[digits..].starts_with(['.', ')']) {
            return None;
        }
        digits + 1
    };
    let after = &rest[marker_len..];
    if !(after.is_empty() || after.starts_with(' ') || after.starts_with('\t')) {
        return None;
    }
    if marker_len == 1 && is_rule(line) {
        return None;
    }
    let mut content = indent_len + marker_len + (after.len() - after.trim_start().len());
    let mut task = false;
    let body = &line[content..];
    for box_text in ["[ ] ", "[x] ", "[X] "] {
        if body.starts_with(box_text) {
            task = true;
            content += box_text.len();
            break;
        }
    }
    if !task && ["[ ]", "[x]", "[X]"].contains(&body) {
        task = true;
        content += 3;
    }
    Some(ListItem {
        indent: line[..indent_len].to_string(),
        marker: rest[..marker_len].to_string(),
        task,
        content,
    })
}

fn next_marker(marker: &str) -> String {
    match marker.strip_suffix(['.', ')']) {
        Some(number) => match number.parse::<u64>() {
            Ok(value) => format!("{}{}", value + 1, &marker[number.len()..]),
            Err(_) => marker.to_string(),
        },
        None => marker.to_string(),
    }
}

/// Lines (`[x]: url`, `[^1]: note`) that other elements refer to; a block
/// holding them is never split so the references keep resolving.
fn is_definition(line: &str) -> bool {
    let trimmed = line.trim_start();
    indent(line) <= 3
        && trimmed.starts_with('[')
        && trimmed
            .find("]:")
            .is_some_and(|close| close > 1 && !trimmed[1..close].contains(']'))
}

/// Classifies a block by its first non-blank line.
pub fn source_kind(text: &str) -> SourceKind {
    let Some(first) = text.lines().find(|line| !is_blank(line)) else {
        return SourceKind::Empty;
    };
    if let Some(level) = heading_level(first) {
        return SourceKind::Heading(level);
    }
    if fence(first).is_some() || (indent(first) >= 4 && !first.trim().is_empty()) {
        return SourceKind::Code;
    }
    if is_math_fence(first) {
        return SourceKind::Math;
    }
    if is_rule(first) {
        return SourceKind::Rule;
    }
    if list_item(first).is_some() {
        return SourceKind::List;
    }
    let trimmed = first.trim_start();
    if trimmed.starts_with('>') {
        return SourceKind::Quote;
    }
    if trimmed.starts_with('|') {
        return SourceKind::Table;
    }
    SourceKind::Paragraph
}

/// Byte ranges of the top-level elements of `text`, in order. Blank lines
/// between elements belong to none of them.
fn element_ranges(text: &str) -> Vec<(usize, usize)> {
    let lines = lines(text);
    let line_text = |line: &Line| &text[line.start..line.end];
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    // Kind of the element being built, for deciding what may continue it.
    let mut current_kind = SourceKind::Empty;
    let mut fence_open: Option<(char, usize)> = None;
    let mut math_open = false;
    let mut pending_blank = false;

    let close = |current: &mut Option<(usize, usize)>, ranges: &mut Vec<(usize, usize)>| {
        if let Some(range) = current.take() {
            ranges.push(range);
        }
    };

    for (index, line) in lines.iter().enumerate() {
        let content = line_text(line);

        // Front matter at the very top is one element.
        if index == 0 && content == "---" {
            if let Some(end) = lines
                .iter()
                .skip(1)
                .position(|l| matches!(line_text(l), "---" | "..."))
            {
                let last = &lines[end + 1];
                ranges.push((line.start, last.end));
                // Skip the front matter lines.
                return ranges
                    .into_iter()
                    .chain(
                        element_ranges(&text[last.end..])
                            .into_iter()
                            .map(|(s, e)| (s + last.end, e + last.end)),
                    )
                    .collect();
            }
        }

        if let Some((ch, length)) = fence_open {
            if let Some(range) = current.as_mut() {
                range.1 = line.end;
            }
            if fence(content).is_some_and(|(c, l)| c == ch && l >= length)
                && content
                    .trim_start()
                    .trim_start_matches(ch)
                    .trim()
                    .is_empty()
            {
                fence_open = None;
                close(&mut current, &mut ranges);
            }
            continue;
        }
        if math_open {
            if let Some(range) = current.as_mut() {
                range.1 = line.end;
            }
            if is_math_fence(content) {
                math_open = false;
                close(&mut current, &mut ranges);
            }
            continue;
        }

        if is_blank(content) {
            pending_blank = current.is_some();
            continue;
        }

        let continues_after_blank = pending_blank
            && matches!(current_kind, SourceKind::List)
            && (list_item(content).is_some() || indent(content) >= 2);
        if pending_blank && !continues_after_blank {
            close(&mut current, &mut ranges);
        }
        pending_blank = false;

        if let Some(open) = fence(content) {
            close(&mut current, &mut ranges);
            current = Some((line.start, line.end));
            current_kind = SourceKind::Code;
            fence_open = Some(open);
            continue;
        }
        if is_math_fence(content) {
            close(&mut current, &mut ranges);
            current = Some((line.start, line.end));
            current_kind = SourceKind::Math;
            math_open = true;
            continue;
        }
        if heading_level(content).is_some() {
            close(&mut current, &mut ranges);
            ranges.push((line.start, line.end));
            current_kind = SourceKind::Empty;
            continue;
        }
        if current.is_some()
            && matches!(current_kind, SourceKind::Paragraph)
            && is_setext_underline(content)
        {
            if let Some(range) = current.as_mut() {
                range.1 = line.end;
            }
            close(&mut current, &mut ranges);
            current_kind = SourceKind::Empty;
            continue;
        }
        if is_rule(content) && list_item(content).is_none() {
            close(&mut current, &mut ranges);
            ranges.push((line.start, line.end));
            current_kind = SourceKind::Empty;
            continue;
        }
        match current.as_mut() {
            Some(range) => range.1 = line.end,
            None => {
                current = Some((line.start, line.end));
                current_kind = source_kind(content);
            }
        }
    }
    close(&mut current, &mut ranges);
    ranges
}

/// Splits a block into one piece per top-level element, or `None` when it
/// already is a single element (or must stay whole).
pub fn split_elements(text: &str) -> Option<Split> {
    if text.lines().any(is_definition) {
        return None;
    }
    let ranges = element_ranges(text);
    if ranges.len() < 2 {
        return None;
    }
    let pieces = ranges
        .into_iter()
        .map(|(start, end)| Piece {
            text: text[start..end].to_string(),
            source_start: utf16_of_byte(text, start),
        })
        .collect();
    Some(Split {
        pieces,
        keep: 0,
        caret_piece: 0,
        caret: 0,
    })
}

/// Whether byte `at` sits inside an open code fence or `$$` block, counting
/// the line it is on.
fn inside_fence(text: &str, at: usize) -> bool {
    let mut fence_open: Option<(char, usize)> = None;
    let mut math_open = false;
    for line in lines(text) {
        if line.start > at {
            break;
        }
        let content = &text[line.start..line.end];
        if let Some((ch, length)) = fence_open {
            if fence(content).is_some_and(|(c, l)| c == ch && l >= length) {
                fence_open = None;
            }
        } else if math_open {
            if is_math_fence(content) {
                math_open = false;
            }
        } else if let Some(open) = fence(content) {
            fence_open = Some(open);
        } else if is_math_fence(content) {
            math_open = true;
        }
    }
    fence_open.is_some() || math_open
}

fn split_at(text: &str, at: usize) -> Split {
    let head = text[..at].trim_end_matches('\n');
    let tail_start = at + (text[at..].len() - text[at..].trim_start_matches('\n').len());
    Split {
        pieces: vec![
            Piece {
                text: head.to_string(),
                source_start: 0,
            },
            Piece {
                text: text[tail_start..].to_string(),
                source_start: utf16_of_byte(text, tail_start),
            },
        ],
        keep: 0,
        caret_piece: 1,
        caret: 0,
    }
}

/// Removes the line `[start, end)` and splits the block around it, leaving an
/// empty block where it was (leaving a list or quote).
fn exit_at_line(text: &str, start: usize, end: usize) -> Split {
    let head = text[..start].trim_end_matches('\n');
    let tail_start = (end + 1).min(text.len());
    let tail = &text[tail_start..];
    let mut pieces = Vec::new();
    let mut keep = 0;
    if !head.is_empty() {
        pieces.push(Piece {
            text: head.to_string(),
            source_start: 0,
        });
    }
    let caret_piece = pieces.len();
    pieces.push(Piece {
        text: String::new(),
        source_start: utf16_of_byte(text, start),
    });
    if head.is_empty() {
        keep = caret_piece;
    }
    if !tail.trim().is_empty() {
        pieces.push(Piece {
            text: tail.to_string(),
            source_start: utf16_of_byte(text, tail_start),
        });
    }
    Split {
        pieces,
        keep,
        caret_piece,
        caret: 0,
    }
}

/// What Enter does with a collapsed caret at UTF-16 offset `caret`.
///
/// Inside code and tables Enter is a newline. In a list or quote it starts
/// the next item, and on an empty item it leaves the list. Anywhere else it
/// ends the block at the caret and continues in a new one.
pub fn enter_action(text: &str, caret: u32) -> EnterAction {
    let at = byte_of_utf16(text, caret);
    if inside_fence(text, at) {
        return EnterAction::Default;
    }
    let line = lines(text)
        .into_iter()
        .find(|line| line.start <= at && at <= line.end)
        .unwrap_or(Line {
            start: 0,
            end: text.len(),
        });
    let content = &text[line.start..line.end];
    if content.trim_start().starts_with('|') {
        return EnterAction::Default;
    }

    if let Some(item) = list_item(content) {
        let content_start = line.start + item.content;
        if at < content_start {
            return EnterAction::Default;
        }
        if text[content_start..line.end].trim().is_empty() {
            return EnterAction::Split(exit_at_line(text, line.start, line.end));
        }
        let mut prefix = format!("\n{}{} ", item.indent, next_marker(&item.marker));
        if item.task {
            prefix.push_str("[ ] ");
        }
        let rest = text[at..line.end].trim_start();
        let rest_start = line.end - rest.len();
        let new_text = format!(
            "{}{}{}",
            text[..at].trim_end_matches(' '),
            prefix,
            &text[rest_start..]
        );
        let caret = utf16_of_byte(
            &new_text,
            text[..at].trim_end_matches(' ').len() + prefix.len(),
        );
        return EnterAction::Replace {
            text: new_text,
            caret,
        };
    }

    let trimmed = content.trim_start();
    if trimmed.starts_with('>') {
        let marker_end = line.start + (content.len() - trimmed.len()) + 1;
        let body_start = marker_end
            + (text[marker_end..line.end].len() - text[marker_end..line.end].trim_start().len());
        if at < body_start {
            return EnterAction::Default;
        }
        if text[body_start..line.end].trim().is_empty() {
            return EnterAction::Split(exit_at_line(text, line.start, line.end));
        }
        let quote_prefix = &content[..content.len() - trimmed.len()];
        let insert = format!("\n{quote_prefix}> ");
        let new_text = format!("{}{}{}", &text[..at], insert, &text[at..]);
        let caret = utf16_of_byte(&new_text, at + insert.len());
        return EnterAction::Replace {
            text: new_text,
            caret,
        };
    }

    if at == 0 && !text.is_empty() {
        // Enter at the very start pushes the block down.
        return EnterAction::Split(Split {
            pieces: vec![
                Piece {
                    text: String::new(),
                    source_start: 0,
                },
                Piece {
                    text: text.to_string(),
                    source_start: 0,
                },
            ],
            keep: 1,
            caret_piece: 1,
            caret: 0,
        });
    }
    EnterAction::Split(split_at(text, at))
}

/// The text of two blocks joined by Backspace at the start of the second,
/// and the caret at the join.
pub fn join(previous: &str, next: &str) -> (String, u32) {
    (format!("{previous}{next}"), utf16_len(previous))
}

/// Maps a click in rendered text back to source.
///
/// `rendered` is the visible text from the start of the clicked element up
/// to the click; `start` is the UTF-16 offset in `source` where that element
/// begins. Markup (`**`, `[`, `](url)`, ...) has no visible text, so visible
/// characters are matched in order and anything else in the source is
/// skipped. Returns a UTF-16 offset in `source`.
pub fn source_offset_for_rendered(source: &str, start: u32, rendered: &str) -> u32 {
    let start_byte = byte_of_utf16(source, start);
    let tail = &source[start_byte..];
    let mut matched_end = 0usize;
    let mut chars = tail.char_indices().peekable();
    // Leading markup of the element (`## `, `- `, `> `) is never visible.
    let mut wanted = rendered.chars().filter(|c| *c != '\u{200b}').peekable();
    let mut misses = 0;
    while let Some(want) = wanted.peek().copied() {
        let Some((index, have)) = chars.next() else {
            break;
        };
        let same = have == want || (have.is_whitespace() && want.is_whitespace());
        if same {
            wanted.next();
            matched_end = index + have.len_utf8();
            misses = 0;
            // Collapse runs of source whitespace onto one visible space.
            if have.is_whitespace() {
                while wanted.peek().is_some_and(|c| c.is_whitespace()) {
                    wanted.next();
                }
            }
        } else {
            misses += 1;
            if misses > 400 {
                break;
            }
        }
    }
    utf16_of_byte(source, start_byte + matched_end)
}

/// A command offered by the `/` menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlashCommand {
    pub id: &'static str,
    pub label: &'static str,
    /// Extra words the menu matches on.
    pub keywords: &'static str,
    /// Icon name for the host's icon set.
    pub icon: &'static str,
    /// Shortcut shown as a hint (the Markdown that does the same).
    pub hint: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        id: "text",
        label: "Text",
        keywords: "paragraph plain",
        icon: "text",
        hint: "",
    },
    SlashCommand {
        id: "h1",
        label: "Heading 1",
        keywords: "title h1 big",
        icon: "heading-1",
        hint: "#",
    },
    SlashCommand {
        id: "h2",
        label: "Heading 2",
        keywords: "subtitle h2",
        icon: "heading-2",
        hint: "##",
    },
    SlashCommand {
        id: "h3",
        label: "Heading 3",
        keywords: "h3 small",
        icon: "heading-3",
        hint: "###",
    },
    SlashCommand {
        id: "bullet",
        label: "Bulleted list",
        keywords: "ul unordered points",
        icon: "list",
        hint: "-",
    },
    SlashCommand {
        id: "numbered",
        label: "Numbered list",
        keywords: "ol ordered steps",
        icon: "list-ordered",
        hint: "1.",
    },
    SlashCommand {
        id: "todo",
        label: "To-do list",
        keywords: "task checkbox check",
        icon: "check-square",
        hint: "[ ]",
    },
    SlashCommand {
        id: "quote",
        label: "Quote",
        keywords: "blockquote citation",
        icon: "quote",
        hint: ">",
    },
    SlashCommand {
        id: "code",
        label: "Code block",
        keywords: "pre snippet fence",
        icon: "code",
        hint: "```",
    },
    SlashCommand {
        id: "table",
        label: "Table",
        keywords: "grid columns rows",
        icon: "table",
        hint: "|",
    },
    SlashCommand {
        id: "math",
        label: "Math block",
        keywords: "equation formula tex latex",
        icon: "sigma",
        hint: "$$",
    },
    SlashCommand {
        id: "space",
        label: "Writing space",
        keywords: "ink draw sketch canvas pen",
        icon: "space",
        hint: "",
    },
    SlashCommand {
        id: "divider",
        label: "Divider",
        keywords: "rule line separator hr",
        icon: "minus",
        hint: "---",
    },
];

/// The `/command` being typed before the caret: `(offset of '/', query)`.
/// A slash only opens the menu at the start of a line or after a space, so
/// paths, URLs and "and/or" never do.
pub fn slash_query(text: &str, caret: u32) -> Option<(u32, String)> {
    let at = byte_of_utf16(text, caret);
    let before = &text[..at];
    let slash = before.rfind('/')?;
    let query = &before[slash + 1..];
    if query.chars().count() > 24 || query.contains(char::is_whitespace) {
        return None;
    }
    let opener = before[..slash].chars().next_back();
    if !opener.is_none_or(char::is_whitespace) {
        return None;
    }
    if inside_fence(text, slash) {
        return None;
    }
    Some((utf16_of_byte(text, slash), query.to_string()))
}

/// Commands matching `query`, best first: label prefix, then word prefix,
/// then anywhere in the label or keywords.
pub fn filter_commands(query: &str) -> Vec<&'static SlashCommand> {
    let query = query.to_lowercase();
    let mut ranked = SLASH_COMMANDS
        .iter()
        .filter_map(|command| {
            let label = command.label.to_lowercase();
            let rank = if query.is_empty() || label.starts_with(&query) {
                0
            } else if label
                .split_whitespace()
                .any(|word| word.starts_with(&query))
            {
                1
            } else if label.contains(&query)
                || command
                    .keywords
                    .split_whitespace()
                    .any(|word| word.starts_with(&query))
            {
                2
            } else {
                return None;
            };
            Some((rank, command))
        })
        .collect::<Vec<_>>();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, command)| command).collect()
}

/// What running a `/` command does to the block it was typed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandEdit {
    /// The block's new text and caret.
    Text { text: String, caret: u32 },
    /// A non-text block (`math`, `space`, `divider`) goes in. `remaining`
    /// is the block's text without the command; when it is empty the new
    /// block takes this block's place.
    Block {
        kind: &'static str,
        remaining: String,
        caret: u32,
    },
}

/// Length in bytes of a heading, list or quote marker opening `line`.
fn marker_len(line: &str) -> usize {
    if let Some(level) = heading_level(line) {
        let indent = line.len() - line.trim_start().len();
        let after = &line[indent + level as usize..];
        return indent + level as usize + (after.len() - after.trim_start().len());
    }
    if let Some(item) = list_item(line) {
        return item.content;
    }
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix('>') {
        return line.len() - rest.trim_start().len();
    }
    0
}

/// Runs command `id`, typed as `/query` from UTF-16 `start` to `caret`.
pub fn apply_command(text: &str, start: u32, caret: u32, id: &str) -> CommandEdit {
    let start_byte = byte_of_utf16(text, start);
    let caret_byte = byte_of_utf16(text, caret).max(start_byte);
    let base = format!("{}{}", &text[..start_byte], &text[caret_byte..]);
    let line = lines(&base)
        .into_iter()
        .find(|line| line.start <= start_byte && start_byte <= line.end)
        .unwrap_or(Line {
            start: 0,
            end: base.len(),
        });
    let content = &base[line.start..line.end];
    let marker = marker_len(content);
    let body = &content[marker..];
    let caret_in_body = start_byte.saturating_sub(line.start + marker);

    let prefix = match id {
        "h1" => "# ",
        "h2" => "## ",
        "h3" => "### ",
        "bullet" => "- ",
        "numbered" => "1. ",
        "todo" => "- [ ] ",
        "quote" => "> ",
        "text" => "",
        "code" => {
            let fenced = format!("```\n{body}\n```");
            let text = format!("{}{}{}", &base[..line.start], fenced, &base[line.end..]);
            let caret = utf16_of_byte(&text, line.start + 4 + body.len());
            return CommandEdit::Text { text, caret };
        }
        "table" => {
            let header = if body.trim().is_empty() {
                "Column"
            } else {
                body.trim()
            };
            let table = format!("| {header} | Column |\n| --- | --- |\n|  |  |");
            let text = format!("{}{}{}", &base[..line.start], table, &base[line.end..]);
            let caret = utf16_of_byte(&text, line.start + 2 + header.len());
            return CommandEdit::Text { text, caret };
        }
        "math" => {
            return CommandEdit::Block {
                kind: "math",
                remaining: base,
                caret: start,
            };
        }
        "space" => {
            return CommandEdit::Block {
                kind: "space",
                remaining: base,
                caret: start,
            };
        }
        "divider" => {
            return CommandEdit::Block {
                kind: "divider",
                remaining: base,
                caret: start,
            };
        }
        _ => {
            return CommandEdit::Text {
                text: base,
                caret: start,
            };
        }
    };
    let text = format!("{}{prefix}{body}{}", &base[..line.start], &base[line.end..]);
    let caret = utf16_of_byte(
        &text,
        line.start + prefix.len() + caret_in_body.min(body.len()),
    );
    CommandEdit::Text { text, caret }
}

/// Ticks or unticks the task on `line` (0-based) of `text`, including one
/// quoted with `>`. Returns the new text, or `None` if that line is not a
/// task.
pub fn toggle_task(text: &str, line: usize) -> Option<String> {
    let mut start = 0;
    for _ in 0..line {
        start += text[start..].find('\n')? + 1;
    }
    let end = text[start..].find('\n').map_or(text.len(), |at| start + at);
    let source = &text[start..end];
    let quoted = source.len()
        - source
            .trim_start_matches(|c: char| c == '>' || c == ' ' || c == '\t')
            .len();
    // Keep the indent of a nested item when there is no quote.
    let quoted = if source[..quoted].contains('>') {
        quoted
    } else {
        0
    };
    let item = list_item(&source[quoted..])?;
    if !item.task {
        return None;
    }
    let box_at = start + quoted + source[quoted..][..item.content].rfind('[')?;
    let ticked = text[box_at + 1..].starts_with(['x', 'X']);
    let mut next = String::with_capacity(text.len());
    next.push_str(&text[..box_at + 1]);
    next.push(if ticked { ' ' } else { 'x' });
    next.push_str(&text[box_at + 2..]);
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(split: &Split) -> Vec<&str> {
        split
            .pieces
            .iter()
            .map(|piece| piece.text.as_str())
            .collect()
    }

    #[test]
    fn splits_paragraphs_headings_and_keeps_lists_and_fences_whole() {
        let text = "# Title\nIntro line\nsecond line\n\n- a\n- b\n\n  more of b\n- c\n\n```rust\nfn x() {\n\n}\n```\n\nEnd";
        let split = split_elements(text).expect("splits");
        assert_eq!(
            texts(&split),
            vec![
                "# Title",
                "Intro line\nsecond line",
                "- a\n- b\n\n  more of b\n- c",
                "```rust\nfn x() {\n\n}\n```",
                "End"
            ]
        );
        assert_eq!(split.pieces[1].source_start, 8);
        assert!(split_elements("just one paragraph\nwith two lines").is_none());
        assert!(split_elements("").is_none());
    }

    #[test]
    fn setext_rules_math_and_definitions() {
        let split = split_elements("Title\n=====\nbody\n\n---\n\n$$\na\n\nb\n$$").unwrap();
        assert_eq!(
            texts(&split),
            vec!["Title\n=====", "body", "---", "$$\na\n\nb\n$$"]
        );
        assert!(split_elements("See [x].\n\n[x]: https://example.com").is_none());
        let front = split_elements("---\ntags: a\n---\n\nHello").unwrap();
        assert_eq!(texts(&front), vec!["---\ntags: a\n---", "Hello"]);
    }

    #[test]
    fn enter_splits_paragraphs_at_the_caret() {
        let text = "Hello world";
        let EnterAction::Split(split) = enter_action(text, 5) else {
            panic!("expected split");
        };
        assert_eq!(texts(&split), vec!["Hello", " world"]);
        assert_eq!((split.keep, split.caret_piece, split.caret), (0, 1, 0));

        let EnterAction::Split(end) = enter_action(text, 11) else {
            panic!("expected split");
        };
        assert_eq!(texts(&end), vec!["Hello world", ""]);

        let EnterAction::Split(start) = enter_action(text, 0) else {
            panic!("expected split");
        };
        assert_eq!(texts(&start), vec!["", "Hello world"]);
        assert_eq!((start.keep, start.caret_piece), (1, 1));
    }

    #[test]
    fn enter_continues_and_leaves_lists_and_quotes() {
        assert_eq!(
            enter_action("- one", 5),
            EnterAction::Replace {
                text: "- one\n- ".into(),
                caret: 8
            }
        );
        assert_eq!(
            enter_action("9. nine", 7),
            EnterAction::Replace {
                text: "9. nine\n10. ".into(),
                caret: 12
            }
        );
        assert_eq!(
            enter_action("- [x] done", 10),
            EnterAction::Replace {
                text: "- [x] done\n- [ ] ".into(),
                caret: 17
            }
        );
        assert_eq!(
            enter_action("> quoted", 8),
            EnterAction::Replace {
                text: "> quoted\n> ".into(),
                caret: 11
            }
        );
        // Splitting an item in the middle carries the rest into the new item.
        assert_eq!(
            enter_action("- onetwo", 5),
            EnterAction::Replace {
                text: "- one\n- two".into(),
                caret: 8
            }
        );

        let EnterAction::Split(exit) = enter_action("- one\n- ", 8) else {
            panic!("expected exit");
        };
        assert_eq!(texts(&exit), vec!["- one", ""]);
        assert_eq!((exit.keep, exit.caret_piece), (0, 1));

        let EnterAction::Split(middle) = enter_action("- a\n- \n- b", 6) else {
            panic!("expected exit");
        };
        assert_eq!(texts(&middle), vec!["- a", "", "- b"]);

        let EnterAction::Split(only) = enter_action("- ", 2) else {
            panic!("expected exit");
        };
        assert_eq!(texts(&only), vec![""]);
        assert_eq!(only.keep, 0);
    }

    #[test]
    fn enter_is_a_newline_in_code_and_tables() {
        assert_eq!(enter_action("```rust", 7), EnterAction::Default);
        assert_eq!(enter_action("```\ncode", 8), EnterAction::Default);
        assert_eq!(enter_action("$$\nx", 4), EnterAction::Default);
        assert_eq!(enter_action("| a | b |", 9), EnterAction::Default);
        assert!(matches!(
            enter_action("```\ncode\n```", 12),
            EnterAction::Split(_)
        ));
    }

    #[test]
    fn split_locates_original_offsets() {
        let split = split_elements("one\n\ntwo\n\nthree").unwrap();
        assert_eq!(split.locate(0), (0, 0));
        assert_eq!(split.locate(2), (0, 2));
        assert_eq!(split.locate(4), (0, 3));
        assert_eq!(split.locate(6), (1, 1));
        assert_eq!(split.locate(14), (2, 4));
    }

    #[test]
    fn classifies_blocks() {
        assert_eq!(source_kind(""), SourceKind::Empty);
        assert_eq!(source_kind("## Momentum"), SourceKind::Heading(2));
        assert_eq!(source_kind("#hashtag"), SourceKind::Paragraph);
        assert_eq!(source_kind("1. a"), SourceKind::List);
        assert_eq!(source_kind("- [ ] task"), SourceKind::List);
        assert_eq!(source_kind("> q"), SourceKind::Quote);
        assert_eq!(source_kind("```\nx\n```"), SourceKind::Code);
        assert_eq!(source_kind("| a |"), SourceKind::Table);
        assert_eq!(source_kind("---"), SourceKind::Rule);
        assert_eq!(source_kind("$$\nx\n$$"), SourceKind::Math);
    }

    #[test]
    fn rendered_clicks_map_back_to_source() {
        let source = "Some **bold** and [a link](https://x.y) here";
        assert_eq!(source_offset_for_rendered(source, 0, "Some bo"), 9);
        assert_eq!(source_offset_for_rendered(source, 0, "Some bold"), 11);
        assert_eq!(
            source_offset_for_rendered(source, 0, "Some bold and a l"),
            22
        );
        assert_eq!(source_offset_for_rendered(source, 0, ""), 0);
        let heading = "## Momentum";
        assert_eq!(source_offset_for_rendered(heading, 0, "Mom"), 6);
        let lines = "first\nsecond";
        assert_eq!(source_offset_for_rendered(lines, 0, "first sec"), 9);
        let emoji = "a😀b";
        assert_eq!(source_offset_for_rendered(emoji, 0, "a😀"), 3);
    }

    #[test]
    fn utf16_byte_conversions() {
        let text = "hé😀x";
        assert_eq!(byte_of_utf16(text, 0), 0);
        assert_eq!(byte_of_utf16(text, 2), 3);
        assert_eq!(byte_of_utf16(text, 4), 7);
        assert_eq!(byte_of_utf16(text, 99), text.len());
        assert_eq!(utf16_of_byte(text, 7), 4);
        assert_eq!(join("ab", "cd"), ("abcd".into(), 2));
    }

    #[test]
    fn slash_opens_only_where_a_command_can_start() {
        assert_eq!(slash_query("/", 1), Some((0, String::new())));
        assert_eq!(slash_query("note /hea", 9), Some((5, "hea".into())));
        assert_eq!(slash_query("a\n/li", 5), Some((2, "li".into())));
        assert_eq!(slash_query("and/or", 6), None);
        assert_eq!(slash_query("https://x", 9), None);
        assert_eq!(slash_query("/two words", 10), None);
        assert_eq!(slash_query("```\n/x", 6), None);
    }

    #[test]
    fn commands_filter_by_label_and_keywords() {
        let ids = |query: &str| {
            filter_commands(query)
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("").len(), SLASH_COMMANDS.len());
        assert_eq!(ids("head"), vec!["h1", "h2", "h3"]);
        assert_eq!(ids("eq"), vec!["math"]);
        assert_eq!(ids("list")[0], "bullet");
        assert!(ids("zzz").is_empty());
    }

    #[test]
    fn commands_rewrite_the_line_they_were_typed_on() {
        assert_eq!(
            apply_command("/h2", 0, 3, "h2"),
            CommandEdit::Text {
                text: "## ".into(),
                caret: 3
            }
        );
        assert_eq!(
            apply_command("Momentum /hea", 9, 13, "h1"),
            CommandEdit::Text {
                text: "# Momentum ".into(),
                caret: 11
            }
        );
        // An existing marker is replaced, not stacked.
        assert_eq!(
            apply_command("## Title/bul", 8, 12, "bullet"),
            CommandEdit::Text {
                text: "- Title".into(),
                caret: 7
            }
        );
        assert_eq!(
            apply_command("- item /text", 7, 12, "text"),
            CommandEdit::Text {
                text: "item ".into(),
                caret: 5
            }
        );
        assert_eq!(
            apply_command("/code", 0, 5, "code"),
            CommandEdit::Text {
                text: "```\n\n```".into(),
                caret: 4
            }
        );
        assert_eq!(
            apply_command("/math", 0, 5, "math"),
            CommandEdit::Block {
                kind: "math",
                remaining: String::new(),
                caret: 0
            }
        );
        assert_eq!(
            apply_command("/todo", 0, 5, "todo"),
            CommandEdit::Text {
                text: "- [ ] ".into(),
                caret: 6
            }
        );
    }

    #[test]
    fn tasks_toggle_on_their_line() {
        let text = "Plan\n- [ ] one\n  - [x] two\n> - [ ] quoted\n- plain";
        assert_eq!(
            toggle_task(text, 1).unwrap(),
            "Plan\n- [x] one\n  - [x] two\n> - [ ] quoted\n- plain"
        );
        assert_eq!(
            toggle_task(text, 2).unwrap(),
            "Plan\n- [ ] one\n  - [ ] two\n> - [ ] quoted\n- plain"
        );
        assert_eq!(
            toggle_task(text, 3).unwrap(),
            "Plan\n- [ ] one\n  - [x] two\n> - [x] quoted\n- plain"
        );
        assert_eq!(toggle_task(text, 0), None);
        assert_eq!(toggle_task(text, 4), None);
        assert_eq!(toggle_task(text, 9), None);
        assert_eq!(toggle_task("1. [X] done", 0).unwrap(), "1. [ ] done");
    }
}
