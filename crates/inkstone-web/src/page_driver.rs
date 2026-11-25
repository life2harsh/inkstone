//! Driver glue for the unified page: one live-preview text surface with a
//! page-wide ink layer on top.

use std::collections::HashMap;

use inkstone_client::ClientEvent;
use inkstone_client::page_ink;
use inkstone_client::page_text;
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::document::{BlockContent, DocumentBlock, InkstoneDocument};
use inkstone_core::ids::{BlockId, UpdateId};
use inkstone_core::{INK_PRESENCE_VERSION, InkPresencePayload, PageInkUpdate, TextTarget};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::icons;
use crate::page_surface::{PAGE_PRESENCE_BLOCK, PageSurface};
use crate::page_touch;
use crate::{
    Driver, DriverInner, PRESENCE_VERSION, PresenceKind, PresencePayload, RENDERING, ink_actor_id,
    markdown_render, math_render,
};

thread_local! {
    /// Browsers still fire `click` after an ink gesture whose pointerdown was
    /// cancelled. A click at the gesture's release point, soon after it, is
    /// swallowed: `(deadline_ms, client_x, client_y)`.
    static SUPPRESSED_CLICK: std::cell::Cell<(f64, f64, f64)> = const { std::cell::Cell::new((0.0, 0.0, 0.0)) };
    static PAGE_SIGNATURE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

fn editing_block(
    inner: &DriverInner,
    view_doc: &InkstoneDocument,
    editable: bool,
) -> Option<BlockId> {
    if !editable {
        return None;
    }
    // A note that is only an empty text block is ready to type into.
    let lone_empty = match view_doc.blocks.as_slice() {
        [block] if matches!(&block.content, BlockContent::Markdown { text } if text.is_empty()) => {
            Some(block.id)
        }
        _ => None,
    };
    inner.editing_block.or(lone_empty)
}

fn page_signature(
    view_doc: &InkstoneDocument,
    editing: Option<BlockId>,
    editable: bool,
    ink_mode: bool,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (editable, ink_mode, editing, view_doc.blocks.len()).hash(&mut hasher);
    for block in &view_doc.blocks {
        block.id.hash(&mut hasher);
        if Some(block.id) == editing {
            continue;
        }
        match &block.content {
            BlockContent::Markdown { text } => (0u8, text).hash(&mut hasher),
            BlockContent::Math { source } => (1u8, source).hash(&mut hasher),
            BlockContent::Ink { .. } => 2u8.hash(&mut hasher),
            BlockContent::Divider => 3u8.hash(&mut hasher),
            BlockContent::Space { min_height } => (4u8, min_height.to_bits()).hash(&mut hasher),
        }
    }
    hasher.finish()
}

/// Whether `event` is the trailing click of the last ink gesture.
fn is_gesture_click(event: &web_sys::MouseEvent) -> bool {
    let (deadline, x, y) = SUPPRESSED_CLICK.with(|cell| cell.replace((0.0, 0.0, 0.0)));
    js_sys::Date::now() <= deadline
        && (f64::from(event.client_x()) - x).abs() <= 4.0
        && (f64::from(event.client_y()) - y).abs() <= 4.0
}

/// Tools that create strokes and therefore need a group and an anchor.
fn tool_draws(tool: &str) -> bool {
    matches!(tool, "pen" | "pencil" | "highlighter")
}

const DOCK_TOOLS: [(&str, &str); 5] = [
    ("pen", "Pen"),
    ("highlighter", "Highlighter"),
    ("eraser", "Eraser"),
    ("lasso", "Lasso"),
    ("comment", "Comment"),
];
const DOCK_COLORS: [(&str, &str); 8] = [
    ("Ink", "31,41,55"),
    ("Graphite", "100,116,139"),
    ("Blue", "37,99,235"),
    ("Violet", "124,58,237"),
    ("Red", "220,38,38"),
    ("Orange", "234,88,12"),
    ("Green", "22,163,74"),
    ("Yellow", "234,179,8"),
];
const MIN_BRUSH: f32 = 1.0;
const MAX_BRUSH: f32 = 24.0;
const SVG_NS: &str = "http://www.w3.org/2000/svg";

const BRUSH_KEY: &str = "inkstone.brush";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn parse_brush(value: &str) -> Option<(f32, (u8, u8, u8))> {
    let (width, rgb) = value.split_once('|')?;
    let width = width
        .parse::<f32>()
        .ok()
        .filter(|width| width.is_finite())?;
    let rgb = rgb
        .split(',')
        .filter_map(|part| part.parse::<u8>().ok())
        .collect::<Vec<_>>();
    let [r, g, b] = rgb[..] else {
        return None;
    };
    Some((width.clamp(MIN_BRUSH, MAX_BRUSH), (r, g, b)))
}

pub(crate) fn tool_brush(tool: &str) -> (f32, (u8, u8, u8)) {
    let storage = local_storage();
    let saved = |key: String| storage.as_ref()?.get_item(&key).ok().flatten();
    saved(format!("{BRUSH_KEY}.{tool}"))
        .and_then(|value| parse_brush(&value))
        .or_else(|| {
            saved(BRUSH_KEY.to_string())?
                .strip_prefix(&format!("{tool}|"))
                .and_then(parse_brush)
        })
        .unwrap_or(if tool == "highlighter" {
            (14.0, (234, 179, 8))
        } else {
            (3.0, (31, 41, 55))
        })
}

pub(crate) fn load_brush() -> (String, f32, (u8, u8, u8)) {
    let tool = local_storage()
        .and_then(|storage| storage.get_item(BRUSH_KEY).ok().flatten())
        .and_then(|value| value.split('|').next().map(str::to_string))
        .filter(|tool| DOCK_TOOLS.iter().any(|(name, _)| name == tool))
        .unwrap_or_else(|| "pen".into());
    let (width, color) = tool_brush(if tool_draws(&tool) { &tool } else { "pen" });
    (tool, width, color)
}

fn save_brush(tool: &str, width: f32, color: (u8, u8, u8)) {
    if let Some(storage) = local_storage().filter(|_| tool != "comment") {
        let _ = storage.set_item(BRUSH_KEY, tool);
        if tool_draws(tool) {
            let _ = storage.set_item(
                &format!("{BRUSH_KEY}.{tool}"),
                &format!("{width}|{},{},{}", color.0, color.1, color.2),
            );
        }
    }
}

fn brush_style(rgb: &str, width: f32) -> String {
    let fill = (width - MIN_BRUSH) / (MAX_BRUSH - MIN_BRUSH) * 100.0;
    format!(
        "--swatch: rgb({rgb}); --brush: {width}px; --dot: {}px; --fill: {fill:.1}%",
        (width + 3.0).clamp(5.0, 18.0)
    )
}

fn format_width(width: f32) -> String {
    if width.fract() == 0.0 {
        format!("{width:.0}")
    } else {
        format!("{width:.1}")
    }
}

fn element(document: &web_sys::Document, tag: &str, class: &str) -> web_sys::Element {
    let element = document.create_element(tag).unwrap();
    element.set_class_name(class);
    element
}

pub(crate) fn focus_in_place(element: &web_sys::HtmlElement) {
    let options = web_sys::FocusOptions::new();
    options.set_prevent_scroll(true);
    let _ = element.focus_with_options(&options);
}

pub(crate) fn autosize(textarea: &web_sys::HtmlElement) {
    let scroller = textarea.closest(".main-content").ok().flatten();
    let top = scroller.as_ref().map(web_sys::Element::scroll_top);
    let style = textarea.style();
    let _ = style.set_property("height", "auto");
    let _ = style.set_property("height", &format!("{}px", textarea.scroll_height()));
    if let (Some(scroller), Some(top)) = (scroller, top)
        && scroller.scroll_top() != top
    {
        scroller.set_scroll_top(top);
    }
}

pub(crate) fn blur_page_source() {
    if let Some(active) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|element| element.class_list().contains("page-source"))
    {
        let _ = active.blur();
    }
}

