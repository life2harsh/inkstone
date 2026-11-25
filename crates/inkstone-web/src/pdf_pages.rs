use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use inkstone_workspace::{WorkspacePath, WorkspaceStore};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::Driver;

const PDFJS: &str = "/vendor/pdfjs-4.10.38";
const PAGE_WIDTH: f64 = 720.0;
const MAX_PAGES: u32 = 500;

#[derive(Default)]
struct PdfState {
    size: usize,
    loading: bool,
    error: Option<String>,
    pages: Vec<PdfPage>,
}

struct PdfPage {
    width: f64,
    height: f64,
    url: Option<String>,
    density: f64,
}

thread_local! {
    static CACHE: RefCell<HashMap<String, Rc<RefCell<PdfState>>>> = RefCell::new(HashMap::new());
}

fn density() -> f64 {
    let ratio = web_sys::window().map_or(1.0, |window| window.device_pixel_ratio());
    (ratio * crate::zoom::get()).clamp(1.5, 3.0)
}

fn blurry(state: &PdfState, target: f64) -> bool {
    state.pages.iter().any(|page| page.density < target * 0.95)
}

fn containers(path: &str) -> Vec<web_sys::Element> {
    let Some(found) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector_all(".pdf-pages[data-pdf-key]").ok())
    else {
        return Vec::new();
    };
    (0..found.length())
        .filter_map(|index| found.item(index)?.dyn_into::<web_sys::Element>().ok())
        .filter(|container| container.get_attribute("data-pdf-key").as_deref() == Some(path))
        .collect()
}

fn next_page(path: &str, state: &PdfState, target: f64) -> Option<usize> {
    let slots = containers(path)
        .first()
        .and_then(|container| container.query_selector_all(".pdf-page").ok());
    let height = web_sys::window()
        .and_then(|window| window.inner_height().ok()?.as_f64())
        .unwrap_or(0.0);
    let distance = |index: usize| {
        slots
            .as_ref()
            .and_then(|slots| {
                slots
                    .item(index as u32)?
                    .dyn_into::<web_sys::Element>()
                    .ok()
            })
            .map_or(f64::MAX, |slot| {
                let rect = slot.get_bounding_client_rect();
                (-rect.bottom()).max(rect.top() - height).max(0.0)
            })
    };
    (0..state.pages.len())
        .filter(|index| state.pages[*index].density < target * 0.95)
        .min_by(|a, b| distance(*a).total_cmp(&distance(*b)).then(a.cmp(b)))
}

pub fn sharpen() {
    let target = density();
    let blurry = CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .filter(|(_, state)| {
                let state = state.borrow();
                !state.loading && state.error.is_none() && blurry(&state, target)
            })
            .map(|(path, state)| (path.clone(), state.clone()))
            .collect::<Vec<_>>()
    });
    for (path, state) in blurry {
        if containers(&path).is_empty() {
            continue;
        }
        crate::ui::driver().render_pdf(path, state);
    }
}

pub fn is_pdf(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("pdf"))
}

fn get(value: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(value, &key.into()).unwrap_or(JsValue::UNDEFINED)
}

fn set(value: &JsValue, key: &str, field: &JsValue) {
    let _ = js_sys::Reflect::set(value, &key.into(), field);
}

fn call(value: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, String> {
    let function = get(value, method)
        .dyn_into::<js_sys::Function>()
        .map_err(|_| format!("{method} is unavailable"))?;
    let list = js_sys::Array::new();
    for arg in args {
        list.push(arg);
    }
    function
        .apply(value, &list)
        .map_err(|error| format!("{method} failed: {error:?}"))
}

async fn wait(promise: JsValue) -> Result<JsValue, String> {
    let promise = promise
        .dyn_into::<js_sys::Promise>()
        .map_err(|_| "expected a promise".to_string())?;
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|error| {
            let message = get(&error, "message");
            message.as_string().unwrap_or_else(|| format!("{error:?}"))
        })
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let value: JsValue = js_sys::Object::new().into();
    for (key, field) in fields {
        set(&value, key, field);
    }
    value
}

async fn library() -> Result<JsValue, String> {
    let window = web_sys::window().ok_or("no window")?;
    let loader = get(&window, "inkstonePdf");
    let module = wait(call(&window, "inkstonePdf", &[]).map_err(|_| {
        if loader.is_undefined() {
            "The PDF viewer is unavailable".to_string()
        } else {
            "The PDF viewer failed to load".to_string()
        }
    })?)
    .await?;
    let options = get(&module, "GlobalWorkerOptions");
    if get(&options, "workerSrc")
        .as_string()
        .unwrap_or_default()
        .is_empty()
    {
        set(
            &options,
            "workerSrc",
            &format!("{PDFJS}/pdf.worker.min.mjs").into(),
        );
    }
    Ok(module)
}

