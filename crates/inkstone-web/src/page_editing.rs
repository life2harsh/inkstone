//! Block editing on the page: one small Markdown block per element, Enter
//! and Backspace splitting and joining blocks, arrows walking between them,
//! and clicks placing the caret where the user clicked.
//!
//! The rules live in `inkstone_client::page_text`; this module measures the
//! DOM, issues the document ops and keeps ink anchored to the text it was
//! written next to while text moves between blocks.

use inkstone_client::ClientEvent;
use inkstone_client::page_ink;
use inkstone_client::page_text::{self, EnterAction, Split};
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::document::{
    AnchorTarget, BlockContent, DocumentBlock, InkAnchor, InkstoneDocument,
};
use inkstone_core::ids::{BlockId, InkGroupId, UpdateId};
use inkstone_core::{RelativePosition, TextAssociation, TextTarget};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::Driver;

/// Where a caret lands in the block being moved into.
#[derive(Clone, Copy)]
enum Landing {
    Start,
    End,
    /// The visible line nearest `y`, at `x` (client coordinates).
    Point(f64, f64),
}

fn block_selector(block_id: BlockId) -> String {
    format!("[data-page-block-id=\"{block_id}\"]")
}

fn block_top(block_id: BlockId) -> Option<f64> {
    web_sys::window()?
        .document()?
        .query_selector(&block_selector(block_id))
        .ok()??
        .get_bounding_client_rect()
        .top()
        .into()
}

fn editable_text(content: &BlockContent) -> Option<&str> {
    match content {
        BlockContent::Markdown { text } => Some(text),
        BlockContent::Math { source } => Some(source),
        _ => None,
    }
}

fn px(value: &str) -> Option<f64> {
    value.trim().strip_suffix("px")?.trim().parse().ok()
}

/// The DOM position under a client point: `(node, offset)`.
fn caret_from_point(document: &web_sys::Document, x: f64, y: f64) -> Option<(web_sys::Node, u32)> {
    let host: &JsValue = document.as_ref();
    for (method, node_key, offset_key) in [
        ("caretPositionFromPoint", "offsetNode", "offset"),
        ("caretRangeFromPoint", "startContainer", "startOffset"),
    ] {
        let Ok(function) = js_sys::Reflect::get(host, &method.into()) else {
            continue;
        };
        let Some(function) = function.dyn_ref::<js_sys::Function>() else {
            continue;
        };
        let Ok(result) = function.call2(host, &x.into(), &y.into()) else {
            continue;
        };
        if result.is_null() || result.is_undefined() {
            continue;
        }
        let node = js_sys::Reflect::get(&result, &node_key.into())
            .ok()?
            .dyn_into::<web_sys::Node>()
            .ok()?;
        let offset = js_sys::Reflect::get(&result, &offset_key.into())
            .ok()?
            .as_f64()? as u32;
        return Some((node, offset));
    }
    None
}

/// The source offset in `text` under a client point inside a rendered block.
pub(crate) fn source_caret_at_point(
    block: &web_sys::Element,
    text: &str,
    x: f64,
    y: f64,
) -> Option<u32> {
    let document = block.owner_document()?;
    let (node, offset) = caret_from_point(&document, x, y)?;
    if !block.contains(Some(&node)) {
        return None;
    }
    let element = match node.dyn_ref::<web_sys::Element>() {
        Some(element) => element.clone(),
        None => node.parent_element()?,
    };
    let positioned = element.closest("[data-sourcepos]").ok()??;
    if !block.contains(Some(&positioned)) {
        return None;
    }
    let span = page_ink::parse_sourcepos(&positioned.get_attribute("data-sourcepos")?)?;
    let start = page_ink::utf16_offset_of_line_col(text, span.start_line, span.start_col);
    let range = document.create_range().ok()?;
    range.set_start(&positioned, 0).ok()?;
    range.set_end(&node, offset).ok()?;
    let prefix = String::from(range.to_string());
    Some(page_text::source_offset_for_rendered(text, start, &prefix))
}

/// Where the caret of `textarea` sits visually.
pub(crate) struct CaretMetrics {
    pub first_line: bool,
    pub last_line: bool,
    /// Client x of the caret.
    pub x: f64,
    /// Client top and bottom of the caret's line.
    pub line_top: f64,
    pub line_bottom: f64,
}