/// Whether the event happened inside a dialog or panel with its own keys.
fn closest_dialog(event: &web_sys::Event) -> bool {
    event_element(event).is_some_and(|element| {
        closest(
            &element,
            ".settings-overlay, .search-panel, [role=\"dialog\"]",
        )
        .is_some()
    })
}

fn event_element(event: &web_sys::Event) -> Option<web_sys::Element> {
    event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
}

fn closest(element: &web_sys::Element, selector: &str) -> Option<web_sys::Element> {
    element.closest(selector).ok().flatten()
}

fn parse_block_id(element: &web_sys::Element, attribute: &str) -> Option<BlockId> {
    element
        .get_attribute(attribute)
        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
        .map(BlockId)
}

impl Driver {
    pub(crate) fn render_page_surface(
        &self,
        document: &web_sys::Document,
        container: &web_sys::Element,
        inner: &DriverInner,
        view_doc: &InkstoneDocument,
        current_path: &str,
        editable: bool,
    ) {
        let surface = element(document, "div", "page-surface");
        surface.set_attribute("data-page-surface", "").unwrap();
        if !editable {
            surface.class_list().add_1("read-only").unwrap();
        } else if inner.ink_mode {
            surface.class_list().add_1("ink-mode").unwrap();
        }
        let editing = editing_block(inner, view_doc, editable);
        PAGE_SIGNATURE.with(|cell| {
            cell.set(Some(page_signature(
                view_doc,
                editing,
                editable,
                inner.ink_mode,
            )))
        });
        let mut heading_counts = HashMap::new();
        let block_count = view_doc.blocks.len();

        for (index, block) in view_doc.blocks.iter().enumerate() {
            let id = block.id.to_string();
            let kind = match &block.content {
                BlockContent::Markdown { .. } => "markdown",
                BlockContent::Math { .. } => "math",
                BlockContent::Ink { .. } => "ink",
                BlockContent::Divider => "divider",
                BlockContent::Space { .. } => "space",
            };
            let is_editing = editing == Some(block.id);
            let section = element(
                document,
                "section",
                &if is_editing {
                    format!("page-block page-{kind} editing")
                } else {
                    format!("page-block page-{kind}")
                },
            );
            section.set_attribute("data-page-block-id", &id).unwrap();
            section.set_attribute("data-block-type", kind).unwrap();
            if let BlockContent::Markdown { text } = &block.content {
                section
                    .set_attribute("data-md", page_text::source_kind(text).as_str())
                    .unwrap();
            }

            if editable {
                let gutter = element(document, "div", "page-gutter");
                let delete = element(document, "button", "page-gutter-btn");
                delete.set_attribute("data-block-action", "delete").unwrap();
                delete.set_attribute("data-block-id", &id).unwrap();
                icons::icon_button_content(document, &delete, "trash", "Delete block");
                gutter.append_child(&delete).unwrap();
                if block_count > 1 {
                    let grip = element(document, "button", "page-gutter-btn page-grip");
                    grip.set_attribute("data-block-id", &id).unwrap();
                    icons::icon_button_content(document, &grip, "grip", "Drag to move block");
                    gutter.append_child(&grip).unwrap();
                }
                section.append_child(&gutter).unwrap();
            }

            match &block.content {
                BlockContent::Markdown { text } if is_editing => {
                    section
                        .append_child(&self.source_textarea(
                            document,
                            inner,
                            block.id,
                            "markdown",
                            text,
                            index,
                            block_count == 1,
                        ))
                        .unwrap();
                }
                BlockContent::Markdown { text } => {
                    let rendered = element(document, "div", "page-rendered preview-content");
                    if text.trim().is_empty() {
                        // An empty block is a blank line; only a page that is
                        // nothing but one says what to do.
                        rendered.class_list().add_1("page-placeholder").unwrap();
                        if editable && block_count == 1 {
                            rendered.set_text_content(Some(
                                "Write, type / for blocks, or draw anywhere with a pen.",
                            ));
                        }
                    } else {
                        let html = markdown_render::render_page_block(
                            text,
                            current_path,
                            &inner.knowledge,
                            &mut heading_counts,
                        );
                        rendered.set_inner_html(&if editable {
                            markdown_render::enable_task_toggles(&html)
                        } else {
                            html
                        });
                    }
                    section.append_child(&rendered).unwrap();
                }
                BlockContent::Math { source } if is_editing => {
                    section
                        .append_child(&self.source_textarea(
                            document, inner, block.id, "math", source, index, false,
                        ))
                        .unwrap();
                }
                BlockContent::Math { source } => {
                    let math = element(document, "div", "math-display page-rendered");
                    if source.trim().is_empty() {
                        math.class_list().add_1("page-placeholder").unwrap();
                        math.set_text_content(Some(if editable { "Empty equation" } else { "" }));
                    } else {
                        math.set_inner_html(&math_render::render_mathml(source));
                    }
                    section.append_child(&math).unwrap();
                }
                BlockContent::Space { min_height } => {
                    section
                        .set_attribute("style", &format!("height:{}px", min_height.round()))
                        .unwrap();
                    section
                        .set_attribute("aria-label", "Writing space")
                        .unwrap();
                }
                BlockContent::Divider => {
                    section
                        .append_child(&element(document, "hr", "document-divider"))
                        .unwrap();
                }
                BlockContent::Ink { .. } if editable => {
                    let note = element(document, "div", "page-placeholder");
                    note.set_text_content(Some("Moving this drawing onto the page…"));
                    section.append_child(&note).unwrap();
                }
                BlockContent::Ink { .. } => {
                    // Legacy drawing in a read-only view: shown as before.
                    let ink_block = element(document, "div", "ink-block ink-preview");
                    ink_block.set_attribute("data-block-id", &id).unwrap();
                    let host = element(document, "div", "ink-canvas-host");
                    let canvas = document.create_element("canvas").unwrap();
                    canvas.set_id(&format!("ink-canvas-{}", block.id));
                    canvas.set_attribute("data-block-id", &id).unwrap();
                    canvas.set_attribute("role", "img").unwrap();
                    canvas
                        .set_attribute("aria-label", "Read-only Stonepen drawing")
                        .unwrap();
                    host.append_child(&canvas).unwrap();
                    ink_block.append_child(&host).unwrap();
                    section.append_child(&ink_block).unwrap();
                }
            }
            surface.append_child(&section).unwrap();
        }

        if editable {
            let insert = element(document, "div", "page-insert");
            for (kind, glyph, text) in [
                ("markdown", "text", "Text"),
                ("math", "sigma", "Math"),
                ("space", "space", "Writing space"),
                ("divider", "minus", "Divider"),
                ("image", "image", "Image or PDF"),
            ] {
                let button = element(document, "button", "add-block-btn");
                button.set_attribute("data-block-type", kind).unwrap();
                button.append_child(&icons::icon(document, glyph)).unwrap();
                button
                    .append_child(&document.create_text_node(text))
                    .unwrap();
                insert.append_child(&button).unwrap();
            }
            surface.append_child(&insert).unwrap();
        }

        let tail = element(document, "div", "page-tail");
        tail.set_attribute("data-page-tail", "").unwrap();
        if editable && view_doc.blocks.is_empty() {
            tail.set_text_content(Some("Click to start writing. Draw anywhere with a pen."));
        }
        surface.append_child(&tail).unwrap();
        container.append_child(&surface).unwrap();

        if editable {
            container
                .append_child(&self.render_ink_dock(document, inner))
                .unwrap();
        }
    }