async fn canvas_blob_url(canvas: &web_sys::HtmlCanvasElement) -> Result<String, String> {
    let canvas = canvas.clone();
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let callback = Closure::once_into_js(move |blob: JsValue| {
            let _ = resolve.call1(&JsValue::NULL, &blob);
        });
        let _ = canvas.to_blob_with_type_and_encoder_options(
            callback.unchecked_ref(),
            "image/webp",
            &JsValue::from_f64(0.9),
        );
    });
    let blob = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|_| "Could not keep the rendered page".to_string())?
        .dyn_into::<web_sys::Blob>()
        .map_err(|_| "Could not keep the rendered page".to_string())?;
    web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|_| "Could not keep the rendered page".to_string())
}

fn paint(container: &web_sys::Element, state: &PdfState) {
    let Some(document) = container.owner_document() else {
        return;
    };
    let _ = container.set_attribute("data-pdf-pages", &state.pages.len().to_string());
    if let Some(error) = &state.error {
        if container
            .query_selector(".pdf-status")
            .ok()
            .flatten()
            .is_none()
        {
            container.set_inner_html("");
            let status = document.create_element("div").unwrap();
            status.set_class_name("pdf-status pdf-error");
            status.set_text_content(Some(&format!("This PDF can't be shown: {error}")));
            let _ = container.append_child(&status);
        }
        return;
    }
    if state.pages.is_empty() {
        if container
            .query_selector(".pdf-status")
            .ok()
            .flatten()
            .is_none()
        {
            let status = document.create_element("div").unwrap();
            status.set_class_name("pdf-status");
            status.set_text_content(Some("Opening PDF…"));
            let _ = container.append_child(&status);
        }
        return;
    }
    if container
        .query_selector_all(".pdf-page")
        .map_or(0, |list| list.length()) as usize
        != state.pages.len()
    {
        container.set_inner_html("");
        for (index, page) in state.pages.iter().enumerate() {
            let slot = document.create_element("div").unwrap();
            slot.set_class_name("pdf-page");
            let _ = slot.set_attribute("data-pdf-page", &(index + 1).to_string());
            let _ = slot.set_attribute(
                "style",
                &format!(
                    "aspect-ratio: {} / {}",
                    page.width.round().max(1.0),
                    page.height.round().max(1.0)
                ),
            );
            let _ = container.append_child(&slot);
        }
    }
    let Ok(slots) = container.query_selector_all(".pdf-page") else {
        return;
    };
    for (index, page) in state.pages.iter().enumerate() {
        let (Some(url), Some(slot)) = (
            page.url.as_ref(),
            slots
                .item(index as u32)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok()),
        ) else {
            continue;
        };
        if let Some(image) = slot.query_selector("img").ok().flatten() {
            if image.get_attribute("src").as_deref() != Some(url) {
                let _ = image.set_attribute("src", url);
            }
            continue;
        }
        let image = document.create_element("img").unwrap();
        let _ = image.set_attribute("src", url);
        let _ = image.set_attribute("alt", &format!("Page {}", index + 1));
        let _ = image.set_attribute("draggable", "false");
        let _ = slot.append_child(&image);
    }
    if container
        .query_selector(".pdf-dark-btn")
        .ok()
        .flatten()
        .is_none()
    {
        let button = document.create_element("button").unwrap();
        button.set_class_name("pdf-dark-btn");
        let _ = button.set_attribute("type", "button");
        let _ = button.set_attribute("aria-label", "Dark PDF pages");
        let _ = button.set_attribute("title", "Dark PDF pages");
        crate::icons::icon_button_content(&document, &button, "moon", "Dark PDF pages");
        let _ = container.insert_before(&button, container.first_child().as_ref());
    }
}

pub(crate) fn toggle_dark() {
    let dark = crate::prefs::get("pdf") == "dark";
    crate::prefs::set("pdf", if dark { "light" } else { "dark" });
}

impl Driver {
    pub(crate) fn mount_pdf(&self, image: &web_sys::Element, path: &str) {
        let Some(document) = image.owner_document() else {
            return;
        };
        let container = document.create_element("div").unwrap();
        container.set_class_name("pdf-pages");
        let _ = container.set_attribute("data-asset-path", path);
        let _ = container.set_attribute("data-pdf-key", path);
        if let Some(label) = image.get_attribute("alt").filter(|label| !label.is_empty()) {
            let _ = container.set_attribute("aria-label", &label);
        }
        let _ = container.set_attribute("role", "document");
        if image.replace_with_with_node_1(&container).is_err() {
            return;
        }
        let cached = CACHE.with(|cache| cache.borrow().get(path).cloned());
        if let Some(state) = &cached {
            paint(&container, &state.borrow());
            let current = state.borrow();
            if current.loading || current.error.is_none() {
                let stale = !current.loading && blurry(&current, density());
                drop(current);
                if stale {
                    self.render_pdf(path.to_string(), state.clone());
                }
                return;
            }
        }
        let state = Rc::new(RefCell::new(PdfState::default()));
        CACHE.with(|cache| cache.borrow_mut().insert(path.to_string(), state.clone()));
        paint(&container, &state.borrow());
        self.render_pdf(path.to_string(), state);
    }