/// Measures the caret with an off-screen copy of the textarea, since
/// textareas do not expose where their lines wrap.
pub(crate) fn caret_metrics(
    textarea: &web_sys::HtmlTextAreaElement,
    caret: u32,
) -> Option<CaretMetrics> {
    let window = web_sys::window()?;
    let document = window.document()?;
    let style = window.get_computed_style(textarea).ok()??;
    let mirror: web_sys::HtmlElement = document.create_element("div").ok()?.dyn_into().ok()?;
    let mirror_style = mirror.style();
    for property in [
        "font-family",
        "font-size",
        "font-weight",
        "font-style",
        "letter-spacing",
        "word-spacing",
        "line-height",
        "text-indent",
        "text-transform",
        "tab-size",
        "padding-top",
        "padding-right",
        "padding-bottom",
        "padding-left",
        "border-top-width",
        "border-right-width",
        "border-bottom-width",
        "border-left-width",
        "border-style",
        "box-sizing",
    ] {
        let _ = mirror_style.set_property(
            property,
            &style.get_property_value(property).unwrap_or_default(),
        );
    }
    for (property, value) in [
        ("position", "absolute"),
        ("visibility", "hidden"),
        ("top", "0"),
        ("left", "-10000px"),
        ("white-space", "pre-wrap"),
        ("overflow-wrap", "break-word"),
        ("height", "auto"),
    ] {
        let _ = mirror_style.set_property(property, value);
    }
    let _ = mirror_style.set_property("width", &format!("{}px", textarea.offset_width()));

    let value = textarea.value();
    let split = page_text::byte_of_utf16(&value, caret);
    mirror
        .append_child(&document.create_text_node(&value[..split]))
        .ok()?;
    let marker: web_sys::HtmlElement = document.create_element("span").ok()?.dyn_into().ok()?;
    marker.set_text_content(Some("\u{200b}"));
    mirror.append_child(&marker).ok()?;
    mirror
        .append_child(&document.create_text_node(&format!("{}\u{200b}", &value[split..])))
        .ok()?;
    document.body()?.append_child(&mirror).ok()?;

    let font_size = px(&style.get_property_value("font-size").unwrap_or_default()).unwrap_or(16.0);
    let line_height =
        px(&style.get_property_value("line-height").unwrap_or_default()).unwrap_or(font_size * 1.7);
    let padding_top =
        px(&style.get_property_value("padding-top").unwrap_or_default()).unwrap_or(0.0);
    let padding_bottom = px(&style
        .get_property_value("padding-bottom")
        .unwrap_or_default())
    .unwrap_or(0.0);
    let top = f64::from(marker.offset_top()) - padding_top;
    let content_height = f64::from(mirror.offset_height()) - padding_top - padding_bottom;
    let rect = textarea.get_bounding_client_rect();
    let line_top = rect.top() + f64::from(marker.offset_top());
    let metrics = CaretMetrics {
        first_line: top < line_height * 0.5,
        last_line: top + line_height * 1.5 > content_height,
        x: rect.left() + f64::from(marker.offset_left()),
        line_top,
        line_bottom: line_top + line_height,
    };
    mirror.remove();
    Some(metrics)
}

fn title_input() -> Option<web_sys::HtmlInputElement> {
    web_sys::window()?
        .document()?
        .query_selector(".document-title-input")
        .ok()
        .flatten()?
        .dyn_into()
        .ok()
}

fn focus_title(landing: Landing) -> bool {
    let Some(title) = title_input() else {
        return false;
    };
    if title.disabled() {
        return false;
    }
    crate::page_driver::blur_page_source();
    let _ = title.focus();
    let caret = match landing {
        Landing::Start => 0,
        _ => page_text::utf16_len(&title.value()),
    };
    let _ = title.set_selection_range(caret, caret);
    true
}

impl Driver {
    /// A note just created opens with the caret in its empty title.
    pub(crate) fn focus_new_note_title(&self) {
        if title_input().is_some_and(|title| title.value().is_empty()) {
            focus_title(Landing::Start);
        }
    }