    pub(crate) fn reconcile_page_blocks(
        &self,
        document: &web_sys::Document,
        active: &web_sys::Element,
    ) {
        let Ok(Some(live)) = document.query_selector(".page-surface[data-page-surface]") else {
            return;
        };
        let holder = element(document, "div", "");
        {
            let inner = self.0.borrow();
            let view_doc = Driver::effective_document(&inner);
            let editable = crate::can_edit_workspace(&inner)
                && inner.document_mode == crate::DocumentMode::Edit
                && crate::document_ready(&inner);
            let editing = editing_block(&inner, &view_doc, editable);
            let signature = page_signature(&view_doc, editing, editable, inner.ink_mode);
            if PAGE_SIGNATURE.with(|cell| cell.get()) == Some(signature) {
                return;
            }
            let path = inner.selected_path.clone().unwrap_or_default();
            self.render_page_surface(document, &holder, &inner, &view_doc, &path, editable);
        }
        let Ok(Some(fresh)) = holder.query_selector(".page-surface") else {
            return;
        };
        live.set_class_name(&fresh.class_name());
        crate::page_morph::morph(&live, &fresh, Some(active));
        self.hydrate_markdown_assets(document);
        self.layout_page();
    }

    fn source_textarea(
        &self,
        document: &web_sys::Document,
        inner: &DriverInner,
        block_id: BlockId,
        kind: &str,
        value: &str,
        index: usize,
        alone: bool,
    ) -> web_sys::Element {
        let textarea = document.create_element("textarea").unwrap();
        let textarea: web_sys::HtmlTextAreaElement = textarea.unchecked_into();
        textarea.set_class_name(if kind == "math" {
            "block-textarea math-source-input page-source"
        } else {
            "block-textarea markdown-source-input page-source"
        });
        textarea.set_placeholder(if kind == "math" {
            "TeX source"
        } else if alone {
            "Write, type / for blocks, or draw anywhere with a pen."
        } else {
            "Type / for blocks"
        });
        if kind == "markdown" {
            textarea
                .class_list()
                .add_1(&format!(
                    "source-{}",
                    page_text::source_kind(value).as_str()
                ))
                .unwrap();
        }
        textarea
            .set_attribute("aria-label", &format!("Block {} source", index + 1))
            .unwrap();
        textarea.set_rows(1);
        textarea.set_disabled(inner.local_persistence_error.is_some());
        textarea
            .set_attribute("data-block-id", &block_id.to_string())
            .unwrap();
        textarea.set_attribute("data-block-kind", kind).unwrap();
        textarea.set_value(value);
        textarea
            .set_attribute("data-canonical-value", value)
            .unwrap();
        textarea.into()
    }

    fn render_ink_dock(
        &self,
        document: &web_sys::Document,
        inner: &DriverInner,
    ) -> web_sys::Element {
        let wrap = element(document, "div", "ink-dock-wrap");
        let dock = element(document, "div", "ink-dock");
        dock.set_attribute("role", "toolbar").unwrap();
        dock.set_attribute("aria-label", "Ink tools").unwrap();

        let modes = element(document, "div", "ink-dock-modes");
        let type_btn = element(document, "button", "ink-dock-btn");
        type_btn.set_attribute("data-page-ink", "type").unwrap();
        icons::icon_button_content(document, &type_btn, "type", "Type");
        type_btn
            .set_attribute("title", "Type: click and type. Pick a pen to draw")
            .unwrap();
        if !inner.ink_mode {
            type_btn.class_list().add_1("active").unwrap();
        }
        modes.append_child(&type_btn).unwrap();
        for (tool, label) in DOCK_TOOLS {
            let button = element(document, "button", "ink-dock-btn");
            button.set_attribute("data-page-ink", "tool").unwrap();
            button.set_attribute("data-tool", tool).unwrap();
            icons::icon_button_content(document, &button, tool, label);
            if inner.ink_tool == tool {
                button
                    .class_list()
                    .add_1(if inner.ink_mode { "active" } else { "pen-tool" })
                    .unwrap();
            }
            modes.append_child(&button).unwrap();
        }
        dock.append_child(&modes).unwrap();

        let rgb = format!(
            "{},{},{}",
            inner.ink_brush_color.0, inner.ink_brush_color.1, inner.ink_brush_color.2
        );
        let style_btn = element(document, "button", "ink-dock-style");
        style_btn.set_attribute("data-page-ink", "style").unwrap();
        style_btn
            .set_attribute("aria-label", "Color and size")
            .unwrap();
        style_btn.set_attribute("title", "Color and size").unwrap();
        style_btn
            .set_attribute(
                "aria-expanded",
                if inner.ink_panel_open {
                    "true"
                } else {
                    "false"
                },
            )
            .unwrap();
        style_btn
            .set_attribute("style", &brush_style(&rgb, inner.ink_brush_width))
            .unwrap();
        if inner.ink_panel_open {
            style_btn.class_list().add_1("open").unwrap();
        }
        style_btn
            .append_child(&element(document, "span", "ink-style-chip"))
            .unwrap();
        dock.append_child(&style_btn).unwrap();

        dock.append_child(&element(document, "span", "ink-dock-sep"))
            .unwrap();
        let has_selection = self
            .1
            .try_borrow()
            .ok()
            .and_then(|registry| registry.page.as_ref().map(PageSurface::has_selection))
            .unwrap_or(false);
        for (action, glyph, label, show) in [
            ("undo", "undo", "Undo", true),
            ("redo", "redo", "Redo", true),
            ("delete", "trash", "Delete", has_selection),
        ] {
            if !show {
                continue;
            }
            let button = element(document, "button", "ink-dock-btn");
            button.set_attribute("data-page-ink", action).unwrap();
            icons::icon_button_content(document, &button, glyph, label);
            dock.append_child(&button).unwrap();
        }

        if inner.ink_panel_open {
            wrap.append_child(&self.render_ink_panel(document, inner, &rgb))
                .unwrap();
        } else if inner.ink_mode && page_touch::needs_scroll_hint() {
            let hint = element(document, "div", "ink-scroll-hint");
            hint.set_text_content(Some("Two fingers to scroll"));
            wrap.append_child(&hint).unwrap();
        }
        wrap.append_child(&dock).unwrap();
        wrap
    }

