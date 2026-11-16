use base64::Engine;
use inkstone_workspace::{WorkspacePath, WorkspaceStore};
use leptos::prelude::{GetUntracked, Set};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::Driver;

const PRINTABLE_WIDTH: f64 = 680.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Pdf,
    Html,
    Markdown,
    Vault,
}

impl ExportFormat {
    pub(crate) const ALL: [ExportFormat; 4] = [
        ExportFormat::Pdf,
        ExportFormat::Html,
        ExportFormat::Markdown,
        ExportFormat::Vault,
    ];

    pub(crate) fn key(self) -> &'static str {
        match self {
            ExportFormat::Pdf => "pdf",
            ExportFormat::Html => "html",
            ExportFormat::Markdown => "markdown",
            ExportFormat::Vault => "vault",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            ExportFormat::Pdf => "PDF",
            ExportFormat::Html => "Web page",
            ExportFormat::Markdown => "Markdown",
            ExportFormat::Vault => "Whole vault",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            ExportFormat::Pdf => "Print or save, ink and all",
            ExportFormat::Html => "One file that opens in any browser",
            ExportFormat::Markdown => "For Obsidian and others, ink as images",
            ExportFormat::Vault => "Every note and file as a ZIP",
        }
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn file_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.strip_suffix(".md").unwrap_or(name);
    if stem.trim().is_empty() {
        "Note".into()
    } else {
        stem.to_string()
    }
}

async fn next_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.request_animation_frame(&resolve);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

async fn fetch_bytes(url: &str) -> Option<(Vec<u8>, String)> {
    let window = web_sys::window()?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url))
        .await
        .ok()?
        .dyn_into::<web_sys::Response>()
        .ok()?;
    if !response.ok() {
        return None;
    }
    let media_type = response
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_default();
    let buffer = wasm_bindgen_futures::JsFuture::from(response.array_buffer().ok()?)
        .await
        .ok()?;
    Some((js_sys::Uint8Array::new(&buffer).to_vec(), media_type))
}