    /// Enter or ↓ in the title moves to the start of the first line,
    /// adding one when the page does not start with text.
    pub(crate) fn title_keydown(&self, event: &web_sys::KeyboardEvent) -> bool {
        if event.is_composing() || event.alt_key() || event.ctrl_key() || event.meta_key() {
            return false;
        }
        let Some(title) = title_input() else {
            return false;
        };
        let at_end =
            title.selection_start().ok().flatten() == Some(page_text::utf16_len(&title.value()));
        match event.key().as_str() {
            "Enter" if !event.shift_key() => {}
            "ArrowDown" if at_end || !title.value().contains('\n') => {}
            _ => return false,
        }
        // Rendering restores focus to whatever had it, so let go first.
        let _ = title.blur();
        let first = {
            let inner = self.0.borrow();
            Driver::effective_document(&inner).blocks.first().cloned()
        };
        match first {
            Some(block) if matches!(block.content, BlockContent::Markdown { .. }) => {
                self.reopen_at(block.id, 0);
            }
            _ => {
                let id = BlockId::new_v4();
                self.insert_block_op(
                    0,
                    id,
                    BlockContent::Markdown {
                        text: String::new(),
                    },
                );
                self.reopen_at(id, 0);
            }
        }
        true
    }

    pub(crate) fn insert_block_op(&self, idx: usize, id: BlockId, content: BlockContent) {
        let now = js_sys::Date::now() as i64;
        self.dispatch(ClientEvent::LocalIntent {
            op: DocumentOp::InsertBlock {
                idx,
                block: DocumentBlock {
                    id,
                    created_at_ms: now,
                    updated_at_ms: now,
                    content,
                },
            },
            update_id: UpdateId::new_v4(),
            applied_at_ms: now,
        });
    }

    /// A position in a Markdown block that follows later edits.
    fn text_position(&self, block_id: BlockId, offset: u32) -> Option<RelativePosition> {
        let target = TextTarget::Markdown(block_id);
        self.ensure_text_target(target);
        self.0
            .borrow_mut()
            .text_client_state
            .as_mut()?
            .relative_position(target, offset, TextAssociation::After)
            .ok()
    }

    /// Ink pinned inside Markdown block `block_id`, with the UTF-16 offset it
    /// is pinned at (block-pinned ink counts as offset 0).
    fn anchored_ink(
        &self,
        doc: &InkstoneDocument,
        block_id: BlockId,
    ) -> Vec<(InkGroupId, InkAnchor, u32)> {
        let Some(layer) = doc.ink.as_ref() else {
            return Vec::new();
        };
        let inner = self.0.borrow();
        layer
            .anchors
            .iter()
            .filter_map(|(group, anchor)| match &anchor.target {
                AnchorTarget::Text { position }
                    if position.target == TextTarget::Markdown(block_id) =>
                {
                    let offset = inner
                        .text_client_state
                        .as_ref()?
                        .resolve_position_optimistic(position)
                        .ok()?;
                    Some((*group, anchor.clone(), offset))
                }
                AnchorTarget::Block { block_id: id } if *id == block_id => {
                    Some((*group, anchor.clone(), 0))
                }
                _ => None,
            })
            .collect()
    }

    /// Re-pins `group` at `offset` of Markdown block `block_id`, shifting its
    /// offset by `dy` so the ink stays where it is on screen.
    fn repin(
        &self,
        group: InkGroupId,
        anchor: &InkAnchor,
        block_id: BlockId,
        offset: u32,
        dy: f32,
    ) {
        let Some(position) = self.text_position(block_id, offset) else {
            return;
        };
        self.dispatch_page_op(DocumentOp::SetInkAnchor {
            group,
            anchor: InkAnchor {
                target: AnchorTarget::Text { position },
                dx: anchor.dx,
                dy: anchor.dy + dy,
            },
        });
    }

    /// Replaces Markdown block `block_id` by the blocks of `split`. Returns
    /// the ids of the resulting blocks, in order.
    fn apply_block_split(&self, block_id: BlockId, split: &Split) -> Option<Vec<BlockId>> {
        let doc = {
            let inner = self.0.borrow();
            Driver::effective_document(&inner)
        };
        let index = doc.blocks.iter().position(|block| block.id == block_id)?;
        let BlockContent::Markdown { text: original } = &doc.blocks[index].content else {
            return None;
        };
        let anchored = self.anchored_ink(&doc, block_id);

        // New blocks go first and the original is trimmed last, so a reload
        // between the two saves duplicates text rather than losing it.
        let mut ids = Vec::with_capacity(split.pieces.len());
        for (offset, piece) in split.pieces.iter().enumerate() {
            if offset == split.keep {
                ids.push(block_id);
                continue;
            }
            let id = BlockId::new_v4();
            self.insert_block_op(
                index + offset,
                id,
                BlockContent::Markdown {
                    text: piece.text.clone(),
                },
            );
            ids.push(id);
        }
        self.dispatch_text_edit(
            TextTarget::Markdown(block_id),
            original,
            &split.pieces[split.keep].text,
        );
        for (group, anchor, offset) in anchored {
            let (piece, local) = split.locate(offset);
            if piece != split.keep {
                self.repin(group, &anchor, ids[piece], local, 0.0);
            }
        }
        Some(ids)
    }

