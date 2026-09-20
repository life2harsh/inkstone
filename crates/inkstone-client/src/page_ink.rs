//! Platform-independent page ink layout logic.
//!
//! The web layer measures the DOM and feeds plain geometry in here; this
//! module decides which group a new stroke joins, where a group is anchored,
//! how tall writing spaces are, and how source positions map to the rendered
//! elements that carry `data-sourcepos`.

use inkstone_core::{AnchorTarget, InkAnchor, InkGroupId, RelativePosition};

/// A stroke starting within this distance of the group drawn last joins it.
pub const JOIN_DISTANCE: f32 = 48.0;
/// ... if it starts within this many milliseconds of that group's last stroke.
pub const JOIN_WINDOW_MS: f64 = 2_500.0;
/// A stroke starting this close to any group's ink joins that group.
pub const TOUCH_DISTANCE: f32 = 6.0;
/// Room kept below the lowest ink in a writing space.
pub const SPACE_PADDING: f32 = 32.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl Rect {
    pub fn new(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    pub fn translate(self, dx: f32, dy: f32) -> Self {
        Self::new(
            self.min_x + dx,
            self.min_y + dy,
            self.max_x + dx,
            self.max_y + dy,
        )
    }

    pub fn distance_to(self, x: f32, y: f32) -> f32 {
        let dx = (self.min_x - x).max(0.0).max(x - self.max_x);
        let dy = (self.min_y - y).max(0.0).max(y - self.max_y);
        (dx * dx + dy * dy).sqrt()
    }

    pub fn area(self) -> f32 {
        (self.max_x - self.min_x).max(0.0) * (self.max_y - self.min_y).max(0.0)
    }
}

/// A group as currently laid out on the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGroup {
    pub group: InkGroupId,
    /// Page coordinates of the group's frame origin.
    pub origin: (f32, f32),
    /// Ink bounds in frame-local coordinates.
    pub bounds: Rect,
}

impl PlacedGroup {
    pub fn page_bounds(&self) -> Rect {
        self.bounds.translate(self.origin.0, self.origin.1)
    }
}

/// The group drawn into most recently, for stroke continuation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecentGroup {
    pub group: InkGroupId,
    pub at_ms: f64,
}

/// Picks the group a stroke starting at `point` belongs to, or `None` for a
/// new group.
///
/// Writing a word or a sketch is several strokes in quick succession close
/// together; those stay one group. Starting a stroke on top of existing ink
/// adds to that ink. Anything else starts a new group with its own anchor.
pub fn choose_group(
    groups: &[PlacedGroup],
    point: (f32, f32),
    now_ms: f64,
    recent: Option<RecentGroup>,
) -> Option<InkGroupId> {
    if let Some(recent) = recent
        && now_ms - recent.at_ms <= JOIN_WINDOW_MS
        && let Some(placed) = groups.iter().find(|g| g.group == recent.group)
        && placed.page_bounds().distance_to(point.0, point.1) <= JOIN_DISTANCE
    {
        return Some(recent.group);
    }
    groups
        .iter()
        .filter(|g| g.page_bounds().distance_to(point.0, point.1) <= TOUCH_DISTANCE)
        .min_by(|a, b| a.page_bounds().area().total_cmp(&b.page_bounds().area()))
        .map(|g| g.group)
}

/// What sits under the point where a new group starts, as measured by the
/// host. Origins are page coordinates of the anchoring element's top-left.
#[derive(Debug, Clone, PartialEq)]
pub enum AnchorCandidate {
    Text {
        position: RelativePosition,
        origin: (f32, f32),
    },
    Block {
        block_id: inkstone_core::BlockId,
        origin: (f32, f32),
    },
    Page,
}

