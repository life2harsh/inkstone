use std::cell::Cell;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::ui::{driver, put, state};

pub const MIN: f64 = 0.5;
pub const MAX: f64 = 3.0;
const STEPS: [f64; 11] = [0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 2.0, 2.5, 3.0];
const KEY: &str = "inkstone-zoom";

thread_local! {
    static ZOOM: Cell<f64> = const { Cell::new(1.0) };
    static QUEUED: Cell<Option<(f64, Option<(f64, f64)>)>> = const { Cell::new(None) };
    static TWEEN: Cell<u32> = const { Cell::new(0) };
    static GOAL: Cell<Option<f64>> = const { Cell::new(None) };
    static SETTLE: Cell<u32> = const { Cell::new(0) };
    static PINCH: Cell<Option<(f64, f64, (f64, f64))>> = const { Cell::new(None) };
}

pub fn get() -> f64 {
    ZOOM.with(Cell::get)
}

pub fn local(px: f64) -> f64 {
    px / get()
}

pub fn percent(zoom: f64) -> String {
    format!("{}%", (zoom * 100.0).round())
}

fn query(selector: &str) -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .query_selector(selector)
        .ok()
        .flatten()
}

fn after(ms: i32, run: impl FnOnce() + 'static) {
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            Closure::once_into_js(run).unchecked_ref(),
            ms,
        );
    }
}

fn frame(run: impl FnOnce(f64) + 'static) {
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(Closure::once_into_js(run).unchecked_ref());
    }
}

fn root() -> Option<web_sys::HtmlElement> {
    web_sys::window()?
        .document()?
        .document_element()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
}

pub(crate) fn measure_view() {
    let (Some(root), Some(main), Some(window)) =
        (root(), query(".main-content"), web_sys::window())
    else {
        return;
    };
    let rect = main.get_bounding_client_rect();
    let height = window
        .inner_height()
        .ok()
        .and_then(|height| height.as_f64())
        .unwrap_or(rect.bottom());
    let style = root.style();
    let _ = style.set_property(
        "--view-cx",
        &format!("{}px", rect.left() + rect.width() / 2.0),
    );
    let _ = style.set_property(
        "--view-bottom",
        &format!("{}px", (height - rect.bottom()).max(0.0)),
    );
}

fn paint(zoom: f64) {
    let Some(root) = root() else {
        return;
    };
    measure_view();
    let _ = root.style().set_property("--zoom", &format!("{zoom}"));
    let _ = if (zoom - 1.0).abs() < 1e-3 {
        root.remove_attribute("data-zoomed")
    } else {
        root.set_attribute("data-zoomed", "")
    };
}

fn set(zoom: f64, focal: Option<(f64, f64)>) {
    let zoom = zoom.clamp(MIN, MAX);
    let old = get();
    if (zoom - old).abs() < 1e-4 {
        return;
    }
    let scroller = query(".main-content");
    let container = query(".editor-container");
    let before = container.as_ref().map(|container| {
        let rect = container.get_bounding_client_rect();
        let focal = focal.unwrap_or_else(|| {
            let view = scroller
                .as_ref()
                .map(|scroller| scroller.get_bounding_client_rect());
            view.map_or((0.0, 0.0), |view| {
                (
                    view.left() + view.width() / 2.0,
                    view.top() + view.height() / 2.0,
                )
            })
        });
        (rect.left(), rect.top(), focal)
    });
    ZOOM.with(|cell| cell.set(zoom));
    paint(zoom);
    if let (Some(scroller), Some(container), Some((left, top, focal))) =
        (scroller, container, before)
    {
        let rect = container.get_bounding_client_rect();
        let ratio = zoom / old;
        scroller.scroll_by_with_x_and_y(
            rect.left() + (focal.0 - left) * ratio - focal.0,
            rect.top() + (focal.1 - top) * ratio - focal.1,
        );
    }
    driver().layout_page();
    put(state().zoom, zoom);
    let token = SETTLE.with(|settle| {
        settle.set(settle.get().wrapping_add(1));
        settle.get()
    });
    after(320, move || {
        if SETTLE.with(Cell::get) != token {
            return;
        }
        if let Some(storage) =
            web_sys::window().and_then(|window| window.local_storage().ok().flatten())
        {
            let _ = storage.set_item(KEY, &get().to_string());
        }
        crate::pdf_pages::sharpen();
    });
}

fn queue(zoom: f64, focal: Option<(f64, f64)>) {
    let idle = QUEUED
        .with(|queued| queued.replace(Some((zoom, focal))))
        .is_none();
    if idle {
        frame(|_| {
            if let Some((zoom, focal)) = QUEUED.with(Cell::take) {
                set(zoom, focal);
            }
        });
    }
}

fn pending() -> f64 {
    QUEUED.with(Cell::get).map_or_else(get, |(zoom, _)| zoom)
}

fn stop_tween() -> u32 {
    GOAL.with(|goal| goal.set(None));
    TWEEN.with(|tween| {
        tween.set(tween.get().wrapping_add(1));
        tween.get()
    })
}