    fn render_pdf(&self, path: String, state: Rc<RefCell<PdfState>>) {
        state.borrow_mut().loading = true;
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = this.load_pdf(&path, &state).await;
            let mut current = state.borrow_mut();
            current.loading = false;
            if let Err(error) = result
                && current.pages.iter().all(|page| page.url.is_none())
            {
                current.error = Some(error);
            }
            drop(current);
            this.repaint_pdf(&path, &state);
        });
    }

    fn repaint_pdf(&self, path: &str, state: &Rc<RefCell<PdfState>>) {
        let found = containers(path);
        for container in &found {
            paint(container, &state.borrow());
        }
        if !found.is_empty() {
            self.layout_page();
        }
    }

    async fn load_pdf(&self, path: &str, state: &Rc<RefCell<PdfState>>) -> Result<(), String> {
        let workspace_db = self
            .0
            .borrow()
            .workspace_db
            .clone()
            .ok_or("the vault is not open")?;
        let asset = WorkspacePath::new(path).map_err(|error| error.to_string())?;
        let bytes = workspace_db
            .read_file(&asset)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("the file is missing")?;
        state.borrow_mut().size = bytes.len();
        let pdfjs = library().await?;
        let params = object(&[
            ("data", js_sys::Uint8Array::from(bytes.as_slice()).into()),
            ("isEvalSupported", false.into()),
            ("enableXfa", false.into()),
            (
                "standardFontDataUrl",
                format!("{PDFJS}/standard_fonts/").into(),
            ),
            ("cMapUrl", format!("{PDFJS}/cmaps/").into()),
            ("cMapPacked", true.into()),
        ]);
        let task = call(&pdfjs, "getDocument", &[params])?;
        let pdf = wait(get(&task, "promise")).await?;
        if state.borrow().pages.is_empty() {
            let count = (get(&pdf, "numPages").as_f64().unwrap_or(0.0) as u32).min(MAX_PAGES);
            let mut pages = Vec::with_capacity(count as usize);
            for number in 1..=count {
                let page = wait(call(&pdf, "getPage", &[JsValue::from(number)])?).await?;
                let viewport = call(&page, "getViewport", &[object(&[("scale", 1.0.into())])])?;
                pages.push(PdfPage {
                    width: get(&viewport, "width").as_f64().unwrap_or(612.0),
                    height: get(&viewport, "height").as_f64().unwrap_or(792.0),
                    url: None,
                    density: 0.0,
                });
            }
            state.borrow_mut().pages = pages;
            self.repaint_pdf(path, state);
        }
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or("no document")?;
        loop {
            let target = density();
            let Some(index) = next_page(path, &state.borrow(), target) else {
                break;
            };
            if containers(path).is_empty() && state.borrow().pages[index].url.is_some() {
                break;
            }
            let page = wait(call(&pdf, "getPage", &[JsValue::from(index as u32 + 1)])?).await?;
            let width = state.borrow().pages[index].width;
            let scale = PAGE_WIDTH * target / width.max(1.0);
            let viewport = call(&page, "getViewport", &[object(&[("scale", scale.into())])])?;
            let canvas = document
                .create_element("canvas")
                .map_err(|_| "no canvas")?
                .dyn_into::<web_sys::HtmlCanvasElement>()
                .map_err(|_| "no canvas")?;
            canvas.set_width(get(&viewport, "width").as_f64().unwrap_or(1.0).ceil() as u32);
            canvas.set_height(get(&viewport, "height").as_f64().unwrap_or(1.0).ceil() as u32);
            let context = canvas.get_context("2d").ok().flatten().ok_or("no canvas")?;
            let render = call(
                &page,
                "render",
                &[object(&[
                    ("canvasContext", context.into()),
                    ("viewport", viewport),
                    ("background", "#ffffff".into()),
                ])],
            )?;
            wait(get(&render, "promise")).await?;
            let url = canvas_blob_url(&canvas).await?;
            let _ = call(&page, "cleanup", &[]);
            let old = {
                let mut current = state.borrow_mut();
                let page = &mut current.pages[index];
                page.density = target;
                page.url.replace(url)
            };
            self.repaint_pdf(path, state);
            if let Some(old) = old {
                let _ = web_sys::Url::revoke_object_url(&old);
            }
        }
        let _ = call(&pdf, "destroy", &[]);
        Ok(())
    }
}