    fn render_ink_panel(
        &self,
        document: &web_sys::Document,
        inner: &DriverInner,
        rgb: &str,
    ) -> web_sys::Element {
        let panel = element(document, "div", "ink-panel");
        panel.set_attribute("role", "dialog").unwrap();
        panel.set_attribute("aria-label", "Color and size").unwrap();
        panel
            .set_attribute("style", &brush_style(rgb, inner.ink_brush_width))
            .unwrap();
        if inner.ink_tool == "highlighter" {
            panel.class_list().add_1("highlighter").unwrap();
        }

        let preview = element(document, "div", "ink-panel-preview");
        let svg = document.create_element_ns(Some(SVG_NS), "svg").unwrap();
        svg.set_attribute("viewBox", "0 0 240 56").unwrap();
        svg.set_attribute("preserveAspectRatio", "xMidYMid meet")
            .unwrap();
        svg.set_attribute("aria-hidden", "true").unwrap();
        let path = document.create_element_ns(Some(SVG_NS), "path").unwrap();
        path.set_attribute(
            "d",
            "M18 36 C 48 10, 74 12, 92 28 S 136 48, 160 28 S 206 12, 222 22",
        )
        .unwrap();
        svg.append_child(&path).unwrap();
        preview.append_child(&svg).unwrap();
        panel.append_child(&preview).unwrap();

        let colors = element(document, "div", "ink-panel-colors");
        for (name, value) in DOCK_COLORS {
            let swatch = element(document, "button", "ink-panel-color");
            swatch.set_attribute("data-page-ink", "color").unwrap();
            swatch.set_attribute("data-color", value).unwrap();
            swatch.set_attribute("aria-label", name).unwrap();
            swatch.set_attribute("title", name).unwrap();
            swatch
                .set_attribute("style", &format!("--swatch: rgb({value})"))
                .unwrap();
            if rgb == value {
                swatch.class_list().add_1("active").unwrap();
                swatch.set_attribute("aria-pressed", "true").unwrap();
            }
            colors.append_child(&swatch).unwrap();
        }
        panel.append_child(&colors).unwrap();

        let size = element(document, "label", "ink-panel-size");
        let caption = element(document, "span", "ink-panel-label");
        caption.set_text_content(Some("Size"));
        size.append_child(&caption).unwrap();
        let slider = element(document, "input", "ink-size-slider");
        for (key, value) in [
            ("type", "range"),
            ("data-page-ink", "size"),
            ("min", &MIN_BRUSH.to_string()),
            ("max", &MAX_BRUSH.to_string()),
            ("step", "0.5"),
            ("aria-label", "Brush size"),
        ] {
            slider.set_attribute(key, value).unwrap();
        }
        if let Ok(slider) = slider.clone().dyn_into::<web_sys::HtmlInputElement>() {
            slider.set_value(&inner.ink_brush_width.to_string());
        }
        size.append_child(&slider).unwrap();
        let value = element(document, "output", "ink-size-value");
        value.set_text_content(Some(&format_width(inner.ink_brush_width)));
        size.append_child(&value).unwrap();
        panel.append_child(&size).unwrap();
        panel
    }

