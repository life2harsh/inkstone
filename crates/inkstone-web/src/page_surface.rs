//! The page ink surface: one transparent Stonepen canvas over the page column.
//!
//! The canvas lives outside `#root` so full re-renders never remount it. It is
//! `pointer-events: none`; the driver forwards pen (and, in ink mode, mouse)
//! input to it. Strokes are grouped; each group is a Stonepen host frame whose
//! position is derived from its anchor in the text flow on every layout pass.

use std::collections::HashMap;

use inkstone_client::page_ink::{self, PlacedGroup, RecentGroup, Rect, SourceSpan};
use inkstone_client::text_sync::TextClientState;
use inkstone_core::document::InkstoneDocument;
use inkstone_core::{
    AnchorTarget, BlockContent, BlockId, DocId, INK_UPDATE_PAYLOAD_VERSION, InkAnchor, InkGroupId,
    PageInkLayer, PageInkUpdate, TextAssociation, TextTarget,
};
use stonepen_wasm::{StonepenHandle, mount_stonepen};
use uuid::Uuid;
use wasm_bindgen::{JsCast, JsValue};

pub const PAGE_CANVAS_ID: &str = "page-ink-canvas";
const REFERENCE_PAGE_WIDTH: f32 = 720.0;
/// Presence previews for the page layer use the nil block id.
pub const PAGE_PRESENCE_BLOCK: Uuid = Uuid::nil();
/// Room below the lowest ink on the page.
const TAIL_PADDING: f64 = 120.0;
const MIN_TAIL: f64 = 160.0;

/// What a finished gesture produced.
pub struct PageCommit {
    pub update: Option<PageInkUpdate>,
    pub anchor: Option<(InkGroupId, InkAnchor)>,
}

pub struct PageSurface {
    doc_id: DocId,
    stream_epoch: Uuid,
    /// Snapshot that creates the layer. Sent with every update until the
    /// document is seen to have the layer, so a dropped first update cannot
    /// strand later ones.
    bootstrap: Option<Vec<u8>>,
    handle: StonepenHandle,
    canvas: web_sys::HtmlCanvasElement,
    synced: Option<inkstone_core::InkSnapshot>,
    read_only: bool,
    frames: HashMap<InkGroupId, (f32, f32, f32)>,
    page_scale: f32,
    bounds: HashMap<InkGroupId, Rect>,
    /// Last resolved anchor origin relative to its block, used while the
    /// block is being edited as source and its rendered elements are gone.
    block_offsets: HashMap<InkGroupId, (BlockId, f32, f32)>,
    /// While a block is edited as source: where each anchored group's text
    /// line sat in the source when editing began, so the ink moves by
    /// exactly as much as its line has since.
    source_lines: HashMap<InkGroupId, (BlockId, f32)>,
    recent: Option<RecentGroup>,
    active_group: Option<InkGroupId>,
    gesture: Option<i32>,
    pending_anchor: Option<(InkGroupId, InkAnchor)>,
    canvas_box: (f64, f64, f64, f64),
    brush_width: f32,
}

fn dom_document() -> Option<web_sys::Document> {
    web_sys::window().and_then(|window| window.document())
}

fn ensure_canvas() -> Result<web_sys::HtmlCanvasElement, JsValue> {
    let document = dom_document().ok_or_else(|| JsValue::from_str("no document"))?;
    if let Some(existing) = document.get_element_by_id(PAGE_CANVAS_ID) {
        return existing
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|_| JsValue::from_str("page ink canvas is not a canvas"));
    }
    let canvas = document
        .create_element("canvas")?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str("failed to create canvas"))?;
    canvas.set_id(PAGE_CANVAS_ID);
    canvas.set_class_name("page-ink-canvas");
    canvas.set_attribute("aria-hidden", "true")?;
    document
        .body()
        .ok_or_else(|| JsValue::from_str("no body"))?
        .append_child(&canvas)?;
    Ok(canvas)
}

fn block_selector(block_id: BlockId) -> String {
    format!("[data-page-block-id=\"{}\"]", block_id)
}

fn top_left(element: &web_sys::Element, origin: (f64, f64)) -> (f32, f32) {
    let rect = element.get_bounding_client_rect();
    (
        (rect.left() - origin.0) as f32,
        (rect.top() - origin.1) as f32,
    )
}

fn sourcepos_elements(block: &web_sys::Element) -> Vec<(web_sys::Element, SourceSpan)> {
    let Ok(list) = block.query_selector_all("[data-sourcepos]") else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
        .filter_map(|element| {
            let span = element
                .get_attribute("data-sourcepos")
                .and_then(|value| page_ink::parse_sourcepos(&value))?;
            Some((element, span))
        })
        .collect()
}

fn markdown_text(doc: &InkstoneDocument, block_id: BlockId) -> Option<&str> {
    match doc.get_block(block_id).map(|block| &block.content) {
        Some(BlockContent::Markdown { text }) => Some(text),
        _ => None,
    }
}

