use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

#[derive(Default)]
struct TouchState {
    pen_seen: bool,
    panned: bool,
    points: HashMap<i32, (f64, f64)>,
    last_mid: Option<(f64, f64)>,
    last_time: f64,
    velocity: (f64, f64),
    momentum: u32,
}

thread_local! {
    static TOUCH: RefCell<TouchState> = RefCell::new(TouchState::default());
}

fn midpoint(points: &HashMap<i32, (f64, f64)>) -> Option<(f64, f64)> {
    if points.is_empty() {
        return None;
    }
    let count = points.len() as f64;
    let (x, y) = points
        .values()
        .fold((0.0, 0.0), |acc, point| (acc.0 + point.0, acc.1 + point.1));
    Some((x / count, y / count))
}

fn scroller() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .query_selector(".main-content")
        .ok()
        .flatten()
}

pub(crate) fn note_pen() {
    TOUCH.with(|touch| touch.borrow_mut().pen_seen = true);
}

pub(crate) fn pen_seen() -> bool {
    TOUCH.with(|touch| touch.borrow().pen_seen)
}

pub(crate) fn touch_down(id: i32, x: f64, y: f64) -> usize {
    TOUCH.with(|touch| {
        let mut touch = touch.borrow_mut();
        touch.momentum = touch.momentum.wrapping_add(1);
        touch.points.insert(id, (x, y));
        if touch.last_mid.is_some() {
            touch.last_mid = midpoint(&touch.points);
        }
        touch.points.len()
    })
}

pub(crate) fn needs_scroll_hint() -> bool {
    TOUCH.with(|touch| {
        let touch = touch.borrow();
        !touch.pen_seen && !touch.panned
    })
}

pub(crate) fn start_pan() {
    TOUCH.with(|touch| {
        let mut touch = touch.borrow_mut();
        touch.panned = touch.points.len() > 1 || touch.panned;
        touch.last_mid = midpoint(&touch.points);
        touch.last_time = js_sys::Date::now();
        touch.velocity = (0.0, 0.0);
    });
}

pub(crate) fn panning() -> bool {
    TOUCH.with(|touch| touch.borrow().last_mid.is_some())
}

pub(crate) fn tracked(id: i32) -> bool {
    TOUCH.with(|touch| touch.borrow().points.contains_key(&id))
}

pub(crate) fn touch_move(id: i32, x: f64, y: f64) -> bool {
    let delta = TOUCH.with(|touch| {
        let mut touch = touch.borrow_mut();
        if !touch.points.contains_key(&id) {
            return None;
        }
        touch.points.insert(id, (x, y));
        let last = touch.last_mid?;
        let mid = midpoint(&touch.points)?;
        let now = js_sys::Date::now();
        let elapsed = (now - touch.last_time).max(1.0);
        let delta = (mid.0 - last.0, mid.1 - last.1);
        let blend = (elapsed / 48.0).min(1.0);
        touch.velocity = (
            touch.velocity.0 * (1.0 - blend) + delta.0 / elapsed * blend,
            touch.velocity.1 * (1.0 - blend) + delta.1 / elapsed * blend,
        );
        touch.last_mid = Some(mid);
        touch.last_time = now;
        Some(delta)
    });
    let Some(delta) = delta else {
        return false;
    };
    if let Some(scroller) = scroller() {
        scroller.scroll_by_with_x_and_y(-delta.0, -delta.1);
    }
    true
}

pub(crate) fn touch_up(id: i32) {
    let fling = TOUCH.with(|touch| {
        let mut touch = touch.borrow_mut();
        touch.points.remove(&id);
        if touch.last_mid.is_none() {
            return None;
        }
        if !touch.points.is_empty() {
            touch.last_mid = midpoint(&touch.points);
            return None;
        }
        touch.last_mid = None;
        let idle = js_sys::Date::now() - touch.last_time;
        let velocity = touch.velocity;
        if idle > 80.0 || velocity.0.hypot(velocity.1) < 0.1 {
            return None;
        }
        touch.momentum = touch.momentum.wrapping_add(1);
        Some((touch.momentum, velocity))
    });
    if let Some((token, velocity)) = fling {
        step_momentum(token, velocity, None);
    }
}

fn step_momentum(token: u32, velocity: (f64, f64), last: Option<f64>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once_into_js(move |now: f64| {
        if TOUCH.with(|touch| touch.borrow().momentum) != token {
            return;
        }
        let elapsed = last.map_or(16.0, |last| (now - last).clamp(1.0, 64.0));
        if let Some(scroller) = scroller() {
            scroller.scroll_by_with_x_and_y(-velocity.0 * elapsed, -velocity.1 * elapsed);
        }
        let decay = 0.95_f64.powf(elapsed / 16.0);
        let next = (velocity.0 * decay, velocity.1 * decay);
        if next.0.hypot(next.1) > 0.02 {
            step_momentum(token, next, Some(now));
        }
    });
    let _ = window.request_animation_frame(callback.unchecked_ref());
}