    pub(crate) fn preview_brush_width(&self, width: f32) {
        let width = width.clamp(MIN_BRUSH, MAX_BRUSH);
        let (tool, color) = {
            let mut inner = self.0.borrow_mut();
            inner.ink_brush_width = width;
            (inner.ink_tool.clone(), inner.ink_brush_color)
        };
        let rgb = format!("{},{},{}", color.0, color.1, color.2);
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            if let Ok(nodes) = document.query_selector_all(".ink-panel, .ink-dock-style") {
                for index in 0..nodes.length() {
                    if let Some(node) = nodes
                        .item(index)
                        .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                    {
                        let _ = node.set_attribute("style", &brush_style(&rgb, width));
                    }
                }
            }
            if let Ok(Some(output)) = document.query_selector(".ink-size-value") {
                output.set_text_content(Some(&format_width(width)));
            }
        }
        save_brush(&tool, width, color);
        if let Ok(mut registry) = self.1.try_borrow_mut() {
            if let Some(page) = registry.page.as_mut() {
                page.configure(&tool, width, color);
            }
        }
    }

    /// Mounts, follows, configures and lays out the page ink layer after a
    /// render. Also starts the one-time migration of legacy Ink blocks.
    pub(crate) fn sync_page_surface(&self, view_doc: &InkstoneDocument, editable: bool) {
        let has_surface = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(".page-surface").ok().flatten())
            .is_some();
        let (device_id, tool, width, color, editing) = {
            let inner = self.0.borrow();
            (
                inner.device_id,
                inner.ink_tool.clone(),
                inner.ink_brush_width,
                inner.ink_brush_color,
                inner.editing_block,
            )
        };

        let mut leftover = None;
        let mut mount_error = None;
        {
            let mut registry = self.1.borrow_mut();
            if !has_surface {
                if let Some(page) = registry.page.take() {
                    let doc_id = page.doc_id();
                    leftover = page.destroy().ok().flatten().map(|update| (doc_id, update));
                }
            } else {
                let keep = match registry.page.as_mut() {
                    Some(page) if page.doc_id() == view_doc.id && page.read_only() == !editable => {
                        page.gesture_active() || page.sync(view_doc.ink.as_ref()).unwrap_or(false)
                    }
                    _ => false,
                };
                if !keep {
                    if let Some(page) = registry.page.take() {
                        let doc_id = page.doc_id();
                        leftover = page.destroy().ok().flatten().map(|update| (doc_id, update));
                    }
                    let actor = ink_actor_id(device_id, view_doc.id, BlockId(PAGE_PRESENCE_BLOCK));
                    match PageSurface::mount(view_doc.id, view_doc.ink.as_ref(), actor, !editable) {
                        Ok(page) => registry.page = Some(page),
                        Err(error) => mount_error = Some(error),
                    }
                }
            }
        }
        if let Some(error) = mount_error {
            self.report_ink_error("Failed to mount the page ink layer", error);
        }
        if let Some((doc_id, update)) = leftover {
            if doc_id == view_doc.id {
                self.dispatch_page_op(DocumentOp::ApplyPageInkUpdate { payload: update });
            }
        }
        if has_surface {
            let inner = self.0.borrow();
            if let Ok(mut registry) = self.1.try_borrow_mut() {
                if let Some(page) = registry.page.as_mut() {
                    page.configure(&tool, width, color);
                    page.layout(view_doc, inner.text_client_state.as_ref(), editing);
                    crate::comments::publish(page, view_doc, &inner);
                }
            }
        }
        if editable {
            self.maybe_migrate_ink_blocks(view_doc);
        }
    }

    fn maybe_migrate_ink_blocks(&self, view_doc: &InkstoneDocument) {
        if view_doc.ink.is_some()
            || !view_doc
                .blocks
                .iter()
                .any(|block| matches!(block.content, BlockContent::Ink { .. }))
            || !self.0.borrow_mut().migrated_docs.insert(view_doc.id)
        {
            return;
        }
        match inkstone_core::plan_ink_block_migration(view_doc) {
            Ok(Some(op)) => {
                let this = self.clone();
                let doc_id = view_doc.id;
                wasm_bindgen_futures::spawn_local(async move {
                    if this.0.borrow().selected_doc_id == doc_id {
                        this.dispatch_page_op(op);
                    }
                });
            }
            Ok(None) => {}
            Err(error) => self.report_ink_error(
                "Failed to move Ink blocks onto the page",
                JsValue::from_str(&error),
            ),
        }
    }

    pub(crate) fn dispatch_page_op(&self, op: DocumentOp) {
        self.dispatch(ClientEvent::LocalIntent {
            op,
            update_id: UpdateId::new_v4(),
            applied_at_ms: js_sys::Date::now() as i64,
        });
    }

    fn dispatch_page_update(&self, update: Option<PageInkUpdate>) {
        if let Some(payload) = update {
            self.dispatch_page_op(DocumentOp::ApplyPageInkUpdate { payload });
        }
    }

    /// Sends any page ink not yet dispatched (before navigation and renders).
    pub(crate) fn flush_page_ink(&self) {
        let pending = {
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            let Some(page) = registry.page.as_mut() else {
                return;
            };
            if page.gesture_active() {
                return;
            }
            (page.doc_id(), page.take_update())
        };
        match pending {
            (doc_id, Ok(update)) if doc_id == self.0.borrow().selected_doc_id => {
                self.dispatch_page_update(update)
            }
            (_, Ok(_)) => {}
            (_, Err(error)) => self.report_ink_error("Failed to export page ink", error),
        }
    }

    /// Re-lays out the page ink after the page geometry changed.
    pub(crate) fn layout_page(&self) {
        let inner = self.0.borrow();
        let Ok(mut registry) = self.1.try_borrow_mut() else {
            return;
        };
        let Some(page) = registry.page.as_mut() else {
            return;
        };
        let doc = Driver::effective_document(&inner);
        page.layout(&doc, inner.text_client_state.as_ref(), inner.editing_block);
        crate::comments::publish(page, &doc, &inner);
    }

    fn update_page_view(&self) {
        if let Ok(mut registry) = self.1.try_borrow_mut() {
            if let Some(page) = registry.page.as_mut() {
                page.update_view();
            }
        }
    }

    pub(crate) fn focus_pending_block_caret(&self, document: &web_sys::Document) {
        let Some((block_id, caret)) = self.0.borrow_mut().pending_block_caret.take() else {
            return;
        };
        let selector = format!(".page-source[data-block-id=\"{}\"]", block_id);
        let Ok(Some(element)) = document.query_selector(&selector) else {
            return;
        };
        let textarea: web_sys::HtmlTextAreaElement = element.unchecked_into();
        autosize(&textarea);
        focus_in_place(&textarea);
        let _ = textarea.set_selection_range(caret, caret);
        self.layout_page();
    }

    pub(crate) fn start_editing(&self, block_id: BlockId, caret: u32) {
        self.history_boundary();
        blur_page_source();
        let left = self.0.borrow().editing_block.filter(|id| *id != block_id);
        if let Some(left) = left {
            self.normalize_block(left);
        }
        {
            let mut inner = self.0.borrow_mut();
            inner.editing_block = Some(block_id);
            inner.pending_block_caret = Some((block_id, caret));
        }
        self.render();
    }

    pub(crate) fn stop_editing(&self) -> bool {
        let left = self.0.borrow_mut().editing_block.take();
        if let Some(left) = left {
            // A focused source control makes render reconcile in place.
            blur_page_source();
            self.normalize_block(left);
            self.render();
        }
        left.is_some()
    }

    pub(crate) fn page_editable(&self) -> bool {
        let inner = self.0.borrow();
        inner.selected_path.is_some()
            && inner.document_mode == crate::DocumentMode::Edit
            && crate::can_edit_workspace(&inner)
            && crate::document_ready(&inner)
            && !inner.settings_open
    }

    /// A click on a rendered task's checkbox: flips `[ ]`/`[x]` on its line.
    fn toggle_task(&self, checkbox: &web_sys::Element) {
        let Some(block_id) = closest(checkbox, "[data-page-block-id]")
            .and_then(|block| parse_block_id(&block, "data-page-block-id"))
        else {
            return;
        };
        let Some(line) = closest(checkbox, "li[data-sourcepos]")
            .and_then(|item| item.get_attribute("data-sourcepos"))
            .and_then(|pos| pos.split(':').next()?.parse::<usize>().ok())
            .and_then(|line| line.checked_sub(1))
        else {
            return;
        };
        let text = {
            let inner = self.0.borrow();
            let doc = Driver::effective_document(&inner);
            match doc.blocks.iter().find(|block| block.id == block_id) {
                Some(DocumentBlock {
                    content: BlockContent::Markdown { text },
                    ..
                }) => text.clone(),
                _ => return,
            }
        };
        let Some(next) = inkstone_client::page_text::toggle_task(&text, line) else {
            return;
        };
        if self.0.borrow().editing_block.is_some() {
            blur_page_source();
            self.0.borrow_mut().editing_block = None;
        }
        let _ = checkbox.unchecked_ref::<web_sys::HtmlElement>().blur();
        self.history_boundary();
        self.ensure_text_target(TextTarget::Markdown(block_id));
        self.dispatch_text_edit(TextTarget::Markdown(block_id), &text, &next);
        self.history_boundary();
        self.render();
    }

    /// Clicks on the page: enter source editing for a block, start writing
    /// from the tail, leave editing elsewhere, and the ink dock. Returns
    /// whether the click was consumed.
    fn page_click(&self, event: &web_sys::MouseEvent) -> bool {
        if is_gesture_click(event) {
            event.prevent_default();
            return true;
        }
        let Some(target) = event_element(event) else {
            return false;
        };
        if closest(&target, ".comment-layer").is_some() {
            return false;
        }
        if closest(&target, ".pdf-dark-btn").is_some() {
            event.prevent_default();
            crate::pdf_pages::toggle_dark();
            return true;
        }
        if let Some(action) = closest(&target, "[data-page-ink]") {
            self.page_ink_action(&action);
            return true;
        }
        if !self.page_editable() {
            return false;
        }
        let commenting = {
            let inner = self.0.borrow();
            inner.ink_mode && inner.ink_tool == "comment"
        };
        if commenting && closest(&target, ".page-surface").is_some() {
            event.prevent_default();
            self.place_comment(f64::from(event.client_x()), f64::from(event.client_y()));
            return true;
        }
        let editing = self.0.borrow().editing_block;
        let in_editing_block = editing.is_some_and(|id| {
            closest(&target, &format!("[data-page-block-id=\"{id}\"]")).is_some()
        });
        if in_editing_block {
            return false;
        }
        if closest(&target, ".page-surface").is_none() {
            return false;
        }
        if closest(&target, ".page-source").is_some() {
            return false;
        }
        if target.class_list().contains("task-toggle") {
            event.prevent_default();
            self.toggle_task(&target);
            return true;
        }
        if closest(
            &target,
            "a, button, input, select, textarea, .page-gutter, .page-insert",
        )
        .is_some()
        {
            if editing.is_some() {
                // The control's own handler re-renders.
                self.0.borrow_mut().editing_block = None;
            }
            return false;
        }
        if let Some(block) = closest(&target, "[data-page-block-id]") {
            let Some(block_id) = parse_block_id(&block, "data-page-block-id") else {
                return false;
            };
            let doc = {
                let inner = self.0.borrow();
                Driver::effective_document(&inner)
            };
            let caret = match doc.get_block(block_id).map(|block| &block.content) {
                Some(BlockContent::Markdown { text }) => {
                    crate::page_editing::source_caret_at_point(
                        &block,
                        text,
                        f64::from(event.client_x()),
                        f64::from(event.client_y()),
                    )
                    .or_else(|| {
                        closest(&target, "[data-sourcepos]")
                            .and_then(|element| element.get_attribute("data-sourcepos"))
                            .and_then(|value| page_ink::parse_sourcepos(&value))
                            .map(|span| {
                                page_ink::utf16_offset_of_line_col(
                                    text,
                                    span.end_line,
                                    span.end_col + 1,
                                )
                            })
                    })
                    .unwrap_or(text.encode_utf16().count() as u32)
                }
                Some(BlockContent::Math { source }) => source.encode_utf16().count() as u32,
                _ => {
                    return self.stop_editing();
                }
            };
            event.prevent_default();
            self.edit_block_at(block_id, caret);
            return true;
        }
        if closest(&target, ".page-tail").is_some() {
            let last = {
                let inner = self.0.borrow();
                let doc = Driver::effective_document(&inner);
                doc.blocks.last().map(|block| match &block.content {
                    BlockContent::Markdown { text }
                        if !text
                            .lines()
                            .rfind(|line| !line.trim().is_empty())
                            .is_some_and(|line| line.trim_start().starts_with("![")) =>
                    {
                        (block.id, Some(text.encode_utf16().count() as u32))
                    }
                    _ => (block.id, None),
                })
            };
            match last {
                Some((block_id, Some(caret))) => self.start_editing(block_id, caret),
                _ => {
                    let now = js_sys::Date::now() as i64;
                    let block_id = BlockId::new_v4();
                    let idx = self
                        .0
                        .borrow()
                        .client_state
                        .effective_view_doc()
                        .blocks
                        .len();
                    {
                        let mut inner = self.0.borrow_mut();
                        inner.editing_block = Some(block_id);
                        inner.pending_block_caret = Some((block_id, 0));
                    }
                    self.dispatch(ClientEvent::LocalIntent {
                        op: DocumentOp::InsertBlock {
                            idx,
                            block: inkstone_core::document::DocumentBlock {
                                id: block_id,
                                created_at_ms: now,
                                updated_at_ms: now,
                                content: BlockContent::Markdown {
                                    text: String::new(),
                                },
                            },
                        },
                        update_id: UpdateId::new_v4(),
                        applied_at_ms: now,
                    });
                    self.render();
                }
            }
            return true;
        }
        self.stop_editing()
    }

    fn page_ink_action(&self, action: &web_sys::Element) {
        let kind = action.get_attribute("data-page-ink").unwrap_or_default();
        match kind.as_str() {
            "type" => {
                let mut inner = self.0.borrow_mut();
                inner.ink_mode = false;
                inner.ink_panel_open = false;
            }
            "tool" => {
                let tool = action
                    .get_attribute("data-tool")
                    .unwrap_or_else(|| "pen".into());
                let mut inner = self.0.borrow_mut();
                if inner.ink_mode && inner.ink_tool == tool && tool_draws(&tool) {
                    inner.ink_panel_open = !inner.ink_panel_open;
                } else {
                    inner.ink_panel_open = false;
                }
                if inner.ink_tool != tool && tool_draws(&tool) {
                    (inner.ink_brush_width, inner.ink_brush_color) = tool_brush(&tool);
                }
                inner.ink_tool = tool;
                inner.ink_mode = true;
            }
            "color" => {
                let parts = action
                    .get_attribute("data-color")
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|part| part.trim().parse::<u8>().ok())
                    .collect::<Vec<_>>();
                if let [r, g, b] = parts[..] {
                    self.0.borrow_mut().ink_brush_color = (r, g, b);
                }
            }
            "style" => {
                let mut inner = self.0.borrow_mut();
                inner.ink_panel_open = !inner.ink_panel_open;
            }
            "size" => return,
            "undo" | "redo" => {
                if !self.page_editable() {
                    return;
                }
                if kind == "undo" {
                    self.undo_page();
                } else {
                    self.redo_page();
                }
            }
            "delete" => {
                if !self.page_editable() {
                    return;
                }
                let result = {
                    let mut registry = self.1.borrow_mut();
                    let Some(page) = registry.page.as_mut() else {
                        return;
                    };
                    page.delete_selection()
                };
                match result {
                    Ok(update) => {
                        if update.is_some() {
                            self.record_ink_step();
                        }
                        self.dispatch_page_update(update);
                        self.remove_orphaned_anchors();
                    }
                    Err(error) => self.report_ink_error("Page ink command failed", error),
                }
            }
            _ => return,
        }
        if matches!(kind.as_str(), "tool" | "color") {
            let inner = self.0.borrow();
            save_brush(
                &inner.ink_tool,
                inner.ink_brush_width,
                inner.ink_brush_color,
            );
        }
        self.render();
    }

    /// Undoes or redoes one ink step in the ink engine's history.
    pub(crate) fn page_ink_history(&self, undo: bool) {
        let result = {
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            let Some(page) = registry.page.as_mut() else {
                return;
            };
            if undo { page.undo() } else { page.redo() }
        };
        match result {
            Ok(update) => {
                self.dispatch_page_update(update);
                self.remove_orphaned_anchors();
            }
            Err(error) => self.report_ink_error("Page ink command failed", error),
        }
        self.render();
    }

    fn remove_orphaned_anchors(&self) {
        let doc = {
            let inner = self.0.borrow();
            inner.client_state.effective_view_doc().clone()
        };
        let groups = self
            .1
            .borrow_mut()
            .page
            .as_mut()
            .map(|page| page.orphaned_groups(doc.ink.as_ref()))
            .unwrap_or_default();
        if !groups.is_empty() {
            self.dispatch_page_op(DocumentOp::RemoveInkAnchors { groups });
        }
    }

    fn page_pointer_down(&self, event: &web_sys::PointerEvent) {
        let pointer_type = event.pointer_type();
        if pointer_type == "pen" {
            page_touch::note_pen();
        }
        let (ink_mode, tool) = {
            let inner = self.0.borrow();
            (inner.ink_mode, inner.ink_tool.clone())
        };
        let touch = ink_mode && pointer_type == "touch";
        let routed = ink_mode
            && (pointer_type == "pen" || (pointer_type == "mouse" && event.button() == 0) || touch);
        if !routed || !self.page_editable() {
            return;
        }
        if let Some(target) = event_element(event) {
            if closest(
                &target,
                ".ink-dock-wrap, .page-gutter, .page-insert, .workspace-topbar, \
                 .document-header-card, .missing-note-prompt, button, select, \
                 .settings-overlay, .sidebar, .inspector, .comment-layer",
            )
            .is_some()
            {
                return;
            }
            if touch && closest(&target, ".page-surface").is_none() {
                return;
            }
        }
        if touch {
            let count = page_touch::touch_down(
                event.pointer_id(),
                f64::from(event.client_x()),
                f64::from(event.client_y()),
            );
            let finger_draws = (!tool_draws(&tool) || !page_touch::pen_seen()) && tool != "comment";
            if count > 1 || !finger_draws {
                self.cancel_page_gesture();
                if !page_touch::panning() {
                    page_touch::start_pan();
                }
                event.prevent_default();
                return;
            }
        }
        if tool == "comment" {
            return;
        }
        self.ensure_page_text_targets();
        let started = {
            let mut guard = self.0.borrow_mut();
            let inner = &mut *guard;
            let doc = Driver::effective_document(inner);
            let editing = inner.editing_block;
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            let Some(page) = registry.page.as_mut() else {
                return;
            };
            if !page.contains_client_point(f64::from(event.client_x()), f64::from(event.client_y()))
            {
                return;
            }
            page.begin(
                event,
                &doc,
                inner.text_client_state.as_mut(),
                editing,
                tool_draws(&tool),
            )
        };
        if started {
            event.prevent_default();
            event.stop_propagation();
            self.queue_page_preview(true);
        } else if touch {
            page_touch::start_pan();
            event.prevent_default();
        }
    }

    fn place_comment(&self, x: f64, y: f64) {
        self.ensure_page_text_targets();
        let anchor = {
            let mut guard = self.0.borrow_mut();
            let inner = &mut *guard;
            let doc = Driver::effective_document(inner);
            let editing = inner.editing_block;
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            let Some(page) = registry.page.as_mut() else {
                return;
            };
            page.comment_anchor_at((x, y), &doc, inner.text_client_state.as_mut(), editing)
        };
        crate::comments::start_draft(anchor);
        self.layout_page();
    }

    fn cancel_page_gesture(&self) {
        let commit = {
            let mut guard = self.0.borrow_mut();
            let inner = &mut *guard;
            let doc = Driver::effective_document(inner);
            let editing = inner.editing_block;
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            let Some(page) = registry.page.as_mut() else {
                return;
            };
            if !page.cancel_gesture() {
                return;
            }
            page.take_commit(&doc, inner.text_client_state.as_mut(), editing)
        };
        self.queue_page_preview(true);
        if let Ok(commit) = commit {
            self.dispatch_page_update(commit.update);
        }
    }

    fn page_pointer_move(&self, event: &web_sys::PointerEvent) {
        let pointer_id = event.pointer_id();
        if event.pointer_type() == "touch" && page_touch::tracked(pointer_id) {
            let owned = self
                .1
                .try_borrow()
                .ok()
                .and_then(|registry| {
                    registry
                        .page
                        .as_ref()
                        .map(|page| page.owns_pointer(pointer_id))
                })
                .unwrap_or(false);
            if !owned {
                if page_touch::touch_move(
                    pointer_id,
                    f64::from(event.client_x()),
                    f64::from(event.client_y()),
                ) {
                    event.prevent_default();
                    event.stop_propagation();
                }
                return;
            }
            page_touch::touch_move(
                pointer_id,
                f64::from(event.client_x()),
                f64::from(event.client_y()),
            );
        }
        if event.pointer_type() != "touch" && event.buttons() == 0 {
            let stale = self
                .1
                .try_borrow()
                .ok()
                .and_then(|registry| {
                    registry
                        .page
                        .as_ref()
                        .map(|page| page.owns_pointer(pointer_id))
                })
                .unwrap_or(false);
            if stale {
                self.page_pointer_end(event, false);
                return;
            }
        }
        let owned = {
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            match registry.page.as_mut() {
                Some(page) if page.owns_pointer(pointer_id) => {
                    page.forward_move(event);
                    true
                }
                _ => false,
            }
        };
        if owned {
            event.prevent_default();
            event.stop_propagation();
            self.queue_page_preview(false);
        }
    }

    fn page_pointer_end(&self, event: &web_sys::PointerEvent, cancelled: bool) {
        if event.pointer_type() == "touch" && page_touch::tracked(event.pointer_id()) {
            page_touch::touch_up(event.pointer_id());
        }
        let commit = {
            let mut guard = self.0.borrow_mut();
            let inner = &mut *guard;
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            let Some(page) = registry.page.as_mut() else {
                return;
            };
            if !page.end(event, cancelled) {
                return;
            }
            let doc = Driver::effective_document(inner);
            let editing = inner.editing_block;
            (
                page.doc_id(),
                page.take_commit(&doc, inner.text_client_state.as_mut(), editing),
            )
        };
        event.prevent_default();
        event.stop_propagation();
        SUPPRESSED_CLICK.with(|cell| {
            cell.set((
                js_sys::Date::now() + 400.0,
                f64::from(event.client_x()),
                f64::from(event.client_y()),
            ))
        });
        self.queue_page_preview(true);
        let (doc_id, commit) = commit;
        if doc_id != self.0.borrow().selected_doc_id {
            return;
        }
        let commit = match commit {
            Ok(commit) => commit,
            Err(error) => {
                self.report_ink_error("Failed to save page ink", error);
                return;
            }
        };
        let changed = commit.update.is_some();
        if changed {
            self.record_ink_step();
        }
        let close_panel = std::mem::take(&mut self.0.borrow_mut().ink_panel_open);
        self.dispatch_page_update(commit.update);
        if let Some((group, anchor)) = commit.anchor {
            self.dispatch_page_op(DocumentOp::SetInkAnchor { group, anchor });
        }
        let tool = self.0.borrow().ink_tool.clone();
        if changed && !tool_draws(&tool) {
            self.remove_orphaned_anchors();
        }
        if close_panel || (!changed && !tool_draws(&tool)) {
            self.render();
        }
    }

    fn queue_page_preview(&self, immediate: bool) {
        let preview = {
            let Ok(mut registry) = self.1.try_borrow_mut() else {
                return;
            };
            registry.page.as_mut().and_then(|page| {
                page.take_preview()
                    .map(|bytes| (page.doc_id(), page.stream_epoch(), bytes))
            })
        };
        let Some((doc_id, stream_epoch, stonepen_preview)) = preview else {
            return;
        };
        if doc_id != self.0.borrow().selected_doc_id {
            return;
        }
        let state = PresenceKind::Ink {
            payload: InkPresencePayload {
                version: INK_PRESENCE_VERSION,
                block_id: PAGE_PRESENCE_BLOCK,
                stream_epoch,
                stonepen_preview,
            },
        };
        if immediate {
            self.live_broadcast(&state, true);
            self.0.borrow_mut().pending_presence = Some(PresencePayload {
                version: PRESENCE_VERSION,
                sent_at_ms: js_sys::Date::now() as i64,
                state,
            });
            self.send_pending_presence();
        } else {
            self.queue_presence(state);
        }
    }

    /// Ctrl/⌘+Z, Ctrl/⌘+Shift+Z and Ctrl+Y on the page.
    fn history_keydown(&self, event: &web_sys::KeyboardEvent) -> bool {
        if !(event.ctrl_key() || event.meta_key()) || event.alt_key() || !self.page_editable() {
            return false;
        }
        let key = event.key().to_lowercase();
        let redo = match key.as_str() {
            "z" => event.shift_key(),
            "y" if !event.shift_key() => true,
            _ => return false,
        };
        // The title and other fields keep their own undo.
        let on_page = event_element(event).is_none_or(|element| {
            element.class_list().contains("page-source")
                || !(matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                    || element.has_attribute("contenteditable"))
        });
        if !on_page || closest_dialog(event) {
            return false;
        }
        if redo {
            self.redo_page();
        } else {
            self.undo_page();
        }
        true
    }

    fn page_keydown(&self, event: &web_sys::KeyboardEvent) {
        if let Some(textarea) = event_element(event)
            .filter(|element| element.class_list().contains("page-source"))
            .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        {
            if self.page_editable() && self.source_keydown(event, &textarea) {
                event.prevent_default();
                event.stop_propagation();
                return;
            }
        }
        if self.history_keydown(event) {
            event.prevent_default();
            event.stop_propagation();
            return;
        }
        if event_element(event)
            .is_some_and(|element| element.class_list().contains("document-title-input"))
            && self.page_editable()
            && self.title_keydown(event)
        {
            event.prevent_default();
            event.stop_propagation();
            return;
        }
        let key = event.key();
        let in_text_control = event_element(event).is_some_and(|element| {
            matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                || element.has_attribute("contenteditable")
        });
        if key == "Escape" {
            if in_text_control && self.0.borrow().editing_block.is_some() {
                event.prevent_default();
                self.stop_editing();
                return;
            }
            let cleared = self
                .1
                .try_borrow_mut()
                .ok()
                .and_then(|mut registry| {
                    registry.page.as_mut().map(|page| {
                        let had = page.has_selection();
                        page.clear_selection();
                        had
                    })
                })
                .unwrap_or(false);
            if cleared {
                self.render();
            }
            return;
        }
        if in_text_control || !matches!(key.as_str(), "Delete" | "Backspace") {
            return;
        }
        let has_selection = self
            .1
            .try_borrow()
            .ok()
            .and_then(|registry| registry.page.as_ref().map(PageSurface::has_selection))
            .unwrap_or(false);
        if has_selection && self.page_editable() {
            event.prevent_default();
            let result = self
                .1
                .borrow_mut()
                .page
                .as_mut()
                .map(PageSurface::delete_selection);
            match result {
                Some(Ok(update)) => {
                    self.dispatch_page_update(update);
                    self.remove_orphaned_anchors();
                    self.render();
                }
                Some(Err(error)) => self.report_ink_error("Failed to delete ink", error),
                None => {}
            }
        }
    }
}