impl PageSurface {
    pub fn mount(
        doc_id: DocId,
        layer: Option<&PageInkLayer>,
        actor: [u8; 16],
        read_only: bool,
    ) -> Result<Self, JsValue> {
        let canvas = ensure_canvas()?;
        let mut handle = mount_stonepen(PAGE_CANVAS_ID)?;
        handle.set_collaboration_actor(&actor)?;
        handle.set_surface_mode(true);
        handle.set_finger_drawing(true);
        let (stream_epoch, bootstrap, synced) = match layer {
            Some(layer) => {
                handle.load_collaboration_snapshot(
                    layer
                        .collaboration
                        .snapshot
                        .bytes()
                        .map_err(|error| JsValue::from_str(&error))?,
                )?;
                (
                    layer.collaboration.stream_epoch,
                    None,
                    Some(layer.collaboration.snapshot.clone()),
                )
            }
            None => {
                let fresh = inkstone_core::first_page_ink_layer(doc_id)
                    .map_err(|error| JsValue::from_str(&error))?;
                let bytes = fresh
                    .collaboration
                    .snapshot
                    .bytes()
                    .map_err(|error| JsValue::from_str(&error))?
                    .to_vec();
                handle.load_collaboration_snapshot(&bytes)?;
                (fresh.collaboration.stream_epoch, Some(bytes), None)
            }
        };
        handle.set_read_only(read_only);
        Ok(Self {
            doc_id,
            stream_epoch,
            bootstrap,
            handle,
            canvas,
            synced,
            read_only,
            frames: HashMap::new(),
            page_scale: 1.0,
            bounds: HashMap::new(),
            block_offsets: HashMap::new(),
            source_lines: HashMap::new(),
            recent: None,
            active_group: None,
            gesture: None,
            pending_anchor: None,
            canvas_box: (0.0, 0.0, 0.0, 0.0),
            brush_width: 0.0,
        })
    }

    pub fn doc_id(&self) -> DocId {
        self.doc_id
    }

    pub fn stream_epoch(&self) -> Uuid {
        self.stream_epoch
    }

    pub fn read_only(&self) -> bool {
        self.read_only
    }

    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    /// Brings the runtime up to date with the document's layer. Returns
    /// `false` when the runtime can no longer follow the layer (another
    /// epoch, or the layer went away after we had seen it) and must be
    /// remounted.
    pub fn sync(&mut self, layer: Option<&PageInkLayer>) -> Result<bool, JsValue> {
        let Some(layer) = layer else {
            return Ok(self.bootstrap.is_some());
        };
        if layer.collaboration.stream_epoch != self.stream_epoch {
            return Ok(false);
        }
        self.bootstrap = None;
        if self
            .synced
            .as_ref()
            .is_some_and(|synced| synced.same(&layer.collaboration.snapshot))
        {
            return Ok(true);
        }
        let remote = layer
            .collaboration
            .snapshot
            .doc()
            .map_err(|error| JsValue::from_str(&error))?;
        let state_vector = self.handle.collaboration_state_vector()?;
        let missing = remote
            .update_since(&state_vector)
            .map_err(|error| JsValue::from_str(&error.to_string()))?;
        if !missing.is_empty() {
            self.handle.apply_collaboration_update(&missing)?;
        }
        self.synced = Some(layer.collaboration.snapshot.clone());
        Ok(true)
    }

    pub fn configure(&mut self, tool: &str, width: f32, color: (u8, u8, u8)) {
        if self.read_only {
            return;
        }
        self.handle.set_tool(tool);
        self.brush_width = width;
        self.handle.set_brush_width(width);
        self.handle.set_brush_color(color.0, color.1, color.2);
    }

    pub fn destroy(mut self) -> Result<Option<PageInkUpdate>, JsValue> {
        let update = self.take_update()?;
        self.handle.destroy();
        let _ = self.canvas.style().set_property("display", "none");
        Ok(update)
    }

    pub fn take_update(&mut self) -> Result<Option<PageInkUpdate>, JsValue> {
        let update = self.handle.take_local_update()?;
        if update.is_empty() {
            return Ok(None);
        }
        Ok(Some(PageInkUpdate {
            version: INK_UPDATE_PAYLOAD_VERSION,
            stream_epoch: self.stream_epoch,
            update,
            bootstrap_snapshot: self.bootstrap.clone(),
        }))
    }

    pub fn take_preview(&mut self) -> Option<Vec<u8>> {
        self.handle
            .take_local_preview()
            .ok()
            .filter(|preview| !preview.is_empty())
    }

    pub fn apply_remote_preview(&mut self, participant: Uuid, preview: &[u8], now_ms: f64) {
        let _ = self.handle.apply_remote_preview(
            participant.as_bytes(),
            self.stream_epoch.as_bytes(),
            preview,
            now_ms,
        );
    }

    pub fn remove_remote_participant(&mut self, participant: Uuid) {
        self.handle
            .remove_remote_preview(participant.as_bytes(), self.stream_epoch.as_bytes());
    }

    pub fn expire_remote_previews(&mut self, now_ms: f64, ttl_ms: f64) {
        self.handle.expire_remote_previews(now_ms, ttl_ms);
    }