    /// Splits a block left holding several elements (a paste, a blank line
    /// typed in the middle) into one block per element.
    pub(crate) fn normalize_block(&self, block_id: BlockId) {
        let text = {
            let inner = self.0.borrow();
            let doc = Driver::effective_document(&inner);
            match doc.get_block(block_id).map(|block| &block.content) {
                Some(BlockContent::Markdown { text }) => text.clone(),
                _ => return,
            }
        };
        if let Some(split) = page_text::split_elements(&text) {
            self.apply_block_split(block_id, &split);
        }
    }

    /// Opens `block_id` for editing with the caret at `caret`, first giving
    /// each element of a multi-element block its own block so only the
    /// clicked element turns into source.
    pub(crate) fn edit_block_at(&self, block_id: BlockId, caret: u32) {
        let text = {
            let inner = self.0.borrow();
            let doc = Driver::effective_document(&inner);
            match doc.get_block(block_id).map(|block| &block.content) {
                Some(BlockContent::Markdown { text }) => Some(text.clone()),
                _ => None,
            }
        };
        if let Some(split) = text.as_deref().and_then(page_text::split_elements) {
            if let Some(ids) = self.apply_block_split(block_id, &split) {
                let (piece, local) = split.locate(caret);
                self.start_editing(ids[piece], local);
                return;
            }
        }
        self.start_editing(block_id, caret);
    }

    /// Commits whatever the textarea holds that the document has not seen.
    pub(crate) fn commit_source(
        &self,
        textarea: &web_sys::HtmlTextAreaElement,
        target: TextTarget,
    ) {
        let value = textarea.value();
        let canonical = textarea
            .get_attribute("data-canonical-value")
            .unwrap_or_default();
        if value != canonical {
            textarea.set_attribute("data-canonical-value", &value).ok();
            self.dispatch_text_edit(target, &canonical, &value);
        }
    }

    pub(crate) fn replace_source(
        &self,
        textarea: &web_sys::HtmlTextAreaElement,
        target: TextTarget,
        text: &str,
        caret: u32,
    ) {
        let before = textarea
            .get_attribute("data-canonical-value")
            .unwrap_or_default();
        textarea.set_value(text);
        textarea.set_attribute("data-canonical-value", text).ok();
        self.dispatch_text_edit(target, &before, text);
        let _ = textarea.set_selection_range(caret, caret);
        crate::page_driver::autosize(textarea.as_ref());
        self.layout_page();
    }

    /// Moves editing to `block_id` with the caret at `caret` after a
    /// structural change, rebuilding the page.
    pub(crate) fn reopen_at(&self, block_id: BlockId, caret: u32) {
        {
            let mut inner = self.0.borrow_mut();
            inner.editing_block = Some(block_id);
            inner.pending_block_caret = Some((block_id, caret));
        }
        crate::page_driver::blur_page_source();
        self.render();
    }