/// Installs the page listeners. The click listener must be registered before
/// the root's main click handler so page clicks are seen first.
pub(crate) fn install_page_listeners(
    driver: &Driver,
    window: &web_sys::Window,
    root: &web_sys::Element,
) -> Result<(), JsValue> {
    let d = driver.clone();
    let on_click = Closure::wrap(Box::new(move |event: web_sys::MouseEvent| {
        if RENDERING.with(|rendering| rendering.get()) {
            return;
        }
        if d.page_click(&event) {
            event.stop_immediate_propagation();
        }
    }) as Box<dyn FnMut(web_sys::MouseEvent)>);
    root.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())?;
    on_click.forget();

    for (name, kind) in [
        ("pointerdown", 0u8),
        ("pointermove", 1),
        ("pointerup", 2),
        ("pointercancel", 3),
    ] {
        let d = driver.clone();
        let handler = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| match kind {
            0 => {
                if !d.drag_pointer_down(&event) {
                    d.page_pointer_down(&event);
                }
            }
            1 => {
                if !d.drag_pointer_move(&event) {
                    d.page_pointer_move(&event);
                }
            }
            2 => {
                if !d.drag_pointer_end(&event, false) {
                    d.page_pointer_end(&event, false);
                }
            }
            _ => {
                if !d.drag_pointer_end(&event, true) {
                    d.page_pointer_end(&event, true);
                }
            }
        }) as Box<dyn FnMut(web_sys::PointerEvent)>);
        window.add_event_listener_with_callback_and_bool(
            name,
            handler.as_ref().unchecked_ref(),
            true,
        )?;
        handler.forget();
    }

    let d = driver.clone();
    let on_scroll = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        d.update_page_view();
    }) as Box<dyn FnMut(web_sys::Event)>);
    window.add_event_listener_with_callback_and_bool(
        "scroll",
        on_scroll.as_ref().unchecked_ref(),
        true,
    )?;
    on_scroll.forget();

    let d = driver.clone();
    let on_resize = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        d.layout_page();
    }) as Box<dyn FnMut(web_sys::Event)>);
    window.add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref())?;
    on_resize.forget();

    // Images and other media inside the page change its geometry.
    let d = driver.clone();
    let on_load = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        d.layout_page();
    }) as Box<dyn FnMut(web_sys::Event)>);
    root.add_event_listener_with_callback_and_bool("load", on_load.as_ref().unchecked_ref(), true)?;
    on_load.forget();

    let d = driver.clone();
    let on_input = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let Some(target) = event_element(&event) else {
            return;
        };
        if target.class_list().contains("ink-size-slider") {
            if let Ok(slider) = target.dyn_into::<web_sys::HtmlInputElement>() {
                d.preview_brush_width(slider.value_as_number() as f32);
            }
            return;
        }
        if target.class_list().contains("page-source") {
            if let Ok(textarea) = target.clone().dyn_into::<web_sys::HtmlElement>() {
                autosize(&textarea);
            }
            if let Ok(textarea) = target.dyn_into::<web_sys::HtmlTextAreaElement>() {
                d.update_slash_menu(&textarea);
            }
            d.layout_page();
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    root.add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())?;
    on_input.forget();

    // Moving the caret can leave or re-enter a `/command`.
    let d = driver.clone();
    let on_keyup = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
        if !matches!(
            event.key().as_str(),
            "ArrowLeft" | "ArrowRight" | "Home" | "End" | "Backspace" | "Delete"
        ) {
            return;
        }
        if let Some(textarea) = event_element(&event)
            .filter(|target| target.class_list().contains("page-source"))
            .and_then(|target| target.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        {
            d.update_slash_menu(&textarea);
        }
    }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
    root.add_event_listener_with_callback("keyup", on_keyup.as_ref().unchecked_ref())?;
    on_keyup.forget();

    let d = driver.clone();
    let on_focusout = Closure::wrap(Box::new(move |event: web_sys::FocusEvent| {
        if event_element(&event).is_some_and(|target| target.class_list().contains("page-source")) {
            d.close_slash_menu();
        }
    }) as Box<dyn FnMut(web_sys::FocusEvent)>);
    root.add_event_listener_with_callback("focusout", on_focusout.as_ref().unchecked_ref())?;
    on_focusout.forget();

    // Whatever source the user is typing into stays open as source.
    let d = driver.clone();
    let on_focusin = Closure::wrap(Box::new(move |event: web_sys::FocusEvent| {
        if let Some(block_id) = event_element(&event)
            .filter(|target| target.class_list().contains("page-source"))
            .and_then(|target| parse_block_id(&target, "data-block-id"))
        {
            d.0.borrow_mut().editing_block = Some(block_id);
        }
    }) as Box<dyn FnMut(web_sys::FocusEvent)>);
    root.add_event_listener_with_callback("focusin", on_focusin.as_ref().unchecked_ref())?;
    on_focusin.forget();

    let d = driver.clone();
    let on_keydown = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
        d.page_keydown(&event);
    }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
    window.add_event_listener_with_callback_and_bool(
        "keydown",
        on_keydown.as_ref().unchecked_ref(),
        true,
    )?;
    on_keydown.forget();
    Ok(())
}
