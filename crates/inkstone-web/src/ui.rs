use crate::{
    DRIVER, Driver, DriverInner, PresenceKind, can_edit_workspace, explorer, graph_label,
    graph_node_radius,
    icons::svg,
    knowledge::WikilinkResolution,
    live, live_toggle_text,
    local_graph::{GraphNodeKind, LocalGraph},
    markdown_render,
    page_export::ExportFormat,
    persistence_state, save_label, search, shortcut_label, sync_status_text, vaults,
    viewport_width,
};
use inkstone_client::{ClientEvent, SessionPhase};
use inkstone_workspace::WorkspaceEntry;
use leptos::{html, prelude::*};
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
    pub auth: RwSignal<Option<crate::auth::Auth>>,
    pub new_vault: RwSignal<Option<crate::vaults::NewVaultDraft>>,
    pub settings: RwSignal<Option<crate::settings::Settings>>,
    pub home: RwSignal<Option<crate::home::Home>>,
    pub home_filter: RwSignal<String>,
    pub home_tag: RwSignal<Option<String>>,
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

pub(crate) fn shell(root: &web_sys::Element) -> web_sys::HtmlElement {
    SHELL.with(|shell| {
        shell
            .get_or_init(|| {
                let app: web_sys::HtmlElement = document()
                    .create_element("div")
                    .expect("app container")
                    .unchecked_into();
                root.append_child(&app).expect("mount app container");
                let auth: web_sys::HtmlElement = document()
                    .create_element("div")
                    .expect("auth host")
                    .unchecked_into();
                auth.style().set_property("display", "contents").expect("auth host style");
                root.append_child(&auth).expect("mount auth host");
                let ui = state();
                leptos::mount::mount_to(auth, move || crate::auth::view(ui)).forget();
                leptos::mount::mount_to(app.clone(), move || {
                    view! {
                        {sidebar(ui)}
                        <div class="main-content">
                            {header(ui)}
                            {crate::home::view(ui)}
                            <div class="page-host" style="display: contents"></div>
                        </div>
                        {inspector(ui)}
                        {move || {
                            let open = ui.header.with(|header| header.sidebar_open)
                                || ui.inspector_open.get();
                            open.then(|| view! {
                                <button class="drawer-backdrop" aria-label="Close panels" on:click=|_| close_panels()></button>
                            })
                        }}
                        {crate::settings::view(ui)}
                        {crate::vaults::new_vault_view(ui)}
                    }
                })
                .forget();
                app
            })
            .clone()
    })
}

