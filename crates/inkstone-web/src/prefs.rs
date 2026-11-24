//! Per-device look and feel: accent, fonts, text size, page width, paper
//! and layout. Each preference is stored in local storage and applied as a
//! `data-*` attribute on `<html>`, which the stylesheet turns into tokens.

pub struct Pref {
    pub key: &'static str,
    pub label: &'static str,
    pub default: &'static str,
    pub options: &'static [(&'static str, &'static str)],
    /// Shown as color swatches rather than words.
    pub swatches: bool,
}

pub const PREFS: &[Pref] = &[
    Pref {
        key: "layout",
        label: "Layout",
        default: "panels",
        options: &[("panels", "Panels"), ("flat", "Flat")],
        swatches: false,
    },
    Pref {
        key: "accent",
        label: "Accent",
        default: "indigo",
        options: &[
            ("indigo", "Indigo"),
            ("blue", "Blue"),
            ("teal", "Teal"),
            ("green", "Green"),
            ("amber", "Amber"),
            ("orange", "Orange"),
            ("rose", "Rose"),
            ("violet", "Violet"),
            ("graphite", "Graphite"),
        ],
        swatches: true,
    },
    Pref {
        key: "font",
        label: "Text font",
        default: "sans",
        options: &[
            ("sans", "Sans"),
            ("serif", "Serif"),
            ("rounded", "Rounded"),
            ("mono", "Mono"),
        ],
        swatches: false,
    },
    Pref {
        key: "size",
        label: "Text size",
        default: "m",
        options: &[
            ("s", "Small"),
            ("m", "Medium"),
            ("l", "Large"),
            ("xl", "Huge"),
        ],
        swatches: false,
    },
    Pref {
        key: "width",
        label: "Page width",
        default: "normal",
        options: &[
            ("narrow", "Narrow"),
            ("normal", "Normal"),
            ("wide", "Wide"),
            ("full", "Full"),
        ],
        swatches: false,
    },
    Pref {
        key: "paper",
        label: "Paper",
        default: "plain",
        options: &[
            ("plain", "Plain"),
            ("lined", "Lined"),
            ("grid", "Grid"),
            ("dots", "Dots"),
        ],
        swatches: false,
    },
];

fn storage_key(key: &str) -> String {
    format!("inkstone-pref-{key}")
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn find(key: &str) -> Option<&'static Pref> {
    PREFS.iter().find(|pref| pref.key == key)
}

/// The stored value, or the default when unset or no longer offered.
pub fn get(key: &str) -> &'static str {
    let Some(pref) = find(key) else {
        return "";
    };
    let stored = storage().and_then(|storage| storage.get_item(&storage_key(key)).ok().flatten());
    stored
        .and_then(|value| {
            pref.options
                .iter()
                .find(|(option, _)| *option == value)
                .map(|(option, _)| *option)
        })
        .unwrap_or(pref.default)
}

pub fn apply_all() {
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
    else {
        return;
    };
    for pref in PREFS {
        let _ = root.set_attribute(&format!("data-{}", pref.key), get(pref.key));
    }
}

/// Stores and applies `value` if `key` offers it. Returns whether it did.
pub fn set(key: &str, value: &str) -> bool {
    let Some(pref) = find(key) else {
        return false;
    };
    if !pref.options.iter().any(|(option, _)| *option == value) {
        return false;
    }
    if let Some(storage) = storage() {
        let _ = storage.set_item(&storage_key(key), value);
    }
    apply_all();
    true
}

/// Forgets every preference, back to the defaults.
pub fn reset() {
    if let Some(storage) = storage() {
        for pref in PREFS {
            let _ = storage.remove_item(&storage_key(pref.key));
        }
    }
    apply_all();
}

/// One labelled row of choices per preference.
pub fn controls(changed: impl Fn() + Copy + Send + Sync + 'static) -> impl leptos::IntoView {
    use leptos::prelude::*;
    let tick = RwSignal::new(0u32);
    let rows = PREFS.iter().map(|pref| {
        let choices = pref.options.iter().map(|(value, name)| {
            view! {
                <button
                    class=if pref.swatches { "pref-choice pref-swatch" } else { "pref-choice" }
                    data-pref-key=pref.key
                    data-pref-value=*value
                    aria-pressed=move || {
                        tick.track();
                        if get(pref.key) == *value { "true" } else { "false" }
                    }
                    aria-label=pref.swatches.then_some(*name)
                    title=pref.swatches.then_some(*name)
                    on:click=move |_| {
                        if set(pref.key, value) {
                            tick.update(|tick| *tick += 1);
                            changed();
                        }
                    }
                >
                    {(!pref.swatches).then_some(*name)}
                </button>
            }
        });
        view! {
            <div class="prefs-row">
                <span class="prefs-label">{pref.label}</span>
                <div
                    class=if pref.swatches { "prefs-swatches" } else { "prefs-choices" }
                    role="group"
                    aria-label=pref.label
                >
                    {choices.collect_view()}
                </div>
            </div>
        }
    });
    view! {
        <div class="prefs-list">
            {rows.collect_view()}
            <button
                class="pref-reset settings-row-action"
                on:click=move |_| {
                    reset();
                    crate::theme::persist_and_apply(crate::theme::ThemePreference::System);
                    tick.update(|tick| *tick += 1);
                    changed();
                }
            >
                "Reset to defaults"
            </button>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_is_one_of_its_options() {
        for pref in PREFS {
            assert!(
                pref.options.iter().any(|(value, _)| *value == pref.default),
                "{}",
                pref.key
            );
        }
    }
}
