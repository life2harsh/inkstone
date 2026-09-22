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
pub fn render_controls(document: &web_sys::Document) -> web_sys::Element {
    let list = document.create_element("div").unwrap();
    list.set_class_name("prefs-list");
    for pref in PREFS {
        let row = document.create_element("div").unwrap();
        row.set_class_name("prefs-row");
        let label = document.create_element("span").unwrap();
        label.set_class_name("prefs-label");
        label.set_text_content(Some(pref.label));
        row.append_child(&label).unwrap();

        let group = document.create_element("div").unwrap();
        group.set_class_name(if pref.swatches {
            "prefs-swatches"
        } else {
            "prefs-choices"
        });
        group.set_attribute("role", "group").unwrap();
        group.set_attribute("aria-label", pref.label).unwrap();
        let current = get(pref.key);
        for (value, name) in pref.options {
            let button = document.create_element("button").unwrap();
            button.set_class_name(if pref.swatches {
                "pref-choice pref-swatch"
            } else {
                "pref-choice"
            });
            button.set_attribute("data-pref-key", pref.key).unwrap();
            button.set_attribute("data-pref-value", value).unwrap();
            button
                .set_attribute(
                    "aria-pressed",
                    if current == *value { "true" } else { "false" },
                )
                .unwrap();
            if pref.swatches {
                button.set_attribute("aria-label", name).unwrap();
                button.set_attribute("title", name).unwrap();
            } else {
                button.set_text_content(Some(name));
            }
            group.append_child(&button).unwrap();
        }
        row.append_child(&group).unwrap();
        list.append_child(&row).unwrap();
    }
    let reset = document.create_element("button").unwrap();
    reset.set_class_name("pref-reset settings-row-action");
    reset.set_text_content(Some("Reset to defaults"));
    list.append_child(&reset).unwrap();
    list
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