    pub fn undo(&mut self) -> Result<Option<PageInkUpdate>, JsValue> {
        self.handle.undo();
        self.take_update()
    }

    pub fn redo(&mut self) -> Result<Option<PageInkUpdate>, JsValue> {
        self.handle.redo();
        self.take_update()
    }

    pub fn has_selection(&self) -> bool {
        self.handle.has_selection()
    }

    pub fn delete_selection(&mut self) -> Result<Option<PageInkUpdate>, JsValue> {
        self.handle.run_command("delete_selection");
        self.take_update()
    }

    pub fn clear_selection(&mut self) {
        self.handle.run_command("clear_selection");
    }

    /// Anchored groups that no longer have any strokes.
    pub fn orphaned_groups(&mut self, layer: Option<&PageInkLayer>) -> Vec<InkGroupId> {
        self.refresh_bounds();
        layer
            .map(|layer| {
                layer
                    .anchors
                    .keys()
                    .filter(|group| !self.bounds.contains_key(group))
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn refresh_bounds(&mut self) {
        self.bounds = self
            .handle
            .frame_bounds()
            .into_iter()
            .map(|(id, min_x, min_y, max_x, max_y)| {
                (
                    InkGroupId(Uuid::from_bytes(id)),
                    Rect::new(min_x, min_y, max_x, max_y),
                )
            })
            .collect();
    }

    /// The page's ink as one SVG in page coordinates, with the box it covers
    /// relative to the page's top-left corner.
    pub fn export_svg(&self, page_width: f32, page_height: f32) -> (String, f32, f32, f32, f32) {
        let (mut min_x, mut min_y, mut max_x, mut max_y) =
            (0.0_f32, 0.0_f32, page_width, page_height);
        for (group, bounds) in &self.bounds {
            if let Some((x, y, scale)) = self.frames.get(group) {
                min_x = min_x.min(x + bounds.min_x * scale - 8.0);
                min_y = min_y.min(y + bounds.min_y * scale - 8.0);
                max_x = max_x.max(x + bounds.max_x * scale + 8.0);
                max_y = max_y.max(y + bounds.max_y * scale + 8.0);
            }
        }
        let (width, height) = (max_x - min_x, max_y - min_y);
        (
            self.handle.export_view_svg(min_x, min_y, width, height),
            min_x,
            min_y,
            width,
            height,
        )
    }

    fn push_frames(&mut self) {
        let frames = self
            .frames
            .iter()
            .map(|(group, (x, y, scale))| (group.0.into_bytes(), *x, *y, *scale))
            .collect::<Vec<_>>();
        self.handle.set_frames(&frames);
    }

    pub fn hide(&self) {
        let _ = self.canvas.style().set_property("display", "none");
    }

    /// Whether a client point is over the visible page canvas.
    pub fn contains_client_point(&self, x: f64, y: f64) -> bool {
        let (left, top, width, height) = self.canvas_box;
        width > 0.0 && x >= left && x < left + width && y >= top && y < top + height
    }

    /// Re-fits the canvas to the scroll viewport and re-points the camera at
    /// the page column. Cheap; runs on every scroll.
    pub fn update_view(&mut self) {
        let Some(document) = dom_document() else {
            return;
        };
        let (Some(surface), Some(scroller)) = (
            document.query_selector(".page-surface").ok().flatten(),
            document.query_selector(".main-content").ok().flatten(),
        ) else {
            self.hide();
            self.canvas_box = (0.0, 0.0, 0.0, 0.0);
            return;
        };
        let viewport = scroller.get_bounding_client_rect();
        let width = f64::from(scroller.client_width());
        let height = f64::from(scroller.client_height());
        let next_box = (viewport.left(), viewport.top(), width, height);
        let style = self.canvas.style();
        let _ = style.set_property("display", "block");
        if next_box != self.canvas_box {
            let _ = style.set_property("left", &format!("{}px", next_box.0));
            let _ = style.set_property("top", &format!("{}px", next_box.1));
            let _ = style.set_property("width", &format!("{}px", next_box.2));
            let _ = style.set_property("height", &format!("{}px", next_box.3));
            let resized = (next_box.2, next_box.3) != (self.canvas_box.2, self.canvas_box.3);
            self.canvas_box = next_box;
            if resized {
                self.handle.resize();
            }
        }
        let page = surface.get_bounding_client_rect();
        self.handle.set_view(
            (page.left() - next_box.0) as f32,
            (page.top() - next_box.1) as f32,
            1.0,
        );
    }

    pub fn layout(
        &mut self,
        doc: &InkstoneDocument,
        text: Option<&TextClientState>,
        editing: Option<BlockId>,
    ) {
        let Some(document) = dom_document() else {
            return;
        };
        let Some(surface) = document.query_selector(".page-surface").ok().flatten() else {
            self.hide();
            self.canvas_box = (0.0, 0.0, 0.0, 0.0);
            return;
        };
        self.refresh_bounds();
        let page = surface.get_bounding_client_rect();
        let page_width = page.width() as f32;
        let empty = Default::default();
        let anchors = doc
            .ink
            .as_ref()
            .map(|layer| &layer.anchors)
            .unwrap_or(&empty);

        let zoom = crate::zoom::get() as f32;
        if self.gesture.is_none() && page_width > 0.0 {
            self.page_scale = (page_width / zoom / REFERENCE_PAGE_WIDTH).clamp(0.2, 1.0) * zoom;
        }

        for block in &doc.blocks {
            let BlockContent::Space { min_height } = block.content else {
                continue;
            };
            let scale = self.page_scale / zoom;
            let bottoms = anchors
                .iter()
                .filter(|(_, anchor)| anchor.target.block_id() == Some(block.id))
                .filter(|(_, anchor)| matches!(anchor.target, AnchorTarget::Block { .. }))
                .filter_map(|(group, anchor)| {
                    self.bounds
                        .get(group)
                        .map(|bounds| (anchor.dy + bounds.max_y) * scale)
                });
            let height = page_ink::space_height(min_height, bottoms);
            if let Ok(Some(element)) = surface.query_selector(&block_selector(block.id)) {
                if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
                    let _ = element
                        .style()
                        .set_property("height", &format!("{}px", height.round()));
                }
            }
        }

        let origin = (page.left(), page.top());
        let visible = document
            .query_selector(".main-content")
            .ok()
            .flatten()
            .map(|scroller| {
                let left = scroller.get_bounding_client_rect().left() - page.left();
                (
                    left as f32,
                    (left + f64::from(scroller.client_width())) as f32,
                )
            })
            .filter(|(left, right)| right > left)
            .map_or((0.0, page_width), |(left, right)| {
                (left.min(0.0), right.max(page_width))
            });
        let mut frames = HashMap::new();
        for (group, anchor) in anchors {
            let is_text = matches!(anchor.target, AnchorTarget::Text { .. });
            let bounds = self.bounds.get(group).copied();
            let scale = if is_text {
                let room = page_width / zoom;
                zoom * bounds
                    .map(|bounds| bounds.max_x - bounds.min_x)
                    .filter(|width| *width > room && room > 0.0)
                    .map_or(1.0, |width| (room / width).max(self.page_scale / zoom))
            } else {
                self.page_scale
            };
            let resolved =
                self.resolve_origin(&surface, origin, doc, text, editing, Some(*group), anchor);
            let Some(base) = resolved else {
                if let Some(previous) = self.frames.get(group) {
                    frames.insert(*group, *previous);
                }
                continue;
            };
            let mut x = base.0 + anchor.dx * scale;
            let y = base.1 + anchor.dy * scale;
            if let (true, Some(bounds)) = (is_text && page_width > 0.0, bounds) {
                if x + bounds.max_x * scale > visible.1 {
                    x -= x + bounds.max_x * scale - page_width;
                }
                if x + bounds.min_x * scale < visible.0 {
                    x = -bounds.min_x * scale;
                }
            }
            frames.insert(*group, (x, y, scale));
        }
        for group in self.bounds.keys() {
            if !frames.contains_key(group) {
                let previous = self.frames.get(group).copied().unwrap_or((0.0, 0.0, 1.0));
                frames.insert(*group, previous);
            }
        }
        if let Some((group, _)) = &self.pending_anchor {
            if let Some(frame) = self.frames.get(group) {
                frames.insert(*group, *frame);
            }
        }
        if let Some(group) = self.active_group.filter(|_| self.gesture.is_some()) {
            if let Some(frame) = self.frames.get(&group) {
                frames.insert(group, *frame);
            }
        }
        self.frames = frames;
        self.push_frames();

        let ink_bottom = self
            .bounds
            .iter()
            .filter_map(|(group, bounds)| {
                self.frames
                    .get(group)
                    .map(|(_, y, scale)| f64::from(y + bounds.max_y * scale))
            })
            .fold(0.0_f64, f64::max);
        if let Ok(Some(tail)) = surface.query_selector(".page-tail") {
            let tail_top = tail.get_bounding_client_rect().top() - page.top();
            let height = (crate::zoom::local(ink_bottom - tail_top) + TAIL_PADDING).max(MIN_TAIL);
            if let Ok(tail) = tail.dyn_into::<web_sys::HtmlElement>() {
                let _ = tail
                    .style()
                    .set_property("height", &format!("{}px", height.round()));
            }
        }

        self.update_view();
    }

    pub fn comment_points(
        &mut self,
        doc: &InkstoneDocument,
        text: Option<&TextClientState>,
        editing: Option<BlockId>,
        pins: &[(Uuid, InkAnchor)],
    ) -> Option<((f64, f64), f64, Vec<(f32, f32)>)> {
        let document = dom_document()?;
        let surface = document.query_selector(".page-surface").ok()??;
        let scroller = document.query_selector(".main-content").ok()??;
        let page = surface.get_bounding_client_rect();
        let view = scroller.get_bounding_client_rect();
        let offset = (
            page.left() - view.left() + f64::from(scroller.scroll_left()),
            page.top() - view.top() + f64::from(scroller.scroll_top()),
        );
        let origin = (page.left(), page.top());
        let scale = self.page_scale;
        let points = pins
            .iter()
            .map(|(id, anchor)| {
                let group = Some(InkGroupId(*id));
                let base = self
                    .resolve_origin(&surface, origin, doc, text, editing, group, anchor)
                    .unwrap_or((0.0, 0.0));
                (base.0 + anchor.dx * scale, base.1 + anchor.dy * scale)
            })
            .collect();
        Some((offset, f64::from(scroller.client_width()), points))
    }

    pub fn comment_anchor_at(
        &mut self,
        client: (f64, f64),
        doc: &InkstoneDocument,
        mut text: Option<&mut TextClientState>,
        editing: Option<BlockId>,
    ) -> Option<InkAnchor> {
        let surface = dom_document()?.query_selector(".page-surface").ok()??;
        let page = surface.get_bounding_client_rect();
        let origin = (page.left(), page.top());
        let point = ((client.0 - origin.0) as f32, (client.1 - origin.1) as f32);
        let target =
            self.anchor_target_at(&surface, origin, point, doc, text.as_deref_mut(), editing);
        let probe = InkAnchor {
            target,
            dx: 0.0,
            dy: 0.0,
        };
        let base = self
            .resolve_origin(
                &surface,
                origin,
                doc,
                text.as_deref(),
                editing,
                None,
                &probe,
            )
            .unwrap_or((0.0, 0.0));
        let scale = self.page_scale.max(0.01);
        let on_text = matches!(probe.target, AnchorTarget::Text { .. });
        Some(InkAnchor {
            dx: (point.0 - base.0) / scale,
            dy: if on_text {
                0.0
            } else {
                (point.1 - base.1) / scale
            },
            ..probe
        })
    }

    /// Page-relative origin an anchor's offset is measured from.
    #[allow(clippy::too_many_arguments)]
    fn resolve_origin(
        &mut self,
        surface: &web_sys::Element,
        origin: (f64, f64),
        doc: &InkstoneDocument,
        text: Option<&TextClientState>,
        editing: Option<BlockId>,
        group: Option<InkGroupId>,
        anchor: &InkAnchor,
    ) -> Option<(f32, f32)> {
        let block_id = match &anchor.target {
            AnchorTarget::Page => return Some((0.0, 0.0)),
            AnchorTarget::Block { block_id } => {
                let element = surface.query_selector(&block_selector(*block_id)).ok()??;
                return Some(top_left(&element, origin));
            }
            AnchorTarget::Text { position } => match position.target {
                TextTarget::Markdown(block_id) => block_id,
                _ => return None,
            },
        };
        let block = surface.query_selector(&block_selector(block_id)).ok()??;
        let block_origin = top_left(&block, origin);
        let AnchorTarget::Text { position } = &anchor.target else {
            return Some(block_origin);
        };
        if editing == Some(block_id) {
            let cached = group
                .and_then(|group| self.block_offsets.get(&group))
                .filter(|(cached_block, _, _)| *cached_block == block_id)
                .map(|(_, dx, dy)| (block_origin.0 + dx, block_origin.1 + dy))
                .unwrap_or(block_origin);
            // The ink follows its line as text above it grows or shrinks.
            let line = block
                .query_selector(".page-source")
                .ok()
                .flatten()
                .and_then(|source| source.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
                .zip(text.and_then(|text| text.resolve_position_optimistic(position).ok()))
                .and_then(|(source, offset)| crate::page_editing::caret_metrics(&source, offset))
                .map(|metrics| (metrics.line_top - block.get_bounding_client_rect().top()) as f32);
            let shift = match (group, line) {
                (Some(group), Some(line)) => {
                    let start = self
                        .source_lines
                        .entry(group)
                        .and_modify(|entry| {
                            if entry.0 != block_id {
                                *entry = (block_id, line);
                            }
                        })
                        .or_insert((block_id, line))
                        .1;
                    line - start
                }
                _ => 0.0,
            };
            return Some((cached.0, cached.1 + shift));
        }
        if let Some(group) = group {
            self.source_lines.remove(&group);
        }
        let resolved = text
            .and_then(|text| text.resolve_position_optimistic(position).ok())
            .zip(markdown_text(doc, block_id))
            .and_then(|(offset, source)| text_origin(&block, source, offset, origin))
            .unwrap_or(block_origin);
        if let Some(group) = group {
            self.block_offsets.insert(
                group,
                (
                    block_id,
                    resolved.0 - block_origin.0,
                    resolved.1 - block_origin.1,
                ),
            );
        }
        Some(resolved)
    }

    fn anchor_target_at(
        &self,
        surface: &web_sys::Element,
        origin: (f64, f64),
        point: (f32, f32),
        doc: &InkstoneDocument,
        text: Option<&mut TextClientState>,
        editing: Option<BlockId>,
    ) -> AnchorTarget {
        let Ok(blocks) = surface.query_selector_all("[data-page-block-id]") else {
            return AnchorTarget::Page;
        };
        let mut chosen: Option<(web_sys::Element, BlockId)> = None;
        for index in 0..blocks.length() {
            let Some(element) = blocks
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            let Some(block_id) = block_id_of(&element) else {
                continue;
            };
            let rect = element.get_bounding_client_rect();
            let top = (rect.top() - origin.1) as f32;
            let bottom = (rect.bottom() - origin.1) as f32;
            if top <= point.1 {
                chosen = Some((element, block_id));
            }
            if point.1 < bottom {
                break;
            }
        }
        let Some((block, block_id)) = chosen else {
            return AnchorTarget::Page;
        };
        if editing == Some(block_id) || block.query_selector(".pdf-pages").ok().flatten().is_some()
        {
            return AnchorTarget::Block { block_id };
        }
        let (Some(text), Some(source)) = (text, markdown_text(doc, block_id)) else {
            return AnchorTarget::Block { block_id };
        };
        let client = (origin.0 + f64::from(point.0), origin.1 + f64::from(point.1));
        if let Some((target, _)) = word_anchor_at(surface, origin, client, doc, text, editing) {
            return target;
        }
        let line_hit = sourcepos_elements(&block)
            .into_iter()
            .filter(|(element, _)| {
                let rect = element.get_bounding_client_rect();
                let top = (rect.top() - origin.1) as f32;
                let bottom = (rect.bottom() - origin.1) as f32;
                top <= point.1 && point.1 < bottom
            })
            .min_by(|(a, _), (b, _)| {
                a.get_bounding_client_rect()
                    .height()
                    .total_cmp(&b.get_bounding_client_rect().height())
            });
        let Some((_, span)) = line_hit else {
            return AnchorTarget::Block { block_id };
        };
        let offset = page_ink::utf16_offset_of_line_col(source, span.start_line, span.start_col);
        match text.relative_position(
            TextTarget::Markdown(block_id),
            offset,
            TextAssociation::After,
        ) {
            Ok(position) => AnchorTarget::Text { position },
            Err(_) => AnchorTarget::Block { block_id },
        }
    }

    pub fn begin(
        &mut self,
        event: &web_sys::PointerEvent,
        doc: &InkstoneDocument,
        mut text: Option<&mut TextClientState>,
        editing: Option<BlockId>,
        drawing: bool,
    ) -> bool {
        if self.read_only || self.gesture.is_some() {
            return false;
        }
        let Some(surface) = dom_document()
            .and_then(|document| document.query_selector(".page-surface").ok().flatten())
        else {
            return false;
        };
        let page = surface.get_bounding_client_rect();
        let origin = (page.left(), page.top());
        let point = (
            (f64::from(event.client_x()) - origin.0) as f32,
            (f64::from(event.client_y()) - origin.1) as f32,
        );

        self.active_group = None;
        self.pending_anchor = None;
        if drawing {
            self.refresh_bounds();
            let placed = self
                .bounds
                .iter()
                .filter_map(|(group, bounds)| {
                    self.frames.get(group).map(|(x, y, scale)| PlacedGroup {
                        group: *group,
                        origin: (*x, *y),
                        bounds: Rect::new(
                            bounds.min_x * scale,
                            bounds.min_y * scale,
                            bounds.max_x * scale,
                            bounds.max_y * scale,
                        ),
                    })
                })
                .collect::<Vec<_>>();
            let now = js_sys::Date::now();
            let group = match page_ink::choose_group(&placed, point, now, self.recent) {
                Some(group) => group,
                None => {
                    let group = InkGroupId::new_v4();
                    let target = self.anchor_target_at(
                        &surface,
                        origin,
                        point,
                        doc,
                        text.as_deref_mut(),
                        editing,
                    );
                    let scale = if matches!(target, AnchorTarget::Text { .. }) {
                        crate::zoom::get() as f32
                    } else {
                        self.page_scale
                    };
                    let probe = InkAnchor {
                        target,
                        dx: 0.0,
                        dy: 0.0,
                    };
                    let base = self
                        .resolve_origin(
                            &surface,
                            origin,
                            doc,
                            text.as_deref(),
                            editing,
                            Some(group),
                            &probe,
                        )
                        .unwrap_or((0.0, 0.0));
                    self.frames.insert(group, (point.0, point.1, scale));
                    self.push_frames();
                    self.pending_anchor = Some((
                        group,
                        InkAnchor {
                            target: probe.target,
                            dx: (point.0 - base.0) / scale,
                            dy: (point.1 - base.1) / scale,
                        },
                    ));
                    group
                }
            };
            let scale = self.frames.get(&group).map_or(1.0, |frame| frame.2);
            if self.brush_width > 0.0 {
                self.handle.set_brush_width(self.brush_width / scale);
            }
            self.handle.set_active_frame(Some(group.0.into_bytes()));
            self.active_group = Some(group);
        } else {
            self.handle.set_active_frame(None);
        }

        let started = self.handle.forward_pointer_down(event);
        if started {
            self.gesture = Some(event.pointer_id());
        } else {
            if let Some((group, _)) = self.pending_anchor.take() {
                self.frames.remove(&group);
                self.push_frames();
            }
            self.active_group = None;
        }
        started
    }

    pub fn owns_pointer(&self, pointer_id: i32) -> bool {
        self.gesture == Some(pointer_id)
    }

    pub fn forward_move(&mut self, event: &web_sys::PointerEvent) {
        if self.owns_pointer(event.pointer_id()) {
            self.handle.forward_pointer_move(event);
        }
    }

    pub fn end(&mut self, event: &web_sys::PointerEvent, cancelled: bool) -> bool {
        if !self.owns_pointer(event.pointer_id()) {
            return false;
        }
        if cancelled {
            self.handle.forward_pointer_cancel(event);
        } else {
            self.handle.forward_pointer_up(event);
        }
        self.gesture = None;
        true
    }

    pub fn cancel_gesture(&mut self) -> bool {
        let Some(pointer_id) = self.gesture else {
            return false;
        };
        let init = web_sys::PointerEventInit::new();
        init.set_pointer_id(pointer_id);
        init.set_pointer_type("touch");
        let Ok(event) = web_sys::PointerEvent::new_with_event_init_dict("pointercancel", &init)
        else {
            return false;
        };
        self.end(&event, true)
    }

    pub fn take_commit(
        &mut self,
        doc: &InkstoneDocument,
        text: Option<&mut TextClientState>,
        editing: Option<BlockId>,
    ) -> Result<PageCommit, JsValue> {
        let update = self.take_update()?;
        self.refresh_bounds();
        let mut anchor = self
            .pending_anchor
            .take()
            .filter(|(group, _)| self.bounds.contains_key(group));
        if let (Some((group, pending)), Some(text)) = (anchor.as_mut(), text) {
            if matches!(pending.target, AnchorTarget::Text { .. }) {
                self.refine_text_anchor(*group, pending, doc, text, editing);
            }
        }
        if let Some(group) = self.active_group.take() {
            if self.bounds.contains_key(&group) {
                self.recent = Some(RecentGroup {
                    group,
                    at_ms: js_sys::Date::now(),
                });
            } else if !self.frames.is_empty() && anchor.is_none() {
                self.frames.remove(&group);
                self.push_frames();
            }
        }
        Ok(PageCommit { update, anchor })
    }

    fn refine_text_anchor(
        &mut self,
        group: InkGroupId,
        anchor: &mut InkAnchor,
        doc: &InkstoneDocument,
        text: &mut TextClientState,
        editing: Option<BlockId>,
    ) {
        let (Some(&(x, y, _)), Some(bounds)) = (self.frames.get(&group), self.bounds.get(&group))
        else {
            return;
        };
        let Some(surface) = dom_document()
            .and_then(|document| document.query_selector(".page-surface").ok().flatten())
        else {
            return;
        };
        let page = surface.get_bounding_client_rect();
        let origin = (page.left(), page.top());
        let client = (
            origin.0 + f64::from(x + (bounds.min_x + bounds.max_x) / 2.0),
            origin.1 + f64::from(y + (bounds.min_y + bounds.max_y) / 2.0),
        );
        let Some((target, at)) = word_anchor_at(&surface, origin, client, doc, text, editing)
        else {
            return;
        };
        self.block_offsets.remove(&group);
        *anchor = InkAnchor {
            target,
            dx: x - at.0,
            dy: y - at.1,
        };
    }
}

fn block_id_of(element: &web_sys::Element) -> Option<BlockId> {
    element
        .get_attribute("data-page-block-id")
        .and_then(|value| Uuid::parse_str(&value).ok())
        .map(BlockId)
}

fn caret_at(document: &web_sys::Document, x: f64, y: f64) -> Option<(web_sys::Node, u32)> {
    let call = |name: &str| -> Option<JsValue> {
        let function = js_sys::Reflect::get(document, &JsValue::from_str(name))
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()?;
        function
            .call2(document, &JsValue::from_f64(x), &JsValue::from_f64(y))
            .ok()
            .filter(|value| !value.is_null() && !value.is_undefined())
    };
    if let Some(range) = call("caretRangeFromPoint") {
        let range = range.dyn_into::<web_sys::Range>().ok()?;
        return Some((range.start_container().ok()?, range.start_offset().ok()?));
    }
    let position = call("caretPositionFromPoint")?;
    let node = js_sys::Reflect::get(&position, &JsValue::from_str("offsetNode"))
        .ok()?
        .dyn_into::<web_sys::Node>()
        .ok()?;
    let offset = js_sys::Reflect::get(&position, &JsValue::from_str("offset"))
        .ok()?
        .as_f64()?;
    Some((node, offset as u32))
}

fn element_of(node: &web_sys::Node) -> Option<web_sys::Element> {
    node.clone()
        .dyn_into::<web_sys::Element>()
        .ok()
        .or_else(|| node.parent_element())
}

fn span_offsets(source: &str, span: SourceSpan) -> (u32, u32) {
    let start = page_ink::utf16_offset_of_line_col(source, span.start_line, span.start_col);
    let end = page_ink::utf16_offset_of_line_col(source, span.end_line, u32::MAX);
    (start, end.max(start))
}

fn utf16_slice(source: &str, start: u32, end: u32) -> Vec<u16> {
    source
        .encode_utf16()
        .skip(start as usize)
        .take(end.saturating_sub(start) as usize)
        .collect()
}

fn rendered_units(element: &web_sys::Element) -> Vec<u16> {
    element
        .text_content()
        .unwrap_or_default()
        .encode_utf16()
        .collect()
}

fn rendered_index(element: &web_sys::Element, node: &web_sys::Node, offset: u32) -> Option<usize> {
    let range = dom_document()?.create_range().ok()?;
    range.set_start(element, 0).ok()?;
    range.set_end(node, offset).ok()?;
    Some(range.to_string().length() as usize)
}

fn text_position(element: &web_sys::Element, index: usize) -> Option<(web_sys::Node, u32, u32)> {
    let mut remaining = index;
    let mut stack = vec![element.clone().unchecked_into::<web_sys::Node>()];
    let mut last = None;
    while let Some(node) = stack.pop() {
        if node.node_type() == web_sys::Node::TEXT_NODE {
            let length = node
                .node_value()
                .map(|value| value.encode_utf16().count())
                .unwrap_or(0);
            if length == 0 {
                continue;
            }
            if remaining < length {
                return Some((node, remaining as u32, length as u32));
            }
            remaining -= length;
            last = Some((node, length as u32));
            continue;
        }
        let children = node.child_nodes();
        for child in (0..children.length()).rev() {
            if let Some(child) = children.item(child) {
                stack.push(child);
            }
        }
    }
    last.map(|(node, length)| (node, length.saturating_sub(1), length))
}

fn char_origin(element: &web_sys::Element, index: usize, origin: (f64, f64)) -> Option<(f32, f32)> {
    let (node, offset, length) = text_position(element, index)?;
    let range = dom_document()?.create_range().ok()?;
    range.set_start(&node, offset).ok()?;
    range.set_end(&node, (offset + 1).min(length)).ok()?;
    let rect = range
        .get_client_rects()
        .and_then(|rects| rects.get(0))
        .unwrap_or_else(|| range.get_bounding_client_rect());
    if rect.width() == 0.0 && rect.height() == 0.0 {
        return None;
    }
    Some((
        (rect.left() - origin.0) as f32,
        (rect.top() - origin.1) as f32,
    ))
}

fn text_origin(
    block: &web_sys::Element,
    source: &str,
    offset: u32,
    origin: (f64, f64),
) -> Option<(f32, f32)> {
    let elements = sourcepos_elements(block);
    let containing = elements
        .iter()
        .map(|(element, span)| (element, span_offsets(source, *span)))
        .filter(|(_, (start, end))| *start <= offset && offset <= *end)
        .min_by_key(|(_, (start, end))| (end - start, std::cmp::Reverse(*start)));
    if let Some((element, (start, end))) = containing {
        let slice = utf16_slice(source, start, end);
        let rendered = rendered_units(element);
        let index =
            page_ink::source_to_rendered_index(&slice, &rendered, (offset - start) as usize);
        if let Some(at) = char_origin(element, index, origin) {
            return Some(at);
        }
        return Some(top_left(element, origin));
    }
    let line = page_ink::line_of_utf16_offset(source, offset);
    let spans = elements.iter().map(|(_, span)| *span).collect::<Vec<_>>();
    page_ink::innermost_span_for_line(&spans, line)
        .map(|index| top_left(&elements[index].0, origin))
}

fn word_anchor_at(
    surface: &web_sys::Element,
    origin: (f64, f64),
    client: (f64, f64),
    doc: &InkstoneDocument,
    text: &mut TextClientState,
    editing: Option<BlockId>,
) -> Option<(AnchorTarget, (f32, f32))> {
    let document = dom_document()?;
    let (node, offset) = caret_at(&document, client.0, client.1)?;
    let start = element_of(&node)?;
    let block = start.closest("[data-page-block-id]").ok()??;
    if !surface.contains(Some(&block)) {
        return None;
    }
    let block_id = block_id_of(&block)?;
    if editing == Some(block_id) {
        return None;
    }
    let source = markdown_text(doc, block_id)?;
    let element = start.closest("[data-sourcepos]").ok()??;
    if !block.contains(Some(&element)) {
        return None;
    }
    let span = element
        .get_attribute("data-sourcepos")
        .and_then(|value| page_ink::parse_sourcepos(&value))?;
    let rect = element.get_bounding_client_rect();
    if client.1 < rect.top() - 4.0 || client.1 > rect.bottom() + 4.0 {
        return None;
    }
    let (span_start, span_end) = span_offsets(source, span);
    let slice = utf16_slice(source, span_start, span_end);
    let rendered = rendered_units(&element);
    if rendered.is_empty() {
        return None;
    }
    let index = rendered_index(&element, &node, offset)?;
    let index = page_ink::word_start(&rendered, index).min(rendered.len() - 1);
    let source_offset =
        span_start + page_ink::rendered_to_source_index(&slice, &rendered, index) as u32;
    let at = char_origin(&element, index, origin).unwrap_or_else(|| top_left(&element, origin));
    let position = text
        .relative_position(
            TextTarget::Markdown(block_id),
            source_offset,
            TextAssociation::After,
        )
        .ok()?;
    Some((AnchorTarget::Text { position }, at))
}