pub fn animate_to(target: f64, focal: Option<(f64, f64)>) {
    let token = stop_tween();
    let target = target.clamp(MIN, MAX);
    GOAL.with(|goal| goal.set(Some(target)));
    let (from, to) = (get().ln(), target.ln());
    let start = js_sys::Date::now();
    fn step(token: u32, from: f64, to: f64, start: f64, focal: Option<(f64, f64)>) {
        frame(move |_| {
            if TWEEN.with(Cell::get) != token {
                return;
            }
            let t = ((js_sys::Date::now() - start) / 200.0).min(1.0);
            let eased = 1.0 - (1.0 - t).powi(3);
            set((from + (to - from) * eased).exp(), focal);
            if t < 1.0 {
                step(token, from, to, start, focal);
            } else {
                GOAL.with(|goal| goal.set(None));
            }
        });
    }
    step(token, from, to, start, focal);
}

pub fn step(direction: i32) {
    let zoom = GOAL.with(Cell::get).unwrap_or_else(get);
    let next = if direction > 0 {
        STEPS.iter().copied().find(|step| *step > zoom + 0.01)
    } else {
        STEPS.iter().rev().copied().find(|step| *step < zoom - 0.01)
    };
    if let Some(next) = next {
        animate_to(next, None);
    }
}

pub fn reset() {
    animate_to(1.0, None);
}

fn in_document(target: Option<web_sys::EventTarget>) -> bool {
    target
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| target.closest(".main-content").ok().flatten())
        .is_some()
        && query(".editor-container .page-surface").is_some()
}

fn on_wheel(event: web_sys::WheelEvent) {
    if !(event.ctrl_key() || event.meta_key()) || !in_document(event.target()) {
        return;
    }
    event.prevent_default();
    stop_tween();
    let lines = if event.delta_mode() == 1 { 16.0 } else { 1.0 };
    let delta = (event.delta_y() * lines).clamp(-60.0, 60.0);
    queue(
        pending() * (-delta * 0.01).exp(),
        Some((f64::from(event.client_x()), f64::from(event.client_y()))),
    );
}

fn fingers(event: &web_sys::TouchEvent) -> Option<(f64, (f64, f64))> {
    let touches = event.touches();
    if touches.length() != 2 {
        return None;
    }
    let (a, b) = (touches.get(0)?, touches.get(1)?);
    let (ax, ay, bx, by) = (
        f64::from(a.client_x()),
        f64::from(a.client_y()),
        f64::from(b.client_x()),
        f64::from(b.client_y()),
    );
    Some((
        (ax - bx).hypot(ay - by).max(1.0),
        ((ax + bx) / 2.0, (ay + by) / 2.0),
    ))
}

fn on_touch(event: web_sys::TouchEvent) {
    let Some((spread, mid)) = fingers(&event) else {
        if PINCH.with(Cell::take).is_some() && (pending() - 1.0).abs() < 0.06 {
            animate_to(1.0, None);
        }
        return;
    };
    if !in_document(event.target()) {
        return;
    }
    let Some((start, zoom, last)) = PINCH.with(Cell::get) else {
        stop_tween();
        PINCH.with(|pinch| pinch.set(Some((spread, get(), mid))));
        return;
    };
    let inking = driver().0.try_borrow().is_ok_and(|inner| inner.ink_mode);
    if event.cancelable() {
        event.prevent_default();
        if !inking && let Some(scroller) = query(".main-content") {
            scroller.scroll_by_with_x_and_y(last.0 - mid.0, last.1 - mid.1);
        }
    }
    PINCH.with(|pinch| pinch.set(Some((start, zoom, mid))));
    queue(zoom * spread / start, Some(mid));
}

pub(crate) fn install(window: &web_sys::Window) -> Result<(), JsValue> {
    let stored = window
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(KEY).ok().flatten())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|zoom| zoom.is_finite())
        .map_or(1.0, |zoom| zoom.clamp(MIN, MAX));
    ZOOM.with(|cell| cell.set(stored));
    paint(stored);
    put(state().zoom, stored);
    let options = web_sys::AddEventListenerOptions::new();
    options.set_passive(false);
    let wheel = Closure::<dyn FnMut(web_sys::WheelEvent)>::new(on_wheel);
    window.add_event_listener_with_callback_and_add_event_listener_options(
        "wheel",
        wheel.as_ref().unchecked_ref(),
        &options,
    )?;
    wheel.forget();
    for kind in ["touchstart", "touchmove", "touchend", "touchcancel"] {
        let touch = Closure::<dyn FnMut(web_sys::TouchEvent)>::new(on_touch);
        window.add_event_listener_with_callback_and_add_event_listener_options(
            kind,
            touch.as_ref().unchecked_ref(),
            &options,
        )?;
        touch.forget();
    }
    Ok(())
}

pub(crate) fn suspend() -> f64 {
    let zoom = get();
    stop_tween();
    set(1.0, None);
    zoom
}

pub(crate) fn resume(zoom: f64) {
    set(zoom, None);
}