    /// Backspace at the start of a block: joins it onto the block above.
    fn join_backward(&self, block_id: BlockId) -> bool {
        let doc = {
            let inner = self.0.borrow();
            Driver::effective_document(&inner)
        };
        let Some(index) = doc.blocks.iter().position(|block| block.id == block_id) else {
            return false;
        };
        let current = &doc.blocks[index];
        let Some(current_text) = editable_text(&current.content) else {
            return false;
        };
        let Some(previous) = index.checked_sub(1).map(|i| &doc.blocks[i]) else {
            return false;
        };
        let current_is_markdown = matches!(current.content, BlockContent::Markdown { .. });
        match &previous.content {
            BlockContent::Markdown {
                text: previous_text,
            } if current_is_markdown => {
                let (joined, caret) = page_text::join(previous_text, current_text);
                let anchored = self.anchored_ink(&doc, block_id);
                let shift = match (block_top(block_id), block_top(previous.id)) {
                    (Some(current_top), Some(previous_top)) => (current_top - previous_top) as f32,
                    _ => 0.0,
                };
                self.dispatch_text_edit(TextTarget::Markdown(previous.id), previous_text, &joined);
                for (group, anchor, offset) in anchored {
                    self.repin(group, &anchor, previous.id, caret + offset, shift);
                }
                self.dispatch_page_op(DocumentOp::DeleteBlock { block_id });
                self.reopen_at(previous.id, caret);
                true
            }
            BlockContent::Divider => {
                self.dispatch_page_op(DocumentOp::DeleteBlock {
                    block_id: previous.id,
                });
                self.reopen_at(block_id, 0);
                true
            }
            BlockContent::Space { .. } if !self.block_has_ink(&doc, previous.id) => {
                self.dispatch_page_op(DocumentOp::DeleteBlock {
                    block_id: previous.id,
                });
                self.reopen_at(block_id, 0);
                true
            }
            content if current_text.is_empty() => {
                // An empty block disappears into whatever is above it.
                self.dispatch_page_op(DocumentOp::DeleteBlock { block_id });
                match editable_text(content) {
                    Some(text) => {
                        self.reopen_at(previous.id, page_text::utf16_len(text));
                    }
                    None => {
                        self.0.borrow_mut().editing_block = None;
                        crate::page_driver::blur_page_source();
                        self.render();
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// Delete at the end of a block: pulls the block below up into it.
    fn join_forward(&self, block_id: BlockId) -> bool {
        let doc = {
            let inner = self.0.borrow();
            Driver::effective_document(&inner)
        };
        let Some(index) = doc.blocks.iter().position(|block| block.id == block_id) else {
            return false;
        };
        let BlockContent::Markdown { text } = &doc.blocks[index].content else {
            return false;
        };
        let Some(next) = doc.blocks.get(index + 1) else {
            return false;
        };
        match &next.content {
            BlockContent::Markdown { text: next_text } => {
                let (joined, caret) = page_text::join(text, next_text);
                let anchored = self.anchored_ink(&doc, next.id);
                let shift = match (block_top(next.id), block_top(block_id)) {
                    (Some(next_top), Some(top)) => (next_top - top) as f32,
                    _ => 0.0,
                };
                self.dispatch_text_edit(TextTarget::Markdown(block_id), text, &joined);
                for (group, anchor, offset) in anchored {
                    self.repin(group, &anchor, block_id, caret + offset, shift);
                }
                self.dispatch_page_op(DocumentOp::DeleteBlock { block_id: next.id });
                self.reopen_at(block_id, caret);
                true
            }
            BlockContent::Divider => {
                self.dispatch_page_op(DocumentOp::DeleteBlock { block_id: next.id });
                self.reopen_at(block_id, page_text::utf16_len(text));
                true
            }
            BlockContent::Space { .. } if !self.block_has_ink(&doc, next.id) => {
                self.dispatch_page_op(DocumentOp::DeleteBlock { block_id: next.id });
                self.reopen_at(block_id, page_text::utf16_len(text));
                true
            }
            _ => false,
        }
    }

    fn block_has_ink(&self, doc: &InkstoneDocument, block_id: BlockId) -> bool {
        doc.ink.as_ref().is_some_and(|layer| {
            layer
                .anchors
                .values()
                .any(|anchor| anchor.target.block_id() == Some(block_id))
        })
    }

    /// Moves the caret into the nearest text or math block above (`step`
    /// −1) or below (+1).
    fn walk(&self, block_id: BlockId, step: isize, landing: Landing) -> bool {
        let doc = {
            let inner = self.0.borrow();
            Driver::effective_document(&inner)
        };
        let Some(mut index) = doc.blocks.iter().position(|block| block.id == block_id) else {
            return false;
        };
        let target = loop {
            let Some(next) = index
                .checked_add_signed(step)
                .filter(|i| *i < doc.blocks.len())
            else {
                // Up from the first line reaches the title.
                return step < 0 && focus_title(Landing::End);
            };
            index = next;
            if editable_text(&doc.blocks[index].content).is_some() {
                break &doc.blocks[index];
            }
        };
        let text = editable_text(&target.content).unwrap_or_default();
        let caret = match landing {
            Landing::Start => 0,
            Landing::End => page_text::utf16_len(text),
            Landing::Point(x, y) => web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| {
                    document
                        .query_selector(&block_selector(target.id))
                        .ok()
                        .flatten()
                })
                .filter(|_| matches!(target.content, BlockContent::Markdown { .. }))
                .and_then(|element| {
                    let rect = element.get_bounding_client_rect();
                    let y = y.clamp(rect.top() + 2.0, rect.bottom() - 2.0);
                    source_caret_at_point(&element, text, x, y)
                })
                .unwrap_or(if step < 0 {
                    page_text::utf16_len(text)
                } else {
                    0
                }),
        };
        self.edit_block_at(target.id, caret);
        true
    }

    /// Keys typed into a block's source. Returns whether the key was handled.
    pub(crate) fn source_keydown(
        &self,
        event: &web_sys::KeyboardEvent,
        textarea: &web_sys::HtmlTextAreaElement,
    ) -> bool {
        if event.alt_key()
            && event.shift_key()
            && !event.ctrl_key()
            && !event.meta_key()
            && matches!(event.key().as_str(), "ArrowUp" | "ArrowDown")
        {
            if let Some(block_id) = textarea
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
            {
                let kind = textarea.get_attribute("data-block-kind");
                let target = if kind.as_deref() == Some("math") {
                    TextTarget::Math(block_id)
                } else {
                    TextTarget::Markdown(block_id)
                };
                self.commit_source(textarea, target);
                return self.move_editing_block(textarea, block_id, event.key() == "ArrowUp");
            }
        }
        if event.is_composing()
            || event.alt_key()
            || event.ctrl_key()
            || event.meta_key()
            || textarea.get_attribute("data-composing").as_deref() == Some("true")
        {
            return false;
        }
        let Some(block_id) = textarea
            .get_attribute("data-block-id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        else {
            return false;
        };
        let is_math = textarea.get_attribute("data-block-kind").as_deref() == Some("math");
        let target = if is_math {
            TextTarget::Math(block_id)
        } else {
            TextTarget::Markdown(block_id)
        };
        if self.slash_keydown(event, textarea) {
            return true;
        }
        let value = textarea.value();
        let start = textarea.selection_start().ok().flatten().unwrap_or(0);
        let end = textarea.selection_end().ok().flatten().unwrap_or(0);
        let collapsed = start == end;
        let length = page_text::utf16_len(&value);
        let shift = event.shift_key();

        match event.key().as_str() {
            "Enter" if !shift && collapsed && !is_math => {
                self.commit_source(textarea, target);
                self.history_boundary();
                match page_text::enter_action(&value, start) {
                    EnterAction::Default => false,
                    EnterAction::Replace { text, caret } => {
                        self.commit_source(textarea, target);
                        self.replace_source(textarea, target, &text, caret);
                        true
                    }
                    EnterAction::Split(split) => {
                        self.commit_source(textarea, target);
                        match self.apply_block_split(block_id, &split) {
                            Some(ids) => {
                                self.reopen_at(ids[split.caret_piece], split.caret);
                                true
                            }
                            None => false,
                        }
                    }
                }
            }
            "Backspace" if collapsed && start == 0 => {
                self.commit_source(textarea, target);
                self.history_boundary();
                self.commit_source(textarea, target);
                self.join_backward(block_id)
            }
            "Delete" if collapsed && start == length && !is_math => {
                self.commit_source(textarea, target);
                self.history_boundary();
                self.commit_source(textarea, target);
                self.join_forward(block_id)
            }
            "ArrowUp" | "ArrowDown" if collapsed && !shift => {
                let up = event.key() == "ArrowUp";
                let Some(metrics) = caret_metrics(textarea, start) else {
                    return false;
                };
                if (up && !metrics.first_line) || (!up && !metrics.last_line) {
                    return false;
                }
                self.commit_source(textarea, target);
                let rect = textarea.get_bounding_client_rect();
                let landing = if up {
                    Landing::Point(metrics.x, rect.top() - 6.0)
                } else {
                    Landing::Point(metrics.x, rect.bottom() + 6.0)
                };
                self.walk(block_id, if up { -1 } else { 1 }, landing)
            }
            "ArrowLeft" if collapsed && !shift && start == 0 => {
                self.commit_source(textarea, target);
                self.walk(block_id, -1, Landing::End)
            }
            "ArrowRight" if collapsed && !shift && start == length => {
                self.commit_source(textarea, target);
                self.walk(block_id, 1, Landing::Start)
            }
            _ => false,
        }
    }
}
