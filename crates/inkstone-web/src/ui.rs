use crate::{DRIVER, Driver, local_graph::LocalGraph, markdown_render, search};
use inkstone_workspace::WorkspaceEntry;
use leptos::prelude::*;
use std::{cell::OnceCell, collections::HashSet};
use wasm_bindgen::JsCast;

#[derive(Clone, PartialEq, Default)]
struct Vault {
    name: String,
    detail: String,
    interactive: bool,
    badge: Option<&'static str>,
    items: Vec<VaultItem>,
}

#[derive(Clone, PartialEq)]
struct VaultItem {
    id: String,
    name: String,
    detail: String,
    icon: &'static str,
    current: bool,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Row {
    path: String,
    name: String,
    depth: usize,
    folder: bool,
    edit: bool,
}

#[derive(Clone, PartialEq, Default)]
pub(crate) struct Create {
    pub folder: bool,
    pub parent: Option<String>,
}

#[derive(Clone, PartialEq, Default)]
struct Footer {
    local: bool,
    dot: &'static str,
    phase: String,
    sync: &'static str,
    error: Option<String>,
    retry: bool,
    index_error: Option<String>,
    signed_in: bool,
}

#[derive(Clone, PartialEq, Default)]
struct Header {
    crumb: String,
    note: bool,
    sidebar_open: bool,
    live: Option<(bool, usize)>,
    presence: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum Tab {
    Backlinks,
    #[default]
    Outline,
    Tags,
    Graph,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Backlinks, Tab::Outline, Tab::Tags, Tab::Graph];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Tab::Backlinks => "Backlinks",
            Tab::Outline => "Outline",
            Tab::Tags => "Tags",
            Tab::Graph => "Graph",
        }
    }
}

#[derive(Clone, PartialEq)]
struct Heading {
    text: String,
    anchor: String,
    block: String,
    line: usize,
    level: u8,
}

#[derive(Clone, PartialEq)]
enum Link {
    Open(String, String),
    Ambiguous(String),
    Missing(String),
}

#[derive(Clone, PartialEq)]
enum Panel {
    Outline(Option<Vec<Heading>>),
    Links(Option<Vec<Link>>, Option<Vec<(String, String, String)>>),
    Tags(Vec<String>, Vec<(String, usize)>),
    Graph(LocalGraph),
}

impl Default for Panel {
    fn default() -> Self {
        Panel::Outline(None)
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Ui {
    pub tab: RwSignal<Tab>,
    inspector_open: RwSignal<bool>,
    panel: RwSignal<Panel>,
    vault: RwSignal<Vault>,
    recent: RwSignal<Vec<String>>,
    selected: RwSignal<Option<String>>,
    entries: RwSignal<Vec<WorkspaceEntry>>,
    can_edit: RwSignal<bool>,
    footer: RwSignal<Footer>,
    header: RwSignal<Header>,
    save: RwSignal<Option<(String, &'static str)>>,
    pub menu_open: RwSignal<bool>,
    pub export_open: RwSignal<bool>,
    pub query: RwSignal<String>,
    pub tag: RwSignal<Option<(String, Vec<String>)>>,
    pub indexing: RwSignal<bool>,
    results: RwSignal<Vec<search::SearchResult>>,
    highlighted: RwSignal<Option<usize>>,
    pub expanded: RwSignal<HashSet<String>>,
    pub create: RwSignal<Option<Create>>,
    pub renaming: RwSignal<Option<String>>,
}

impl Ui {
    pub(crate) fn rank(self, docs: &[search::SearchDocument]) {
        let results = self
            .query
            .with_untracked(|query| search::rank_results(docs, query));
        let current = self.highlighted.get_untracked().unwrap_or(0);
        self.highlighted
            .set((!results.is_empty()).then_some(if current < results.len() {
                current
            } else {
                0
            }));
        self.results.set(results);
    }

    pub(crate) fn searching(self) -> bool {
        self.query.with_untracked(|query| !query.trim().is_empty())
            || self.tag.with_untracked(Option::is_some)
    }

    pub(crate) fn clear_search(self) {
        self.query.set(String::new());
        self.tag.set(None);
        self.results.set(Vec::new());
        self.highlighted.set(None);
    }

    pub(crate) fn filter_tag(self, tag: String, paths: Vec<String>) {
        self.clear_search();
        self.tag.set(Some((tag, paths)));
    }

    pub(crate) fn reveal(self, path: &str) {
        self.expanded
            .update(|expanded| expanded.extend(markdown_render::ancestor_folders(path)));
    }

    pub(crate) fn reset_explorer(self) {
        self.renaming.set(None);
        self.create.set(None);
    }
}

thread_local! {
    static UI: OnceCell<Ui> = const { OnceCell::new() };
    static SHELL: OnceCell<web_sys::HtmlElement> = const { OnceCell::new() };
}

pub(crate) fn state() -> Ui {
    UI.with(|cell| *cell.get_or_init(Ui::default))
}

fn put<T: PartialEq + Send + Sync + 'static>(signal: RwSignal<T>, value: T) {
    if signal.with_untracked(|current| *current != value) {
        signal.set(value);
    }
}

pub(crate) fn driver() -> Driver {
    DRIVER.with(|driver| driver.borrow().clone().expect("driver"))
}

pub(crate) fn focus_soon(key: &str) {
    let selector = format!("[data-focus-key=\"{}\"]", key.replace('"', "\\\""));
    set_timeout(
        move || {
            if let Some(element) = document()
                .query_selector(&selector)
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
            {
                let _ = element.focus();
            }
        },
        std::time::Duration::ZERO,
    );
}

pub(crate) fn bool_str(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