async fn fetch_text(url: &str) -> String {
    fetch_bytes(url)
        .await
        .and_then(|(bytes, _)| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn stylesheet_urls(document: &web_sys::Document) -> Vec<String> {
    let Ok(links) = document.query_selector_all("link[rel=\"stylesheet\"]") else {
        return Vec::new();
    };
    (0..links.length())
        .filter_map(|index| links.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
        .filter_map(|link| link.get_attribute("href"))
        .collect()
}

fn image_data_url(
    document: &web_sys::Document,
    image: &web_sys::HtmlImageElement,
) -> Option<String> {
    let (width, height) = (image.natural_width(), image.natural_height());
    if width == 0 || height == 0 {
        return None;
    }
    let canvas = document
        .create_element("canvas")
        .ok()?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .ok()?;
    canvas.set_width(width);
    canvas.set_height(height);
    let context = canvas
        .get_context("2d")
        .ok()??
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .ok()?;
    context
        .draw_image_with_html_image_element(image, 0.0, 0.0)
        .ok()?;
    canvas
        .to_data_url_with_type_and_encoder_options("image/jpeg", &JsValue::from_f64(0.9))
        .ok()
}

async fn read_asset(workspace_db: Option<&crate::WorkspaceDb>, asset: &str) -> Option<Vec<u8>> {
    let path = WorkspacePath::new(asset).ok()?;
    workspace_db?.read_file(&path).await.ok().flatten()
}

struct PageSnapshot {
    title: String,
    body: String,
    width: f64,
    zoom: f64,
}

impl Driver {
    pub(crate) fn close_export_menu(&self, target: Option<&web_sys::Element>) -> bool {
        let open = crate::ui::state().export_open;
        let outside =
            target.is_none_or(|target| target.closest(".export-wrap").ok().flatten().is_none());
        if !open.get_untracked() || !outside {
            return false;
        }
        open.set(false);
        if target.is_none() {
            crate::ui::focus_soon("export-menu");
        }
        true
    }

    pub(crate) fn export(&self, format: ExportFormat) {
        if format == ExportFormat::Vault {
            self.export_vault();
            return;
        }
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = match format {
                ExportFormat::Pdf => this.export_pdf().await,
                ExportFormat::Html => this.export_html().await,
                ExportFormat::Markdown => this.export_markdown().await,
                ExportFormat::Vault => Ok(()),
            };
            if let Err(error) = result {
                this.set_sync_error(format!("Export failed: {error}"));
            }
        });
    }

    async fn snapshot_page(&self) -> Result<PageSnapshot, String> {
        self.flush_ink_before_transition();
        self.stop_editing();
        next_frame().await;
        next_frame().await;
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or("The page is unavailable")?;
        let surface = document
            .query_selector(".page-surface")
            .ok()
            .flatten()
            .ok_or("Open a note to export it")?;
        let rect = surface.get_bounding_client_rect();
        let (width, height) = (
            rect.width(),
            surface.scroll_height().max(rect.height() as i32) as f64,
        );
        let ink = self.1.try_borrow().ok().and_then(|registry| {
            registry
                .page
                .as_ref()
                .map(|page| page.export_svg(width as f32, height as f32))
        });
        let clone = surface
            .clone_node_with_deep(true)
            .map_err(|_| "Could not copy the page")?
            .dyn_into::<web_sys::Element>()
            .map_err(|_| "Could not copy the page")?;
        clone.set_class_name("page-surface read-only export-surface");
        clone.remove_attribute("style").ok();
        if let Ok(junk) = clone.query_selector_all(
            ".page-gutter, .page-placeholder, .ink-dock-wrap, .ink-scroll-hint, button, textarea, input:not([type=\"checkbox\"]), .asset-download",
        ) {
            for index in 0..junk.length() {
                if let Some(node) = junk
                    .item(index)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                {
                    node.remove();
                }
            }
        }
        if let Ok(editable) = clone.query_selector_all("[contenteditable], [tabindex]") {
            for index in 0..editable.length() {
                if let Some(node) = editable
                    .item(index)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                {
                    let _ = node.remove_attribute("contenteditable");
                    let _ = node.remove_attribute("tabindex");
                }
            }
        }
        if let Ok(boxes) = clone.query_selector_all("input[type=\"checkbox\"]") {
            for index in 0..boxes.length() {
                if let Some(node) = boxes
                    .item(index)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                {
                    if node
                        .dyn_ref::<web_sys::HtmlInputElement>()
                        .is_some_and(|input| input.checked())
                    {
                        let _ = node.set_attribute("checked", "");
                    }
                }
            }
        }
        if let (Ok(live), Ok(copies)) = (
            surface.query_selector_all(".pdf-page img"),
            clone.query_selector_all(".pdf-page img"),
        ) {
            for index in 0..live.length().min(copies.length()) {
                let (Some(source), Some(copy)) = (
                    live.item(index)
                        .and_then(|node| node.dyn_into::<web_sys::HtmlImageElement>().ok()),
                    copies
                        .item(index)
                        .and_then(|node| node.dyn_into::<web_sys::Element>().ok()),
                ) else {
                    continue;
                };
                if let Some(data) = image_data_url(&document, &source) {
                    let _ = copy.set_attribute("src", &data);
                }
            }
        }
        let workspace_db = self.0.borrow().workspace_db.clone();
        if let Ok(images) = clone.query_selector_all("img[data-asset-path]") {
            for index in 0..images.length() {
                let Some(image) = images
                    .item(index)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                else {
                    continue;
                };
                let Some(asset) = image.get_attribute("data-asset-path") else {
                    continue;
                };
                let Some(bytes) = read_asset(workspace_db.as_ref(), &asset).await else {
                    continue;
                };
                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                let _ = image.set_attribute(
                    "src",
                    &format!("data:{};base64,{data}", crate::asset_media_type(&asset)),
                );
            }
        }
        let mut body = clone.outer_html();
        if let Some((svg, left, top, svg_width, svg_height)) = ink {
            if svg.contains("<path") || svg.contains("<image") {
                body.push_str(&format!(
                    "<div class=\"export-ink\" style=\"left:{left}px;top:{top}px;width:{svg_width}px;height:{svg_height}px\">{svg}</div>"
                ));
            }
        }
        let title = {
            let inner = self.0.borrow();
            let doc = Driver::effective_document(&inner);
            if doc.title.trim().is_empty() {
                inner
                    .selected_path
                    .as_deref()
                    .map(file_stem)
                    .unwrap_or_else(|| "Note".into())
            } else {
                doc.title.clone()
            }
        };
        Ok(PageSnapshot {
            title,
            body,
            width,
            zoom: (PRINTABLE_WIDTH / width).min(1.0),
        })
    }

    async fn page_html(&self, print: bool) -> Result<(String, String), String> {
        let snapshot = self.snapshot_page().await?;
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or("The page is unavailable")?;
        let mut styles = String::new();
        for url in stylesheet_urls(&document) {
            styles.push_str(&fetch_text(&url).await);
            styles.push('\n');
        }
        let styles = styles.replace("</style", "<\\/style");
        let title = escape_html(&snapshot.title);
        let print_script = if print {
            "<script>addEventListener('load',()=>setTimeout(()=>print(),150))</script>"
        } else {
            ""
        };
        let html = format!(
            r#"<!doctype html>
<html lang="en" data-theme="light">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="generator" content="Inkstone">
<title>{title}</title>
<style>
{styles}
html, body {{ height: auto; overflow: visible; background: #fff !important; color: #37352f; }}
body {{ display: block; margin: 0; }}
.export-page {{ width: {width}px; max-width: none; margin: 40px auto 64px; padding: 0 24px; box-sizing: content-box; }}
.export-page .document-title {{ margin-bottom: 18px; }}
.export-body {{ position: relative; }}
.export-surface {{ max-width: none; width: {width}px; }}
.export-surface .page-tail {{ min-height: 0; }}
.export-ink {{ position: absolute; pointer-events: none; }}
.export-ink svg {{ display: block; width: 100%; height: 100%; overflow: visible; }}
@page {{ margin: 14mm; }}
@media print {{
  .export-page {{ zoom: {zoom}; margin: 0; padding: 0; }}
  .page-block:not(:has(.pdf-pages)) {{ break-inside: avoid; }}
  .pdf-page {{ break-inside: avoid; }}
}}
</style>
</head>
<body>
<div class="export-page">
<h1 class="document-title">{title}</h1>
<div class="export-body">{body}</div>
</div>
{print_script}
</body>
</html>
"#,
            width = snapshot.width.round(),
            zoom = (snapshot.zoom * 1000.0).round() / 1000.0,
            body = snapshot.body,
        );
        Ok((snapshot.title, html))
    }

    fn export_name(&self, extension: &str) -> String {
        let stem = self
            .0
            .borrow()
            .selected_path
            .as_deref()
            .map(file_stem)
            .unwrap_or_else(|| "Note".into());
        let clean: String = stem
            .chars()
            .map(|character| {
                if matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                ) {
                    '-'
                } else {
                    character
                }
            })
            .collect();
        format!("{clean}.{extension}")
    }

    async fn export_html(&self) -> Result<(), String> {
        let (_, html) = self.page_html(false).await?;
        crate::download_bytes(
            &self.export_name("html"),
            html.as_bytes(),
            "text/html;charset=utf-8",
        )
    }

    async fn export_pdf(&self) -> Result<(), String> {
        let (_, html) = self.page_html(false).await?;
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or("The page is unavailable")?;
        let frame = document
            .create_element("iframe")
            .map_err(|_| "Could not prepare printing")?
            .dyn_into::<web_sys::HtmlIFrameElement>()
            .map_err(|_| "Could not prepare printing")?;
        frame.set_class_name("export-print-frame");
        frame.set_attribute("aria-hidden", "true").ok();
        frame.set_srcdoc(&html);
        let loaded = js_sys::Promise::new(&mut |resolve, _| {
            frame.set_onload(Some(resolve.unchecked_ref()));
        });
        document
            .body()
            .ok_or("The page is unavailable")?
            .append_child(&frame)
            .map_err(|_| "Could not prepare printing")?;
        let _ = wasm_bindgen_futures::JsFuture::from(loaded).await;
        sleep(150).await;
        let window = frame
            .content_window()
            .ok_or("Printing is unavailable in this browser")?;
        window.focus().ok();
        window
            .print()
            .map_err(|_| "Printing is unavailable in this browser".to_string())?;
        let cleanup = frame.clone();
        let remove = Closure::once_into_js(move || cleanup.remove());
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                remove.unchecked_ref(),
                60_000,
            );
        }
        Ok(())
    }

    async fn export_markdown(&self) -> Result<(), String> {
        self.flush_ink_before_transition();
        let (path, doc, workspace_db) = {
            let inner = self.0.borrow();
            (
                inner
                    .selected_path
                    .clone()
                    .ok_or("Open a note to export it")?,
                Driver::effective_document(&inner),
                inner.workspace_db.clone(),
            )
        };
        let stem = file_stem(&path);
        let dir = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
        let note = inkstone_core::export::portable_markdown(&doc, "_ink", &stem);
        let mut assets = Vec::new();
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            if let Ok(found) = document.query_selector_all(".page-surface [data-asset-path]") {
                for index in 0..found.length() {
                    let Some(element) = found
                        .item(index)
                        .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                    else {
                        continue;
                    };
                    let Some(asset) = element.get_attribute("data-asset-path") else {
                        continue;
                    };
                    if assets.iter().any(|(existing, _)| existing == &asset) {
                        continue;
                    }
                    let bytes = read_asset(workspace_db.as_ref(), &asset).await;
                    if let Some(bytes) = bytes {
                        assets.push((asset, bytes));
                    }
                }
            }
        }
        if note.images.is_empty() && assets.is_empty() {
            return crate::download_bytes(
                &self.export_name("md"),
                note.markdown.as_bytes(),
                "text/markdown;charset=utf-8",
            );
        }
        let in_dir = |name: &str| {
            if dir.is_empty() {
                name.to_string()
            } else {
                format!("{dir}/{name}")
            }
        };
        let mut files = vec![(path.clone(), note.markdown.into_bytes())];
        for image in note.images {
            files.push((
                in_dir(&format!("_ink/{}", image.file_name)),
                image.svg.into_bytes(),
            ));
        }
        files.extend(assets);
        let archive =
            inkstone_workspace::build_file_archive(&files).map_err(|error| error.to_string())?;
        crate::download_bytes(&self.export_name("zip"), &archive, "application/zip")
    }
}