/// Builds the anchor for a new group whose frame origin is `frame_origin`.
pub fn anchor_for(candidate: AnchorCandidate, frame_origin: (f32, f32)) -> InkAnchor {
    let (target, origin) = match candidate {
        AnchorCandidate::Text { position, origin } => (AnchorTarget::Text { position }, origin),
        AnchorCandidate::Block { block_id, origin } => (AnchorTarget::Block { block_id }, origin),
        AnchorCandidate::Page => (AnchorTarget::Page, (0.0, 0.0)),
    };
    InkAnchor {
        target,
        dx: frame_origin.0 - origin.0,
        dy: frame_origin.1 - origin.1,
    }
}

/// Rendered height of a writing space given the bottoms (relative to the
/// space's top) of the ink anchored to it.
pub fn space_height(min_height: f32, ink_bottoms: impl IntoIterator<Item = f32>) -> f32 {
    ink_bottoms
        .into_iter()
        .filter(|bottom| bottom.is_finite())
        .map(|bottom| bottom + SPACE_PADDING)
        .fold(min_height, f32::max)
}

/// A rendered element carrying comrak's `data-sourcepos="l1:c1-l2:c2"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

pub fn parse_sourcepos(value: &str) -> Option<SourceSpan> {
    let (start, end) = value.split_once('-')?;
    let (start_line, start_col) = start.split_once(':')?;
    let (end_line, end_col) = end.split_once(':')?;
    Some(SourceSpan {
        start_line: start_line.trim().parse().ok()?,
        start_col: start_col.trim().parse().ok()?,
        end_line: end_line.trim().parse().ok()?,
        end_col: end_col.trim().parse().ok()?,
    })
}

/// Index of the innermost span (smallest line range, latest start) that
/// contains `line`, so a list item wins over its list.
pub fn innermost_span_for_line(spans: &[SourceSpan], line: u32) -> Option<usize> {
    spans
        .iter()
        .enumerate()
        .filter(|(_, span)| span.start_line <= line && line <= span.end_line)
        .min_by_key(|(_, span)| {
            (
                span.end_line - span.start_line,
                std::cmp::Reverse(span.start_line),
            )
        })
        .map(|(index, _)| index)
        .or_else(|| {
            // Blank lines between elements belong to the element above.
            spans
                .iter()
                .enumerate()
                .filter(|(_, span)| span.end_line < line)
                .max_by_key(|(_, span)| (span.end_line, std::cmp::Reverse(span.start_line)))
                .map(|(index, _)| index)
        })
}

/// 1-based line number containing UTF-16 offset `offset` of `text`.
pub fn line_of_utf16_offset(text: &str, offset: u32) -> u32 {
    let mut line = 1;
    let mut seen = 0u32;
    for ch in text.chars() {
        if seen >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
        }
        seen += ch.len_utf16() as u32;
    }
    line
}

/// UTF-16 offset of 1-based `line` and 1-based byte column `col`, clamped to
/// the text. Comrak reports columns in bytes.
pub fn utf16_offset_of_line_col(text: &str, line: u32, col: u32) -> u32 {
    let mut current_line = 1;
    let mut line_start_byte = 0usize;
    if line > 1 {
        for (index, ch) in text.char_indices() {
            if ch == '\n' {
                current_line += 1;
                if current_line == line {
                    line_start_byte = index + 1;
                    break;
                }
            }
        }
        if current_line < line {
            return text.encode_utf16().count() as u32;
        }
    }
    let line_end_byte = text[line_start_byte..]
        .find('\n')
        .map(|end| line_start_byte + end)
        .unwrap_or(text.len());
    let mut target = line_start_byte + col.saturating_sub(1) as usize;
    target = target.min(line_end_byte);
    while !text.is_char_boundary(target) {
        target -= 1;
    }
    text[..target].encode_utf16().count() as u32
}

fn is_space_unit(unit: u16) -> bool {
    matches!(unit, 0x20 | 0x09 | 0x0A | 0x0D | 0xA0)
}

const ALIGN_WINDOW: usize = 48;