pub(crate) fn publish(driver: &Driver, inner: &DriverInner) {
    let ui = state();
    let current = inner
        .workspaces
        .iter()
        .find(|workspace| Some(workspace.id) == inner.workspace_id);
    let mut items = inner
        .workspaces
        .iter()
        .map(|workspace| VaultItem {
            id: workspace.id.to_string(),
            name: inner
                .vault_names
                .get(&workspace.id)
                .cloned()
                .unwrap_or_else(|| "Locked vault".into()),
            detail: vaults::access_summary(workspace),
            icon: vaults::access_of(workspace).icon(),
            current: Some(workspace.id) == inner.workspace_id,
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| item.name.to_lowercase());
    put(
        ui.vault,
        Vault {
            name: inner.workspace_name.clone(),
            detail: match (&inner.guest_name, &inner.public_view) {
                (Some(name), _) => format!("Editing as {name}"),
                (None, Some(_)) => "View only".into(),
                _ if inner.local_mode => "On this device".into(),
                _ => current.map(vaults::access_summary).unwrap_or_default(),
            },
            interactive: !inner.local_mode && inner.public_view.is_none(),
            badge: inner.public_view.as_ref().map(|_| {
                if inner.guest_name.is_some() {
                    "pencil"
                } else {
                    "eye"
                }
            }),
            items,
        },
    );
    put(ui.recent, inner.recent_notes.clone());
    put(ui.selected, inner.selected_path.clone());
    put(ui.entries, inner.entries.clone());
    put(ui.can_edit, can_edit_workspace(inner));

    let phase = inner.client_state.phase();
    let error = inner.sync_error.clone().or(inner.auth_error.clone());
    put(
        ui.footer,
        Footer {
            local: inner.local_mode,
            dot: persistence_state(&save_label(inner).unwrap_or_default()),
            phase: format!("{phase:?}"),
            sync: sync_status_text(inner, phase),
            retry: error.is_some() && phase == SessionPhase::Offline && inner.auth_error.is_none(),
            error,
            index_error: inner.index_error.clone(),
            signed_in: inner.public_view.is_none(),
        },
    );

    let note = inner.selected_path.is_some();
    let active = inner
        .remote_presence
        .values()
        .filter(|presence| !matches!(presence.payload.state, PresenceKind::Idle))
        .collect::<Vec<_>>();
    let inking = active
        .iter()
        .filter(|presence| matches!(presence.payload.state, PresenceKind::Ink { .. }))
        .count();
    put(
        ui.header,
        Header {
            crumb: inner.selected_path.clone().unwrap_or_else(|| "Home".into()),
            note,
            sidebar_open: inner.sidebar_open,
            live: (note && !inner.local_mode)
                .then(|| (live::enabled(), driver.live_direct_count())),
            presence: match (active.len(), inking) {
                (0, _) => None,
                (count, 0) => Some(format!("{count} collaborator(s) editing text")),
                (count, ink) => Some(format!(
                    "{count} collaborator(s) active · {ink} pointing in Ink"
                )),
            },
        },
    );
    put(
        ui.save,
        (note || inner.local_persistence_error.is_some()).then(|| {
            let label = save_label(inner).unwrap_or_else(|| "Saving…".into());
            let state = persistence_state(&label);
            (label, state)
        }),
    );

    put(ui.inspector_open, inner.inspector_open);
    put(
        ui.settings,
        inner
            .settings_open
            .then(|| crate::settings::snapshot(inner)),
    );
    put(ui.new_vault, inner.new_vault.clone());
    put(ui.auth, crate::auth::snapshot(inner));
    put(
        ui.home,
        (inner.selected_path.is_none() && inner.storage_error.is_none())
            .then(|| crate::home::snapshot(inner, ui.home_tag.get_untracked().as_deref())),
    );
    put(ui.panel, panel_of(ui.tab.get_untracked(), inner));

    let modal = inner.settings_open || inner.new_vault.is_some();
    let closed_drawer = viewport_width() <= 760.0 && !inner.sidebar_open;
    SHELL.with(|shell| {
        let Some(app) = shell.get() else { return };
        app.set_hidden(!inner.local_mode && inner.auth_phase != crate::AuthPhase::Ready);
        app.set_class_name(
            match (inner.sidebar_open, inner.local_persistence_error.is_some()) {
                (true, true) => "app-container sidebar-open save-failed",
                (true, false) => "app-container sidebar-open",
                (false, true) => "app-container save-failed",
                (false, false) => "app-container",
            },
        );
        let children = app.children();
        if let Some(main) = children.item(1) {
            let _ = main.toggle_attribute_with_force("inert", modal);
        }
        for (index, hidden) in [(0, closed_drawer), (2, !inner.inspector_open)] {
            if let Some(panel) = children.item(index) {
                let _ = panel.toggle_attribute_with_force("inert", hidden || modal);
                let _ = match hidden {
                    true => panel.set_attribute("aria-hidden", "true"),
                    false => panel.remove_attribute("aria-hidden"),
                };
            }
        }
    });
}

pub(crate) fn refresh() {
    let driver = driver();
    publish(&driver, &driver.0.borrow());
}

fn panel_of(tab: Tab, inner: &DriverInner) -> Panel {
    let knowledge = &inner.knowledge;
    let path = inner.selected_path.as_deref();
    let doc = path.and_then(|path| Some((path, knowledge.path_to_doc.get(path)?)));
    match tab {
        Tab::Outline => Panel::Outline(doc.map(|(_, doc)| {
            doc.headings
                .iter()
                .map(|heading| Heading {
                    text: heading.text.clone(),
                    anchor: heading.anchor.clone(),
                    block: heading.block_id.to_string(),
                    line: heading.line,
                    level: heading.level,
                })
                .collect()
        })),
        Tab::Backlinks => Panel::Links(
            doc.map(|(path, doc)| {
                doc.outgoing_links
                    .iter()
                    .map(|link| {
                        let label = link
                            .alias
                            .clone()
                            .unwrap_or_else(|| link.target_raw.clone());
                        let resolved = if link.is_wikilink {
                            knowledge.resolve_wikilink(path, &link.target_raw)
                        } else {
                            knowledge.resolve_markdown_link(path, &link.target_raw)
                        };
                        match resolved {
                            WikilinkResolution::Resolved(target) => Link::Open(label, target),
                            WikilinkResolution::Ambiguous(_) => Link::Ambiguous(label),
                            WikilinkResolution::Unresolved => {
                                Link::Missing(link.target_raw.clone())
                            }
                        }
                    })
                    .collect()
            }),
            path.map(|path| {
                knowledge
                    .get_backlinks(path)
                    .into_iter()
                    .map(|link| {
                        let context = format!(
                            "{}\n{}",
                            link.source_path,
                            link.context_snippet.unwrap_or_default()
                        );
                        (link.source_name, link.source_path, context)
                    })
                    .collect()
            }),
        ),
        Tab::Tags => Panel::Tags(
            path.and_then(|path| knowledge.path_to_tags.get(path))
                .map(|tags| {
                    let mut tags = tags.iter().cloned().collect::<Vec<_>>();
                    tags.sort();
                    tags
                })
                .unwrap_or_default(),
            knowledge.get_all_tags_with_counts(),
        ),
        Tab::Graph => Panel::Graph(
            path.map(|path| crate::local_graph::build_local_graph(knowledge, path))
                .unwrap_or_default(),
        ),
    }
}

pub(crate) fn filter_tag(tag: &str) {
    let tag = tag.to_lowercase();
    let paths = driver()
        .0
        .borrow()
        .knowledge
        .tag_to_paths
        .get(&tag)
        .map(|paths| paths.iter().cloned().collect())
        .unwrap_or_default();
    state().filter_tag(tag, paths);
}

fn marked(text: &str, query: &str) -> Vec<AnyView> {
    let mut views = Vec::new();
    let mut at = 0;
    if !query.is_empty() {
        for (start, end) in search::find_case_insensitive_ranges(text, &query.to_lowercase()) {
            views.push(text[at..start].to_string().into_any());
            views.push(
                view! { <mark class="search-highlight">{text[start..end].to_string()}</mark> }
                    .into_any(),
            );
            at = end;
        }
    }
    views.push(text[at..].to_string().into_any());
    views
}

fn sidebar(ui: Ui) -> impl IntoView {
    let searching = Memo::new(move |_| {
        ui.query.with(|query| !query.trim().is_empty()) || ui.tag.with(Option::is_some)
    });
    view! {
        <div
            class="sidebar"
            id="workspace-navigation"
            role="navigation"
            aria-label="Workspace navigation"
        >
            <div class="vault-switcher-wrap">
                {move || switcher(ui)}
                {move || ui.menu_open.get().then(|| vault_menu(ui))}
            </div>
            <button
                class="sidebar-home-btn"
                aria-current=move || ui.selected.with(Option::is_none).then_some("page")
                on:click=|_| driver().go_home()
            >
                {svg("home", "icon")}
                <span>"Home"</span>
            </button>
            {search_box(ui)}
            {move || {
                let show = !searching.get() && ui.recent.with(|recent| !recent.is_empty());
                show.then(|| recent_list(ui))
            }}
            <div class="sidebar-heading">
                <span>{move || if searching.get() { "Results" } else { "Files" }}</span>
                {move || {
                    let show = !searching.get() && ui.can_edit.get()
                        && ui.create.with(Option::is_none);
                    show.then(|| explorer_actions(ui))
                }}
            </div>
            <div
                id="workspace-results"
                class="file-list"
                role=move || searching.get().then_some("listbox")
            >
                {move || {
                    if searching.get() { results(ui).into_any() } else { tree(ui).into_any() }
                }}
            </div>
            {footer(ui)}
        </div>
    }
}

fn switcher(ui: Ui) -> impl IntoView {
    let vault = ui.vault.get();
    let label = view! {
        <span class="brand-mark">{svg("mark", "icon")}</span>
        <span class="brand-text">
            <span class="brand-name vault-name">{vault.name}</span>
            <span class="workspace-name">{vault.detail}</span>
        </span>
    };
    if vault.interactive {
        view! {
            <button
                type="button"
                class="brand vault-switcher"
                data-vault-action="menu"
                aria-haspopup="menu"
                aria-expanded=move || bool_str(ui.menu_open.get())
                data-focus-key="vault-switcher"
            >
                {label}
                {svg("chevrons-up-down", "icon vault-chevron")}
            </button>
        }
        .into_any()
    } else {
        view! {
            <div class="brand vault-switcher">
                {label}
                {vault.badge.map(|icon| svg(icon, "icon vault-chevron"))}
            </div>
        }
        .into_any()
    }
}

fn vault_menu(ui: Ui) -> impl IntoView {
    let items = ui.vault.get().items.into_iter().map(|item| {
        view! {
            <button
                type="button"
                class="vault-menu-item"
                role="menuitemradio"
                aria-checked=bool_str(item.current)
                data-vault-action="switch"
                data-workspace-id=item.id
            >
                {svg(item.icon, "icon")}
                <span class="vault-menu-text">
                    <span class="vault-menu-name">{item.name}</span>
                    <span class="vault-menu-detail">{item.detail}</span>
                </span>
                {item.current.then(|| svg("check", "icon vault-menu-check"))}
            </button>
        }
    });
    let commands = [
        ("new", "plus", "New vault"),
        ("share", "users", "Share this vault"),
        ("settings", "settings", "Vault settings"),
    ]
    .map(|(action, icon, label)| {
        view! {
            <button
                type="button"
                class="vault-menu-item vault-menu-command"
                role="menuitem"
                data-vault-action=action
            >
                {svg(icon, "icon")}
                <span>{label}</span>
            </button>
        }
    });
    view! {
        <div class="vault-menu" role="menu" aria-label="Vaults">
            <div class="vault-menu-label">"Vaults"</div>
            {items.collect_view()}
            <div class="vault-menu-divider"></div>
            {commands.into_iter().collect_view()}
        </div>
    }
}

fn search_box(ui: Ui) -> impl IntoView {
    let status = Memo::new(move |_| {
        if ui.indexing.get() {
            Some("Indexing notes...".to_string())
        } else if ui.query.with(|query| query.trim().is_empty()) {
            None
        } else {
            Some(match ui.results.with(Vec::len) {
                0 => "No matching notes".to_string(),
                1 => "1 result".to_string(),
                count => format!("{count} results"),
            })
        }
    });
    let on_keydown = move |event: web_sys::KeyboardEvent| {
        let len = ui.results.with_untracked(Vec::len) as isize;
        match event.key().as_str() {
            "ArrowDown" | "ArrowUp" if len > 0 => {
                let delta = if event.key() == "ArrowDown" { 1 } else { -1 };
                ui.highlighted.update(|index| {
                    *index = Some((index.unwrap_or(0) as isize + delta).rem_euclid(len) as usize)
                });
            }
            "Enter" => {
                let path = ui.highlighted.get_untracked().and_then(|index| {
                    ui.results
                        .with_untracked(|results| results.get(index).map(|hit| hit.path.clone()))
                });
                if let Some(path) = path {
                    open_result(ui, &path);
                }
            }
            "Escape" => {
                ui.clear_search();
                let input: web_sys::HtmlElement = event_target(&event);
                let _ = input.blur();
            }
            _ => return,
        }
        event.prevent_default();
    };
    view! {
        <div class="search-container">
            {move || {
                ui.tag
                    .get()
                    .map(|(tag, _)| {
                        view! {
                            <div class="active-tag-filter">
                                <span>{format!("#{tag}")}</span>
                                <button
                                    class="clear-tag-btn"
                                    aria-label="Clear tag filter"
                                    on:click=move |_| ui.clear_search()
                                >
                                    "×"
                                </button>
                            </div>
                        }
                    })
            }}
            <div class="search-field">
                {svg("search", "icon")}
                <input
                    type="text"
                    class="search-input"
                    placeholder="Search notes"
                    aria-label="Search notes"
                    role="combobox"
                    aria-controls="workspace-results"
                    aria-expanded=move || bool_str(ui.results.with(|results| !results.is_empty()))
                    aria-activedescendant=move || {
                        ui.highlighted.get().map(|index| format!("search-option-{index}"))
                    }
                    data-focus-key="search-input"
                    prop:value=move || ui.query.get()
                    on:input=move |event| {
                        ui.query.set(event_target_value(&event));
                        ui.rank(&driver().0.borrow().search_documents);
                    }
                    on:keydown=on_keydown
                />
                <span class="kbd">{shortcut_label("K")}</span>
            </div>
            {move || {
                status.get().map(|text| view! { <div class="search-status" role="status">{text}</div> })
            }}
        </div>
    }
}

fn recent_list(ui: Ui) -> impl IntoView {
    view! {
        <div class="sidebar-heading recent-heading">
            <span>"Recent"</span>
            <button class="recent-clear-btn" on:click=|_| driver().clear_recent_notes()>
                "Clear"
            </button>
        </div>
        <div class="recent-list">
            <For each=move || ui.recent.get() key=Clone::clone let:path>
                {recent_item(ui, path)}
            </For>
        </div>
    }
}

fn recent_item(ui: Ui, path: String) -> impl IntoView {
    let key = path.clone();
    let active = Memo::new(move |_| ui.selected.with(|selected| selected.as_ref() == Some(&key)));
    let open = path.clone();
    view! {
        <button
            class=move || if active.get() { "recent-item active" } else { "recent-item" }
            data-path=path.clone()
            data-focus-key=format!("recent:{path}")
            aria-current=move || active.get().then_some("page")
            on:click=move |_| driver().select_entry(&open)
        >
            <span class="recent-item-name">{explorer::file_name_only(&path)}</span>
            {path
                .rsplit_once('/')
                .map(|(folder, _)| view! { <span class="recent-item-path">{folder.to_string()}</span> })}
        </button>
    }
}

fn explorer_actions(ui: Ui) -> impl IntoView {
    let start = move |folder: bool| {
        move |_: web_sys::MouseEvent| {
            if folder || !driver().0.borrow().client_state.has_pending_intent() {
                ui.create.set(Some(Create {
                    folder,
                    parent: None,
                }));
            }
        }
    };
    view! {
        <div class="sidebar-heading-actions">
            <button
                class="explorer-new-btn"
                aria-label="New file"
                title="New file"
                data-focus-key="new-file"
                on:click=start(false)
            >
                {svg("file-plus", "icon")}
            </button>
            <button
                class="explorer-new-folder-btn"
                aria-label="New folder"
                title="New folder"
                data-focus-key="new-folder"
                on:click=start(true)
            >
                {svg("folder-plus", "icon")}
            </button>
            <label class="asset-import-label" title="Import asset">
                {svg("upload", "icon")}
                <input
                    class="asset-import-input"
                    type="file"
                    aria-label="Import encrypted asset"
                    on:change=|event| {
                        let input: web_sys::HtmlInputElement = event_target(&event);
                        if let Some(file) = input.files().and_then(|files| files.get(0)) {
                            driver().import_asset(file);
                            input.set_value("");
                        }
                    }
                />
            </label>
        </div>
    }
}

fn results(ui: Ui) -> impl IntoView {
    move || {
        if let Some((_, paths)) = ui
            .tag
            .get()
            .filter(|_| ui.query.with(|q| q.trim().is_empty()))
        {
            return paths
                .into_iter()
                .map(|path| {
                    let open = path.clone();
                    let label = path.clone();
                    view! {
                        <button
                            class="search-result-item"
                            data-path=path
                            role="option"
                            on:click=move |_| open_result(ui, &open)
                        >
                            {label}
                        </button>
                    }
                })
                .collect_view()
                .into_any();
        }
        let hits = ui.results.get();
        if hits.is_empty() {
            return view! { <div class="search-no-results">"No matching notes"</div> }.into_any();
        }
        let query = ui.query.get_untracked();
        hits.into_iter()
            .enumerate()
            .map(|(index, hit)| {
                let key = hit.path.clone();
                let active =
                    Memo::new(move |_| ui.selected.with(|selected| selected.as_ref() == Some(&key)));
                let lit = move || ui.highlighted.get() == Some(index);
                let open = hit.path.clone();
                let detail = hit
                    .snippet
                    .clone()
                    .or_else(|| (!hit.title.is_empty()).then(|| hit.title.clone()));
                view! {
                    <button
                        class=move || {
                            match (active.get(), lit()) {
                                (true, true) => "search-result-item active highlighted",
                                (true, false) => "search-result-item active",
                                (false, true) => "search-result-item highlighted",
                                (false, false) => "search-result-item",
                            }
                        }
                        id=format!("search-option-{index}")
                        role="option"
                        aria-selected=move || bool_str(lit())
                        data-path=hit.path.clone()
                        data-focus-key=format!("search-result:{}", hit.path)
                        aria-current=move || active.get().then_some("page")
                        on:click=move |_| open_result(ui, &open)
                    >
                        <div class="search-result-name">{marked(&hit.file_name, &query)}</div>
                        <div class="search-result-path">{marked(&hit.path, &query)}</div>
                        {detail
                            .map(|detail| {
                                view! {
                                    <div class="search-result-snippet">{marked(&detail, &query)}</div>
                                }
                            })}
                    </button>
                }
            })
            .collect_view()
            .into_any()
    }
}

fn open_result(ui: Ui, path: &str) {
    driver().select_entry(path);
    ui.clear_search();
}

fn tree(ui: Ui) -> impl IntoView {
    let rows = Memo::new(move |_| {
        let edit = ui.can_edit.get();
        ui.entries.with(|entries| {
            ui.expanded.with(|expanded| {
                explorer::build_flat_tree(entries, expanded)
                    .into_iter()
                    .map(|node| Row {
                        path: node.path,
                        name: node.name,
                        depth: node.depth,
                        folder: node.is_folder,
                        edit,
                    })
                    .collect::<Vec<_>>()
            })
        })
    });
    view! {
        {move || {
            let folder = ui.create.with(|create| create.as_ref().map(|create| create.folder));
            folder.filter(|_| ui.can_edit.get()).map(|folder| create_row(ui, folder))
        }}
        <For each=move || rows.get() key=Clone::clone let:row>
            {if row.folder { folder_row(ui, row).into_any() } else { file_row(ui, row).into_any() }}
        </For>
    }
}

fn autofocus() -> NodeRef<html::Input> {
    let input = NodeRef::<html::Input>::new();
    Effect::new(move |_| {
        if let Some(input) = input.get() {
            let _ = input.focus();
        }
    });
    input
}

fn create_row(ui: Ui, folder: bool) -> impl IntoView {
    view! {
        <div class="explorer-create-row">
            <input
                node_ref=autofocus()
                class="explorer-create-input"
                placeholder=if folder { "Folder name" } else { "filename.md" }
                aria-label="New file path"
                on:keydown=move |event: web_sys::KeyboardEvent| {
                    match event.key().as_str() {
                        "Enter" => {
                            let value = event_target_value(&event);
                            if !value.trim().is_empty() {
                                driver().create_file(&value);
                            }
                        }
                        "Escape" => {
                            ui.create.set(None);
                            focus_soon("new-file");
                        }
                        _ => return,
                    }
                    event.prevent_default();
                }
            />
        </div>
    }
}

fn folder_row(ui: Ui, row: Row) -> impl IntoView {
    let path = row.path.clone();
    let expanded = {
        let path = path.clone();
        Memo::new(move |_| ui.expanded.with(|expanded| expanded.contains(&path)))
    };
    let target = {
        let path = path.clone();
        Memo::new(move |_| {
            ui.create
                .with(|create| create.as_ref().and_then(|c| c.parent.as_ref()) == Some(&path))
        })
    };
    let toggle = {
        let path = path.clone();
        move |_: web_sys::MouseEvent| {
            ui.expanded.update(|expanded| {
                if !expanded.remove(&path) {
                    expanded.insert(path.clone());
                }
            })
        }
    };
    let actions = row.edit.then(|| {
        let (create, rename, delete) = (path.clone(), path.clone(), path.clone());
        view! {
            <button
                class="folder-create-btn"
                data-path=path.clone()
                aria-label=format!("Create file in {}", row.name)
                title=format!("Create file in {}", row.name)
                on:click=move |_| {
                    ui.expanded.update(|expanded| {
                        expanded.insert(create.clone());
                    });
                    ui.create.set(Some(Create {
                        folder: false,
                        parent: Some(create.clone()),
                    }));
                }
            >
                {svg("plus", "icon")}
            </button>
            <button
                class="folder-rename-btn"
                data-path=path.clone()
                aria-label=format!("Rename or move {}", row.name)
                title=format!("Rename or move {}", row.name)
                on:click=move |_| {
                    if let Some(destination) = window()
                        .prompt_with_message_and_default("New folder path", &rename)
                        .ok()
                        .flatten()
                    {
                        driver().rename_folder_tree(&rename, &destination);
                    }
                }
            >
                {svg("pencil", "icon")}
            </button>
            <button
                class="folder-delete-btn"
                data-path=path.clone()
                aria-label=format!("Delete folder {}", row.name)
                title=format!("Delete folder {}", row.name)
                on:click=move |_| {
                    if window()
                        .confirm_with_message(&format!("Delete {delete} and everything inside it?"))
                        .unwrap_or(false)
                    {
                        driver().delete_folder_tree(&delete);
                    }
                }
            >
                {svg("trash", "icon")}
            </button>
        }
    });
    view! {
        <div class="explorer-folder-row" style=format!("padding-left: {}px", 6 + row.depth * 16)>
            <button
                class=move || {
                    match (expanded.get(), target.get()) {
                        (true, true) => "explorer-folder expanded active-target",
                        (true, false) => "explorer-folder expanded",
                        (false, true) => "explorer-folder active-target",
                        (false, false) => "explorer-folder",
                    }
                }
                data-path=path.clone()
                data-depth=row.depth.to_string()
                aria-expanded=move || bool_str(expanded.get())
                data-focus-key=format!("folder:{path}")
                on:click=toggle
            >
                <span class="explorer-toggle">{svg("chevron-right", "icon")}</span>
                <span class="folder-name">{row.name.clone()}</span>
            </button>
            {actions}
        </div>
    }
}

fn file_row(ui: Ui, row: Row) -> impl IntoView {
    let path = row.path.clone();
    let active = {
        let path = path.clone();
        Memo::new(move |_| {
            ui.selected
                .with(|selected| selected.as_ref() == Some(&path))
        })
    };
    let renaming = {
        let path = path.clone();
        Memo::new(move |_| {
            ui.renaming
                .with(|renaming| renaming.as_ref() == Some(&path))
        })
    };
    let select = {
        let path = path.clone();
        move |event: web_sys::MouseEvent| {
            if event.target() == event.current_target() {
                driver().select_entry(&path);
            }
        }
    };
    let actions = row.edit.then(|| {
        let (rename, delete) = (path.clone(), path.clone());
        view! {
            <span class="file-actions">
                <button
                    class="rename-btn"
                    data-path=path.clone()
                    data-focus-key=format!("rename:{path}")
                    aria-label=format!("Rename {}", row.name)
                    title=format!("Rename {}", row.name)
                    on:click=move |_| {
                        if !driver().0.borrow().client_state.has_pending_intent() {
                            ui.renaming.set(Some(rename.clone()));
                        }
                    }
                >
                    {svg("pencil", "icon")}
                </button>
                <button
                    class="delete-btn"
                    data-path=path.clone()
                    aria-label=format!("Delete {}", row.name)
                    title=format!("Delete {}", row.name)
                    on:click=move |_| {
                        let driver = driver();
                        if driver.0.borrow().client_state.has_pending_intent() {
                            return;
                        }
                        if window()
                            .confirm_with_message(
                                &format!("Delete \"{delete}\"? This cannot be undone."),
                            )
                            .unwrap_or(false)
                        {
                            driver.delete_entry(&delete);
                        }
                    }
                >
                    {svg("trash", "icon")}
                </button>
            </span>
        }
    });
    let name = row.name.clone();
    let entry = path.clone();
    view! {
        <div
            class=move || if active.get() { "file-entry active" } else { "file-entry" }
            data-path=path.clone()
            data-depth=row.depth.to_string()
            style=format!("padding-left: {}px", 6 + row.depth * 16)
            on:click=select
        >
            {move || {
                if renaming.get() {
                    rename_input(ui, entry.clone(), name.clone()).into_any()
                } else {
                    let open = entry.clone();
                    view! {
                        <button
                            class="file-select-btn"
                            data-path=entry.clone()
                            data-focus-key=format!("file:{entry}")
                            aria-current=move || active.get().then_some("page")
                            on:click=move |_| driver().select_entry(&open)
                        >
                            {svg("file", "icon")}
                            <span class="file-name">{name.clone()}</span>
                        </button>
                    }
                        .into_any()
                }
            }}
            {actions}
        </div>
    }
}

fn rename_input(ui: Ui, path: String, name: String) -> impl IntoView {
    view! {
        <input
            node_ref=autofocus()
            class="rename-input"
            aria-label=format!("Rename {name}")
            prop:value=name.clone()
            on:keydown=move |event: web_sys::KeyboardEvent| {
                match event.key().as_str() {
                    "Enter" => {
                        let name = event_target_value(&event);
                        let name = name.trim();
                        if !name.is_empty() {
                            let renamed = match explorer::parent_folder(&path) {
                                Some(parent) if !parent.is_empty() => format!("{parent}/{name}"),
                                _ => name.to_string(),
                            };
                            driver().rename_entry(&path, &renamed);
                        }
                    }
                    "Escape" => {
                        ui.renaming.set(None);
                        if let Some(selected) = ui.selected.get_untracked() {
                            focus_soon(&format!("file:{selected}"));
                        }
                    }
                    _ => return,
                }
                event.prevent_default();
            }
        />
    }
}

fn footer(ui: Ui) -> impl IntoView {
    move || {
        let footer = ui.footer.get();
        let status = if footer.local {
            view! {
                <span class="status-dot" data-state=footer.dot></span>
                <span>"Stored on this device"</span>
            }
            .into_any()
        } else {
            view! {
                <span class=format!("phase-badge phase-{}", footer.phase.to_lowercase())>
                    {footer.phase.clone()}
                </span>
            }
            .into_any()
        };
        let synced = (!footer.local).then(|| {
            view! {
                <div class="sync-status" role="status">{footer.sync}</div>
                {footer.error.clone().map(|error| view! { <div class="sync-error" role="alert">{error}</div> })}
                {footer.retry.then(|| view! {
                    <button
                        class="retry-sync-btn text-btn"
                        on:click=|_| {
                            let driver = driver();
                            driver.0.borrow_mut().sync_error = None;
                            driver.dispatch(ClientEvent::RetrySync);
                        }
                    >
                        "Retry sync now"
                    </button>
                })}
                {footer.index_error.clone().map(|error| view! { <div class="index-error" role="status">{error}</div> })}
                {footer.signed_in.then(|| view! {
                    <button class="logout-btn text-btn" on:click=|_| driver().logout()>"Sign out"</button>
                })}
            }
        });
        view! {
            <div class="sidebar-footer">
                <div class="vault-status">{status}</div>
                {footer.signed_in.then(|| view! {
                    <button
                        class="settings-open-btn"
                        aria-haspopup="dialog"
                        data-focus-key="settings-open"
                        aria-label="Vault settings"
                        title="Vault settings"
                        on:click=|_| {
                            let driver = driver();
                            let mut inner = driver.0.borrow_mut();
                            inner.settings_open = true;
                            inner.pending_focus_key = Some("settings-content".into());
                            drop(inner);
                            driver.render();
                            driver.refresh_settings_data();
                        }
                    >
                        {svg("settings", "icon")}
                    </button>
                })}
                {synced}
            </div>
        }
    }
}

fn header(ui: Ui) -> impl IntoView {
    let crumb = Memo::new(move |_| ui.header.with(|header| header.crumb.clone()));
    let note = Memo::new(move |_| ui.header.with(|header| header.note));
    let live = Memo::new(move |_| ui.header.with(|header| header.live));
    let toggle_sidebar = |_| {
        let driver = driver();
        let mut inner = driver.0.borrow_mut();
        inner.sidebar_open = !inner.sidebar_open;
        inner.pending_focus_key = Some(
            if inner.sidebar_open {
                "search-input"
            } else {
                "sidebar-toggle"
            }
            .into(),
        );
        drop(inner);
        driver.render();
    };
    view! {
        <header class="workspace-topbar">
            <button
                class="sidebar-toggle-btn"
                aria-label="Toggle workspace navigation"
                title="Toggle workspace navigation"
                aria-controls="workspace-navigation"
                aria-expanded=move || bool_str(ui.header.with(|header| header.sidebar_open))
                data-focus-key="sidebar-toggle"
                on:click=toggle_sidebar
            >
                {svg("panel-left", "icon")}
            </button>
            <div class="breadcrumb" title=move || crumb.get()>
                {move || {
                    let crumb = crumb.get();
                    let parts = crumb.split('/').map(str::to_string).collect::<Vec<_>>();
                    let last = parts.len() - 1;
                    parts
                        .into_iter()
                        .enumerate()
                        .map(|(index, part)| {
                            view! {
                                {(index > 0).then(|| view! { <span class="crumb-sep">"/"</span> })}
                                <span class=if index == last { "crumb-current" } else { "crumb" }>
                                    {part}
                                </span>
                            }
                        })
                        .collect_view()
                }}
            </div>
            <div class="topbar-actions">
                <button
                    class="topbar-search-btn"
                    aria-label="Search pages"
                    data-focus-key="quick-search"
                    on:click=|_| quick_search(false)
                >
                    {svg("search", "icon")}
                    <span>"Search"</span>
                    <span class="kbd">{shortcut_label("K")}</span>
                </button>
                {move || {
                    ui.save
                        .get()
                        .map(|(label, state)| {
                            view! {
                                <span class="topbar-save-status" data-state=state role="status">
                                    {label}
                                </span>
                            }
                        })
                }}
                {move || {
                    live.get()
                        .map(|(on, direct)| {
                            let (label, title) = live_toggle_text(on, direct);
                            view! {
                                <button
                                    class="live-toggle-btn"
                                    aria-pressed=bool_str(on)
                                    data-direct=direct.to_string()
                                    title=title
                                    data-focus-key="live-toggle"
                                    on:click=|_| driver().toggle_live_mode()
                                >
                                    <span>{label}</span>
                                </button>
                            }
                        })
                }}
                {move || {
                    ui.header
                        .with(|header| header.presence.clone())
                        .map(|text| view! { <span class="collaborator-presence">{text}</span> })
                }}
                {move || note.get().then(|| mode_bar(ui))}
            </div>
        </header>
    }
}

fn mode_bar(ui: Ui) -> impl IntoView {
    view! {
        <div class="mode-bar">
            <div class="export-wrap">
                <button
                    class="mode-toggle-btn export-btn"
                    type="button"
                    aria-haspopup="menu"
                    aria-expanded=move || bool_str(ui.export_open.get())
                    data-focus-key="export-menu"
                    aria-label="Export"
                    title="Export"
                    on:click=move |_| ui.export_open.update(|open| *open = !*open)
                >
                    {svg("download", "icon")}
                </button>
                {move || {
                    ui.export_open
                        .get()
                        .then(|| {
                            view! {
                                <div class="export-menu" role="menu" aria-label="Export">
                                    {ExportFormat::ALL.map(|format| export_item(ui, format)).collect_view()}
                                </div>
                            }
                        })
                }}
            </div>
            <button
                class="mode-toggle-btn toggle-inspector-btn"
                aria-label="Inspector"
                title="Inspector"
                aria-controls="document-inspector"
                aria-expanded=move || bool_str(ui.inspector_open.get())
                data-focus-key="inspector-toggle"
                on:click=|_| toggle_inspector()
            >
                {svg("panel-right", "icon")}
            </button>
        </div>
    }
}

fn export_item(ui: Ui, format: ExportFormat) -> impl IntoView {
    view! {
        <button
            class="export-menu-item"
            type="button"
            role="menuitem"
            data-export-action=format.key()
            on:click=move |_| {
                ui.export_open.set(false);
                driver().export(format);
            }
        >
            <strong>{format.title()}</strong>
            <span>{format.detail()}</span>
        </button>
    }
}

pub(crate) fn quick_search(open: bool) {
    let driver = driver();
    if open || viewport_width() <= 760.0 {
        driver.0.borrow_mut().sidebar_open = true;
        driver.render();
    }
    if let Some(input) = document()
        .query_selector(".search-input")
        .ok()
        .flatten()
        .and_then(|input| input.dyn_into::<web_sys::HtmlInputElement>().ok())
    {
        let _ = input.focus();
        input.select();
    }
}

pub(crate) fn toggle_inspector() {
    let driver = driver();
    let mut inner = driver.0.borrow_mut();
    inner.inspector_open = !inner.inspector_open;
    inner.pending_focus_key = Some(if inner.inspector_open {
        format!("inspector-tab:{}", state().tab.get_untracked().name())
    } else {
        "inspector-toggle".to_string()
    });
    drop(inner);
    driver.render();
}

fn select_tab(tab: Tab) {
    state().tab.set(tab);
    refresh();
    focus_soon(&format!("inspector-tab:{}", tab.name()));
}

fn inspector(ui: Ui) -> impl IntoView {
    let on_keydown = move |event: web_sys::KeyboardEvent| {
        let count = Tab::ALL.len();
        let focused = event_target::<web_sys::Element>(&event).get_attribute("data-tab");
        let current = Tab::ALL
            .iter()
            .position(|tab| Some(tab.name()) == focused.as_deref())
            .unwrap_or(0);
        let next = match event.key().as_str() {
            "ArrowRight" => (current + 1) % count,
            "ArrowLeft" => (current + count - 1) % count,
            "Home" => 0,
            "End" => count - 1,
            _ => return,
        };
        event.prevent_default();
        select_tab(Tab::ALL[next]);
    };
    let tabs = Tab::ALL.map(|tab| {
        let on = move || ui.tab.get() == tab;
        view! {
            <button
                class=move || if on() { "inspector-tab active" } else { "inspector-tab" }
                data-tab=tab.name()
                id=format!("inspector-tab-{}", tab.name().to_lowercase())
                role="tab"
                aria-controls="inspector-panel"
                aria-selected=move || bool_str(on())
                tabindex=move || if on() { "0" } else { "-1" }
                data-focus-key=format!("inspector-tab:{}", tab.name())
                on:click=move |_| select_tab(tab)
            >
                {tab.name()}
            </button>
        }
    });
    view! {
        <div
            id="document-inspector"
            role="complementary"
            aria-label="Document inspector"
            class=move || if ui.inspector_open.get() { "inspector open" } else { "inspector" }
        >
            <div class="inspector-header">
                <div
                    class="inspector-tabs"
                    role="tablist"
                    aria-label="Inspector sections"
                    on:keydown=on_keydown
                >
                    {tabs.into_iter().collect_view()}
                </div>
                <button
                    class="toggle-inspector-btn inspector-close-btn"
                    aria-label="Close document inspector"
                    title="Close document inspector"
                    on:click=|_| toggle_inspector()
                >
                    {svg("x", "icon")}
                </button>
            </div>
            <div
                id="inspector-panel"
                class="inspector-section"
                role="tabpanel"
                aria-labelledby=move || format!("inspector-tab-{}", ui.tab.get().name().to_lowercase())
                data-section=move || ui.tab.get().name().to_lowercase()
            >
                {move || panel(ui.panel.get())}
            </div>
        </div>
    }
}

fn muted(text: &'static str) -> AnyView {
    view! {
        <div class="outline-item" style="color: var(--text-muted); cursor: default;">
            {text}
        </div>
    }
    .into_any()
}

fn list<T>(items: Vec<T>, empty: &'static str, item: impl Fn(T) -> AnyView) -> AnyView {
    if items.is_empty() {
        muted(empty)
    } else {
        items.into_iter().map(item).collect_view().into_any()
    }
}

fn panel(panel: Panel) -> AnyView {
    match panel {
        Panel::Outline(headings) => headings
            .map(|headings| list(headings, "No headings", outline_item))
            .into_any(),
        Panel::Links(outgoing, backlinks) => view! {
            <h4>"Outgoing Links"</h4>
            {outgoing.map(|links| list(links, "No outgoing links", link_item))}
            <h4>"Backlinks"</h4>
            {backlinks
                .map(|links| {
                    list(
                        links,
                        "No backlinks",
                        |(name, path, context)| {
                            let open = path.clone();
                            view! {
                                <button
                                    class="link-item"
                                    data-path=path
                                    on:click=move |_| driver().select_entry(&open)
                                >
                                    {name}
                                    <span class="backlink-context">{context}</span>
                                </button>
                            }
                                .into_any()
                        },
                    )
                })}
        }
        .into_any(),
        Panel::Tags(current, all) => view! {
            <h4>"Current Note Tags"</h4>
            {current.into_iter().map(|tag| tag_item(tag, None)).collect_view()}
            <h4>"Workspace Tags"</h4>
            {list(all, "No tags in workspace", |(tag, count)| tag_item(tag, Some(count)))}
        }
        .into_any(),
        Panel::Graph(graph) => graph_view(graph).into_any(),
    }
}

fn outline_item(heading: Heading) -> AnyView {
    let (anchor, block) = (heading.anchor.clone(), heading.block.clone());
    view! {
        <button
            class="outline-item"
            data-anchor=heading.anchor
            data-block-id=heading.block
            data-heading-line=heading.line.to_string()
            data-heading-level=heading.level.to_string()
            style=format!("padding-left: {}px", heading.level.saturating_sub(1) as usize * 12)
            on:click=move |_| {
                let target = document()
                    .get_element_by_id(&anchor)
                    .or_else(|| {
                        document()
                            .query_selector(&format!("[data-block-id=\"{block}\"]"))
                            .ok()
                            .flatten()
                    });
                if let Some(target) = target {
                    target.scroll_into_view();
                }
            }
        >
            {heading.text}
        </button>
    }
    .into_any()
}

fn link_item(link: Link) -> AnyView {
    match link {
        Link::Open(label, path) => {
            let open = path.clone();
            view! {
                <button
                    class="link-item"
                    data-path=path
                    on:click=move |_| driver().select_entry(&open)
                >
                    {label}
                </button>
            }
            .into_any()
        }
        Link::Ambiguous(label) => {
            view! { <button class="link-item ambiguous">{label}</button> }.into_any()
        }
        Link::Missing(target) => {
            let (missing, label) = (target.clone(), target.clone());
            view! {
                <button
                    class="link-item missing"
                    data-missing-target=target
                    on:click=move |_| {
                        let driver = driver();
                        let source = driver.0.borrow().selected_path.clone().unwrap_or_default();
                        driver.request_missing_note(&missing, &source);
                    }
                >
                    {label}
                </button>
            }
            .into_any()
        }
    }
}

fn tag_item(tag: String, count: Option<usize>) -> AnyView {
    let label = format!("#{tag}");
    let filter = tag.clone();
    let body = match count {
        Some(count) => view! {
            <span class="tag-name">{label}</span>
            <span class="tag-count">{count.to_string()}</span>
        }
        .into_any(),
        None => label.into_any(),
    };
    view! {
        <button class="tag-item" data-tag=tag on:click=move |_| filter_tag(&filter)>
            {body}
        </button>
    }
    .into_any()
}

fn graph_view(graph: LocalGraph) -> impl IntoView {
    let point = |path: &str| graph.nodes.iter().find(|node| node.path == path);
    let edges = graph
        .edges
        .iter()
        .filter_map(|edge| {
            let (source, target) = (point(&edge.source)?, point(&edge.target)?);
            Some(view! {
                <line
                    class="local-graph-edge"
                    x1=source.x.to_string()
                    y1=source.y.to_string()
                    x2=target.x.to_string()
                    y2=target.y.to_string()
                    data-source=edge.source.clone()
                    data-target=edge.target.clone()
                ></line>
            })
        })
        .collect_view();
    let nodes = graph
        .nodes
        .iter()
        .map(|node| {
            let kind = match node.kind {
                GraphNodeKind::Active => "active",
                GraphNodeKind::Outgoing => "outgoing",
                GraphNodeKind::Backlink => "backlink",
                GraphNodeKind::Connected => "connected",
            };
            let radius = graph_node_radius(node.kind);
            view! {
                <g class=format!("local-graph-node {kind}") data-path=node.path.clone()>
                    <circle cx=node.x.to_string() cy=node.y.to_string() r=radius.to_string()></circle>
                    <title>{node.label.clone()}</title>
                    <text x=node.x.to_string() y=(node.y + radius + 14.0).to_string() text-anchor="middle">
                        {graph_label(&node.label)}
                    </text>
                </g>
            }
        })
        .collect_view();
    let hits = graph
        .nodes
        .iter()
        .map(|node| {
            let open = node.path.clone();
            let diameter = (graph_node_radius(node.kind) + 3.0) * 2.0;
            view! {
                <button
                    class="graph-node-hit"
                    data-graph-path=node.path.clone()
                    data-path=node.path.clone()
                    data-focus-key=format!("graph:{}", node.path)
                    aria-label=if node.kind == GraphNodeKind::Active {
                        format!("Open {} (current note)", node.label)
                    } else {
                        format!("Open {}", node.label)
                    }
                    style=format!(
                        "left:{}%;top:{}%;width:{}%",
                        node.x / 3.0,
                        node.y / 2.64,
                        diameter / 3.0,
                    )
                    on:click=move |_| driver().select_entry(&open)
                ></button>
            }
        })
        .collect_view();
    view! {
        <div class="graph-header">"Active note neighborhood"</div>
        {(graph.nodes.len() <= 1)
            .then(|| view! { <div class="graph-empty-state">"No linked notes yet. Add a link or backlink."</div> })}
        <div class="local-graph-wrap">
            <svg class="local-graph" viewBox="0 0 300 264" role="img" aria-label="Linked note graph">
                {edges}
                {nodes}
            </svg>
            {hits}
        </div>
    }
}

fn close_panels() {
    let driver = driver();
    let mut inner = driver.0.borrow_mut();
    inner.pending_focus_key = Some(
        if inner.inspector_open {
            "inspector-toggle"
        } else {
            "sidebar-toggle"
        }
        .into(),
    );
    inner.sidebar_open = false;
    inner.inspector_open = false;
    drop(inner);
    driver.render();
}