pub fn align_rendered_to_source(source: &[u16], rendered: &[u16]) -> Vec<Option<usize>> {
    let mut cursor = 0usize;
    rendered
        .iter()
        .map(|&unit| {
            let limit = source.len().min(cursor + ALIGN_WINDOW);
            let found = (cursor..limit).find(|&index| {
                source[index] == unit || (is_space_unit(unit) && is_space_unit(source[index]))
            });
            if let Some(index) = found {
                cursor = index + 1;
            }
            found
        })
        .collect()
}

pub fn rendered_to_source_index(source: &[u16], rendered: &[u16], rendered_index: usize) -> usize {
    let map = align_rendered_to_source(source, rendered);
    map.iter()
        .skip(rendered_index)
        .find_map(|index| *index)
        .or_else(|| {
            map.iter()
                .take(rendered_index)
                .rev()
                .find_map(|index| index.map(|index| index + 1))
        })
        .unwrap_or(0)
        .min(source.len())
}

pub fn source_to_rendered_index(source: &[u16], rendered: &[u16], source_index: usize) -> usize {
    let map = align_rendered_to_source(source, rendered);
    map.iter()
        .position(|index| index.is_some_and(|index| index >= source_index))
        .unwrap_or(rendered.len())
}

pub fn word_start(rendered: &[u16], index: usize) -> usize {
    let is_word = |unit: u16| {
        char::from_u32(u32::from(unit)).is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
            || (0xD800..0xE000).contains(&unit)
    };
    let mut index = index.min(rendered.len());
    if index < rendered.len() && !is_word(rendered[index]) {
        if index == 0 || !is_word(rendered[index - 1]) {
            return index;
        }
    }
    while index > 0 && is_word(rendered[index - 1]) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::BlockId;

    fn group(origin: (f32, f32), bounds: Rect) -> PlacedGroup {
        PlacedGroup {
            group: InkGroupId::new_v4(),
            origin,
            bounds,
        }
    }

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn rendered_offsets_map_through_markup_to_source() {
        let source = units("Some **bold** and [a link](https://x.y) here");
        let rendered = units("Some bold and a link here");
        let bold = 5;
        assert_eq!(rendered_to_source_index(&source, &rendered, bold), 7);
        let here = rendered.len() - 4;
        let expected = source.len() - 4;
        assert_eq!(rendered_to_source_index(&source, &rendered, here), expected);
        assert_eq!(source_to_rendered_index(&source, &rendered, 7), bold);
        assert_eq!(source_to_rendered_index(&source, &rendered, expected), here);
        assert_eq!(source_to_rendered_index(&source, &rendered, 5), bold);
    }

    #[test]
    fn unmatched_rendered_text_does_not_derail_alignment() {
        let source = units("x $a^2$ word");
        let rendered = units("x a2a2\u{200b} word");
        let word = rendered.len() - 4;
        assert_eq!(
            rendered_to_source_index(&source, &rendered, word),
            source.len() - 4
        );
    }

    #[test]
    fn word_start_snaps_to_the_beginning_of_the_word() {
        let text = units("hello wonderful world");
        assert_eq!(word_start(&text, 9), 6);
        assert_eq!(word_start(&text, 6), 6);
        assert_eq!(word_start(&text, 5), 0);
        assert_eq!(word_start(&units("a  b"), 2), 2);
        assert_eq!(word_start(&text, 0), 0);
    }

    #[test]
    fn continues_recent_group_when_close_in_time_and_space() {
        let word = group((100.0, 100.0), Rect::new(0.0, 0.0, 40.0, 20.0));
        let recent = Some(RecentGroup {
            group: word.group,
            at_ms: 1_000.0,
        });
        assert_eq!(
            choose_group(&[word], (170.0, 110.0), 2_000.0, recent),
            Some(word.group)
        );
        assert_eq!(choose_group(&[word], (170.0, 110.0), 9_000.0, recent), None);
        assert_eq!(choose_group(&[word], (400.0, 110.0), 2_000.0, recent), None);
    }

    #[test]
    fn starting_on_existing_ink_joins_the_tightest_group() {
        let big = group((0.0, 0.0), Rect::new(0.0, 0.0, 500.0, 500.0));
        let small = group((100.0, 100.0), Rect::new(0.0, 0.0, 20.0, 20.0));
        assert_eq!(
            choose_group(&[big, small], (110.0, 110.0), 0.0, None),
            Some(small.group)
        );
        assert_eq!(
            choose_group(&[big, small], (300.0, 300.0), 0.0, None),
            Some(big.group)
        );
        assert_eq!(choose_group(&[big, small], (900.0, 900.0), 0.0, None), None);
    }

    #[test]
    fn anchors_store_offset_from_the_anchoring_element() {
        let block_id = BlockId::new_v4();
        let anchor = anchor_for(
            AnchorCandidate::Block {
                block_id,
                origin: (0.0, 200.0),
            },
            (40.0, 260.0),
        );
        assert_eq!(anchor.target, AnchorTarget::Block { block_id });
        assert_eq!((anchor.dx, anchor.dy), (40.0, 60.0));

        let page = anchor_for(AnchorCandidate::Page, (5.0, 6.0));
        assert_eq!(
            (page.target, page.dx, page.dy),
            (AnchorTarget::Page, 5.0, 6.0)
        );
    }

    #[test]
    fn space_grows_to_fit_its_ink() {
        assert_eq!(space_height(160.0, []), 160.0);
        assert_eq!(space_height(160.0, [100.0]), 160.0);
        assert_eq!(space_height(160.0, [100.0, 300.0]), 300.0 + SPACE_PADDING);
        assert_eq!(space_height(160.0, [f32::NAN]), 160.0);
    }

    #[test]
    fn sourcepos_parses_and_innermost_span_wins() {
        assert_eq!(
            parse_sourcepos("3:1-5:12"),
            Some(SourceSpan {
                start_line: 3,
                start_col: 1,
                end_line: 5,
                end_col: 12
            })
        );
        assert_eq!(parse_sourcepos("nonsense"), None);

        let list = SourceSpan {
            start_line: 3,
            start_col: 1,
            end_line: 6,
            end_col: 5,
        };
        let item = SourceSpan {
            start_line: 4,
            start_col: 1,
            end_line: 4,
            end_col: 9,
        };
        let heading = SourceSpan {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 7,
        };
        let spans = [heading, list, item];
        assert_eq!(innermost_span_for_line(&spans, 4), Some(2));
        assert_eq!(innermost_span_for_line(&spans, 5), Some(1));
        assert_eq!(innermost_span_for_line(&spans, 1), Some(0));
        assert_eq!(innermost_span_for_line(&spans, 2), Some(0));
        assert_eq!(innermost_span_for_line(&spans, 9), Some(1));
        assert_eq!(innermost_span_for_line(&[], 1), None);
    }

    #[test]
    fn utf16_offsets_and_lines_roundtrip() {
        let text = "# Title\n\nhé😀 world\nlast";
        assert_eq!(line_of_utf16_offset(text, 0), 1);
        let third = utf16_offset_of_line_col(text, 3, 1);
        assert_eq!(third, 9);
        assert_eq!(line_of_utf16_offset(text, third), 3);
        // byte column 4 is after "hé" (h=1 byte, é=2 bytes).
        assert_eq!(utf16_offset_of_line_col(text, 3, 4), 11);
        // columns past the end of the line clamp to the line end.
        assert_eq!(
            utf16_offset_of_line_col(text, 1, 99),
            "# Title".encode_utf16().count() as u32
        );
        assert_eq!(
            utf16_offset_of_line_col(text, 40, 1),
            text.encode_utf16().count() as u32
        );
        assert_eq!(line_of_utf16_offset(text, 999), 4);
    }
}
