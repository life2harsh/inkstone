use inkstone_core::document::{BlockContent, InkstoneDocument};

use crate::{
    DriverInner,
    icons::svg,
    ui::{Create, Ui, refresh, state},
};
use leptos::prelude::*;

const VIEW_KEY: &str = "inkstone-home-view";
const SORT_KEY: &str = "inkstone-home-sort";
const SNIPPET_CHARS: usize = 150;
const MAX_TASKS_PER_NOTE: usize = 50;
const TASKS_SHOWN: usize = 12;
const RECENT_SHOWN: usize = 4;
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NoteSummary {
    pub edited_at_ms: i64,
    pub words: usize,
    pub has_ink: bool,
    pub open_tasks: Vec<String>,
    pub done_tasks: usize,
    pub snippet: String,
}

impl NoteSummary {
    pub fn total_tasks(&self) -> usize {
        self.open_tasks.len() + self.done_tasks
    }
}
pub fn summarize(doc: &InkstoneDocument, edited_at_ms: i64) -> NoteSummary {
    let mut summary = NoteSummary {
        edited_at_ms: doc
            .blocks
            .iter()
            .map(|block| block.updated_at_ms)
            .max()
            .unwrap_or(0)
            .max(edited_at_ms),
        ..NoteSummary::default()
    };
    let mut snippet = String::new();
    for block in &doc.blocks {
        match &block.content {
            BlockContent::Markdown { text } => {
                for line in text.lines() {
                    match task(line) {
                        Some((false, item)) => {
                            if summary.open_tasks.len() < MAX_TASKS_PER_NOTE {
                                summary.open_tasks.push(item);
                            }
                        }
                        Some((true, _)) => summary.done_tasks += 1,
                        None => {}
                    }
                    let plain = plain_line(line);
                    summary.words += plain.split_whitespace().count();
                    if snippet.chars().count() < SNIPPET_CHARS && !plain.is_empty() {
                        if !snippet.is_empty() {
                            snippet.push_str(" · ");
                        }
                        snippet.push_str(&plain);
                    }
                }
            }
            BlockContent::Ink { stonepen_json, .. } if !stonepen_json.is_empty() => {
                summary.has_ink = true;
            }
            _ => {}
        }
    }
    if doc.ink.as_ref().is_some_and(|ink| !ink.anchors.is_empty()) {
        summary.has_ink = true;
    }
    summary.snippet = truncate(&snippet, SNIPPET_CHARS);
    summary
}
fn task(line: &str) -> Option<(bool, String)> {
    let mut rest = line.trim_start();
    while let Some(inner) = rest.strip_prefix('>') {
        rest = inner.trim_start();
    }
    let rest = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
        .or_else(|| {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            (digits > 0)
                .then(|| rest[digits..].strip_prefix(". "))
                .flatten()
        })?;
    let (checked, text) = if let Some(text) = rest.strip_prefix("[ ]") {
        (false, text)
    } else if let Some(text) = rest
        .strip_prefix("[x]")
        .or_else(|| rest.strip_prefix("[X]"))
    {
        (true, text)
    } else {
        return None;
    };
    let text = plain_inline(text.trim());
    (!text.is_empty()).then_some((checked, text))
}
fn plain_line(line: &str) -> String {
    let mut rest = line.trim();
    if rest.starts_with("```") || rest.starts_with("$$") || rest.starts_with('|') {
        return String::new();
    }
    if rest.chars().all(|c| matches!(c, '-' | '*' | '_' | ' ')) {
        return String::new();
    }
    while let Some(inner) = rest.strip_prefix('>') {
        rest = inner.trim_start();
    }
    rest = rest.trim_start_matches('#').trim_start();
    for marker in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ "] {
        if let Some(inner) = rest.strip_prefix(marker) {
            rest = inner;
            break;
        }
    }
    plain_inline(rest)
}
fn plain_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' | '`' | '~' => {}
            '[' | ']' => {
                if c == ']' && chars.peek() == Some(&'(') {
                    for skipped in chars.by_ref() {
                        if skipped == ')' {
                            break;
                        }
                    }
                }
            }
            '!' if chars.peek() == Some(&'[') => {}
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", cut.trim_end_matches([' ', '·']))
}
pub fn relative_time(at_ms: i64, now_ms: i64) -> String {
    if at_ms <= 0 {
        return String::new();
    }
    let minutes = (now_ms - at_ms).max(0) / 60_000;
    let day = |ms: i64| ms.div_euclid(86_400_000);
    let days = day(now_ms) - day(at_ms);
    match () {
        _ if minutes < 1 => "just now".into(),
        _ if minutes < 60 => format!("{minutes} min ago"),
        _ if minutes < 24 * 60 && days == 0 => format!("{} h ago", minutes / 60),
        _ if days <= 1 => "yesterday".into(),
        _ if days < 7 => format!("{days} days ago"),
        _ => {
            let (year, month, date) = civil_date(at_ms);
            let (now_year, _, _) = civil_date(now_ms);
            const MONTHS: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let month = MONTHS[(month - 1) as usize];
            if year == now_year {
                format!("{date} {month}")
            } else {
                format!("{date} {month} {year}")
            }
        }
    }
}
fn civil_date(ms: i64) -> (i64, u32, u32) {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Edited,
    Name,
    Folder,
}

impl Sort {
    const ALL: [(Sort, &'static str, &'static str); 3] = [
        (Sort::Edited, "edited", "Last edited"),
        (Sort::Name, "name", "Name"),
        (Sort::Folder, "folder", "Folder"),
    ];

    fn parse(value: &str) -> Self {
        Self::ALL
            .iter()
            .find(|(_, key, _)| *key == value)
            .map_or(Sort::Edited, |(sort, ..)| *sort)
    }
}

fn stored(key: &str) -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()
        .flatten()?
        .get_item(key)
        .ok()
        .flatten()
}

fn store(key: &str, value: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(key, value);
    }
}

fn list_view() -> bool {
    stored(VIEW_KEY).as_deref() == Some("list")
}

fn current_sort() -> Sort {
    Sort::parse(stored(SORT_KEY).as_deref().unwrap_or("edited"))
}
pub struct HomeNote<'a> {
    pub path: &'a str,
    pub title: String,
    pub folder: String,
    pub summary: Option<&'a NoteSummary>,
}

impl HomeNote<'_> {
    fn edited(&self) -> i64 {
        self.summary.map_or(0, |summary| summary.edited_at_ms)
    }
    fn haystack(&self) -> String {
        format!(
            "{} {} {}",
            self.title,
            self.path,
            self.summary.map_or("", |summary| summary.snippet.as_str())
        )
        .to_lowercase()
    }
}

pub fn sort_notes(notes: &mut [HomeNote<'_>], sort: Sort) {
    let name = |note: &HomeNote<'_>| note.title.to_lowercase();
    match sort {
        Sort::Edited => notes.sort_by(|a, b| {
            b.edited()
                .cmp(&a.edited())
                .then_with(|| name(a).cmp(&name(b)))
        }),
        Sort::Name => notes.sort_by_key(name),
        Sort::Folder => notes.sort_by(|a, b| {
            a.folder
                .to_lowercase()
                .cmp(&b.folder.to_lowercase())
                .then_with(|| name(a).cmp(&name(b)))
        }),
    }
}
pub fn display_title(title: &str, path: &str) -> String {
    let title = title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".md").unwrap_or(file).to_string()
}

fn folder_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(folder, _)| folder.replace('/', " / "))
        .unwrap_or_default()
}
fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[derive(Clone, PartialEq)]
pub(crate) struct Card {
    path: String,
    title: String,
    folder: String,
    filter: String,
    edited: String,
    snippet: String,
    ink: bool,
    done: usize,
    total: usize,
    open: Vec<String>,
    words: Option<usize>,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Home {
    name: String,
    stats: String,
    can_edit: bool,
    sort: Sort,
    list: bool,
    tags: Vec<(String, usize)>,
    tag: Option<String>,
    recent: Vec<Card>,
    notes: Vec<Card>,
}

pub(crate) fn snapshot(inner: &DriverInner, tag: Option<&str>) -> Home {
    let now = js_sys::Date::now() as i64;
    let tagged = tag.and_then(|tag| {
        inner
            .knowledge
            .tag_to_paths
            .get(tag)
            .map(|paths| (tag, paths))
    });
    let mut notes: Vec<HomeNote<'_>> = inner
        .search_documents
        .iter()
        .filter(|doc| tagged.is_none_or(|(_, paths)| paths.contains(&doc.path)))
        .map(|doc| HomeNote {
            path: &doc.path,
            title: display_title(&doc.title, &doc.path),
            folder: folder_of(&doc.path),
            summary: inner.note_summaries.get(&doc.path),
        })
        .collect();
    let sort = current_sort();
    sort_notes(&mut notes, sort);
    let card = |note: &HomeNote<'_>| Card {
        path: note.path.to_string(),
        title: note.title.clone(),
        folder: note.folder.clone(),
        filter: note.haystack(),
        edited: relative_time(note.edited(), now),
        snippet: note
            .summary
            .map(|summary| summary.snippet.clone())
            .unwrap_or_default(),
        ink: note.summary.is_some_and(|summary| summary.has_ink),
        done: note.summary.map_or(0, |summary| summary.done_tasks),
        total: note.summary.map_or(0, NoteSummary::total_tasks),
        open: note
            .summary
            .map(|summary| summary.open_tasks.clone())
            .unwrap_or_default(),
        words: note.summary.map(|summary| summary.words),
    };
    let folders = inner
        .entries
        .iter()
        .filter(|entry| entry.kind == inkstone_workspace::WorkspaceEntryKind::Folder)
        .count();
    let summaries = inner.note_summaries.values();
    let open_tasks: usize = summaries
        .clone()
        .map(|summary| summary.open_tasks.len())
        .sum();
    let inked = summaries.filter(|summary| summary.has_ink).count();
    let mut stats = vec![plural(inner.search_documents.len(), "note", "notes")];
    if folders > 0 {
        stats.push(plural(folders, "folder", "folders"));
    }
    if open_tasks > 0 {
        stats.push(plural(open_tasks, "open task", "open tasks"));
    }
    if inked > 0 {
        stats.push(format!("{inked} with ink"));
    }
    let list = list_view();
    Home {
        name: inner.workspace_name.clone(),
        stats: stats.join(" · "),
        can_edit: crate::can_edit_workspace(inner),
        sort,
        list,
        tags: inner
            .knowledge
            .get_all_tags_with_counts()
            .into_iter()
            .take(16)
            .collect(),
        tag: tagged.map(|(tag, _)| tag.to_string()),
        recent: if tagged.is_some() || list {
            Vec::new()
        } else {
            inner
                .recent_notes
                .iter()
                .filter_map(|path| notes.iter().find(|note| note.path == path))
                .take(RECENT_SHOWN)
                .map(card)
                .collect()
        },
        notes: notes.iter().map(card).collect(),
    }
}

fn open(path: &str) -> impl Fn(web_sys::MouseEvent) + 'static {
    let path = path.to_string();
    move |_| crate::ui::driver().select_entry(&path)
}

pub(crate) fn view(ui: Ui) -> impl IntoView {
    move || ui.home.get().map(|home| page(ui, home))
}

fn page(ui: Ui, home: Home) -> AnyView {
    let head = view! {
        <header class="home-head">
            <div class="home-heading">
                <h1 class="home-title">{home.name.clone()}</h1>
                <p class="home-stats">{home.stats.clone()}</p>
            </div>
            {home.can_edit.then(|| view! {
                <button
                    class="home-new-btn"
                    on:click=|_| {
                        let driver = crate::ui::driver();
                        if driver.0.borrow().client_state.has_pending_intent() {
                            return;
                        }
                        state().create.set(Some(Create::default()));
                        driver.0.borrow_mut().sidebar_open = true;
                        driver.render();
                    }
                >
                    {svg("plus", "icon")}
                    <span>"New note"</span>
                </button>
            })}
        </header>
    };
    if home.notes.is_empty() && home.tag.is_none() {
        return view! {
            <section class="home" aria-label="Home">
                {head}
                <div class="home-empty">
                    <h2>"Nothing here yet"</h2>
                    <p>"Start a note to write, sketch, or plan. Everything you make shows up here."</p>
                </div>
            </section>
        }
        .into_any();
    }
    let query = Memo::new(move |_| ui.home_filter.with(|query| query.trim().to_lowercase()));
    let shown = move |text: &str| {
        query.with(|query| query.split_whitespace().all(|word| text.contains(word)))
    };
    let filters = home
        .notes
        .iter()
        .map(|note| note.filter.clone())
        .collect::<Vec<_>>();
    let count = Memo::new(move |_| filters.iter().filter(|text| shown(text)).count());
    let sorts = Sort::ALL.map(|(option, key, label)| {
        view! {
            <button
                class="home-seg"
                data-home-sort=key
                aria-pressed=crate::ui::bool_str(option == home.sort)
                on:click=move |_| {
                    store(SORT_KEY, key);
                    refresh();
                }
            >
                {label}
            </button>
        }
    });
    let views = [("cards", "grid", "Cards"), ("list", "list", "List")].map(|(key, icon, label)| {
        view! {
            <button
                class="home-seg home-seg-icon"
                data-home-view=key
                aria-label=label
                title=label
                aria-pressed=crate::ui::bool_str((key == "list") == home.list)
                on:click=move |_| {
                    store(VIEW_KEY, key);
                    refresh();
                }
            >
                {svg(icon, "icon")}
            </button>
        }
    });
    let pick = move |tag: Option<String>| {
        move |_: web_sys::MouseEvent| {
            ui.home_tag.set(tag.clone());
            refresh();
        }
    };
    let tags = (!home.tags.is_empty()).then(|| {
        let chips = home.tags.iter().map(|(tag, count)| {
            view! {
                <button
                    class="home-tag"
                    data-home-tag=tag.clone()
                    aria-pressed=crate::ui::bool_str(home.tag.as_ref() == Some(tag))
                    on:click=pick(Some(tag.clone()))
                >
                    <span>{format!("#{tag}")}</span>
                    <span class="home-tag-count">{count.to_string()}</span>
                </button>
            }
        });
        view! {
            <div class="home-tags">
                <button
                    class="home-tag"
                    data-home-tag=""
                    aria-pressed=crate::ui::bool_str(home.tag.is_none())
                    on:click=pick(None)
                >
                    "All"
                </button>
                {chips.collect_view()}
            </div>
        }
    });
    let recent = (!home.recent.is_empty()).then(|| {
        let items = home.recent.iter().map(|note| {
            view! {
                <button class="home-recent-item" data-path=note.path.clone() on:click=open(&note.path)>
                    {svg("file", "icon")}
                    <span class="home-recent-text">
                        <span class="home-recent-title">{note.title.clone()}</span>
                        <span class="home-recent-meta">{note.edited.clone()}</span>
                    </span>
                </button>
            }
        });
        view! {
            <section class="home-section home-recent-section" hidden=move || !query.with(String::is_empty)>
                <h2 class="home-section-title">"Jump back in"</h2>
                <div class="home-recent">{items.collect_view()}</div>
            </section>
        }
    });
    let notes = if home.list {
        table(&home.notes, shown).into_any()
    } else {
        grid(&home.notes, shown).into_any()
    };
    let tasks = home
        .notes
        .iter()
        .flat_map(|note| note.open.iter().map(move |task| (note, task)))
        .collect::<Vec<_>>();
    let tasks = (!tasks.is_empty()).then(|| {
        let expanded = RwSignal::new(false);
        let total = tasks.len();
        let items = tasks.into_iter().enumerate().map(|(index, (note, task))| {
            let filter = format!("{} {}", task.to_lowercase(), note.filter);
            view! {
                <li data-overflow=(index >= TASKS_SHOWN).then_some("") hidden=move || !shown(&filter)>
                    <button class="home-task" data-path=note.path.clone() on:click=open(&note.path)>
                        <span class="home-task-box"></span>
                        <span class="home-task-text">{task.clone()}</span>
                        <span class="home-task-note">{note.title.clone()}</span>
                    </button>
                </li>
            }
        });
        view! {
            <section class=move || if expanded.get() { "home-section home-tasks-expanded" } else { "home-section" }>
                <h2 class="home-section-title">
                    "Open tasks "
                    <span class="home-all-count-static">{total.to_string()}</span>
                </h2>
                <ul class="home-tasks">{items.collect_view()}</ul>
                {move || (total > TASKS_SHOWN && !expanded.get()).then(|| view! {
                    <button class="home-tasks-more" on:click=move |_| expanded.set(true)>
                        {format!("Show all {total}")}
                    </button>
                })}
            </section>
        }
    });
    view! {
        <section class="home" aria-label="Home">
            {head}
            <div class="home-toolbar">
                <label class="home-filter-wrap">
                    {svg("search", "icon")}
                    <input
                        type="search"
                        class="home-filter"
                        placeholder="Filter notes"
                        aria-label="Filter notes"
                        data-focus-key="home-filter"
                        prop:value=move || ui.home_filter.get()
                        on:input=move |event| ui.home_filter.set(event_target_value(&event))
                    />
                </label>
                <div class="home-controls">
                    <div class="home-segmented" role="group" aria-label="Sort by">
                        {sorts.into_iter().collect_view()}
                    </div>
                    <div class="home-segmented" role="group" aria-label="Show as">
                        {views.into_iter().collect_view()}
                    </div>
                </div>
            </div>
            {tags}
            {recent}
            <section class="home-section">
                <h2 class="home-section-title">
                    {match &home.tag {
                        Some(tag) => format!("Tagged #{tag} "),
                        None => "All notes ".to_string(),
                    }}
                    <span class="home-all-count">{move || count.get().to_string()}</span>
                </h2>
                {notes}
                <p class="home-no-match" hidden={move || count.get() > 0}>
                    "No notes match that filter."
                </p>
            </section>
            {tasks}
        </section>
    }
    .into_any()
}

fn table(
    notes: &[Card],
    shown: impl Fn(&str) -> bool + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let rows = notes.iter().map(|note| {
        let filter = note.filter.clone();
        let total = note.total;
        view! {
            <tr class="home-row" data-path=note.path.clone() data-note="" hidden=move || !shown(&filter)>
                <td>
                    <div class="home-row-name">
                        <button class="home-open" data-path=note.path.clone() on:click=open(&note.path)>
                            {note.title.clone()}
                        </button>
                        {note.ink.then(|| view! { <span class="home-badge">"Ink"</span> })}
                    </div>
                </td>
                <td class="home-col-folder">{note.folder.clone()}</td>
                <td class="home-col-edited">{note.edited.clone()}</td>
                <td class="home-col-num">
                    {(total > 0).then(|| format!("{}/{total}", note.done))}
                </td>
                <td class="home-col-num home-col-words">{note.words.map(|words| words.to_string())}</td>
            </tr>
        }
    });
    let heads = [
        ("Name", ""),
        ("Folder", "home-col-folder"),
        ("Edited", ""),
        ("Tasks", "home-col-num"),
        ("Words", "home-col-num home-col-words"),
    ]
    .map(|(label, class)| view! { <th class=class scope="col">{label}</th> });
    view! {
        <div class="home-table-wrap">
            <table class="home-table">
                <thead>
                    <tr>{heads.into_iter().collect_view()}</tr>
                </thead>
                <tbody>{rows.collect_view()}</tbody>
            </table>
        </div>
    }
}

fn grid(
    notes: &[Card],
    shown: impl Fn(&str) -> bool + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let cards = notes.iter().map(|note| {
        let filter = note.filter.clone();
        let total = note.total;
        let blank = note.snippet.is_empty();
        view! {
            <button
                class="home-card"
                data-path=note.path.clone()
                data-note=""
                hidden=move || !shown(&filter)
                on:click=open(&note.path)
            >
                {(!note.folder.is_empty()).then(|| view! { <span class="home-card-folder">{note.folder.clone()}</span> })}
                <span class="home-card-title">{note.title.clone()}</span>
                <span class=if blank { "home-card-snippet home-card-blank" } else { "home-card-snippet" }>
                    {if blank { "Empty page".to_string() } else { note.snippet.clone() }}
                </span>
                <span class="home-card-meta">
                    <span>{note.edited.clone()}</span>
                    {note.ink.then(|| view! { <span class="home-badge">"Ink"</span> })}
                    {(total > 0).then(|| view! {
                        <span class="home-badge" data-done=note.open.is_empty().then_some("true")>
                            {format!("{}/{total} done", note.done)}
                        </span>
                    })}
                </span>
            </button>
        }
    });
    view! { <div class="home-grid">{cards.collect_view()}</div> }
}

#[cfg(test)]
mod tests {
    use super::*;
    use inkstone_core::document::DocumentBlock;
    use inkstone_core::ids::{BlockId, DocId};

    fn doc(texts: &[&str]) -> InkstoneDocument {
        let mut doc = InkstoneDocument::new(DocId(uuid::Uuid::from_u128(1)));
        for (index, text) in texts.iter().enumerate() {
            let mut block = DocumentBlock::new(
                BlockId(uuid::Uuid::from_u128(index as u128 + 10)),
                BlockContent::Markdown {
                    text: text.to_string(),
                },
            );
            block.updated_at_ms = 1_000 + index as i64;
            doc.blocks.push(block);
        }
        doc
    }

    #[test]
    fn summaries_count_tasks_words_and_read_like_prose() {
        let summary = summarize(
            &doc(&[
                "# Trip **plan**",
                "- [ ] book [train](https://x.test)\n- [x] pack\n> - [ ] quoted [[Visa]]\n1. [ ] numbered\n- plain item",
            ]),
            0,
        );
        assert_eq!(
            summary.open_tasks,
            vec!["book train", "quoted Visa", "numbered"]
        );
        assert_eq!(summary.done_tasks, 1);
        assert_eq!(summary.edited_at_ms, 1_001);
        assert!(summary.snippet.starts_with("Trip plan · book train · pack"));
        assert!(!summary.has_ink);
        assert!(summary.words >= 9);
    }

    #[test]
    fn later_edit_time_wins_and_long_text_is_cut_on_a_word() {
        let long = "word ".repeat(80);
        let summary = summarize(&doc(&[&long]), 5_000);
        assert_eq!(summary.edited_at_ms, 5_000);
        assert!(summary.snippet.ends_with('…'));
        assert!(summary.snippet.chars().count() <= SNIPPET_CHARS + 1);
        assert!(!summary.snippet.contains("wor…"));
    }

    #[test]
    fn code_and_tables_stay_out_of_previews() {
        let summary = summarize(&doc(&["```rust", "| a | b |", "---", "Real text"]), 0);
        assert_eq!(summary.snippet, "Real text");
    }

    #[test]
    fn times_read_naturally() {
        let now = 1_791_931_117_283;
        assert_eq!(relative_time(now - 10_000, now), "just now");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5 min ago");
        assert_eq!(relative_time(now - 3 * 86_400_000, now), "3 days ago");
        assert_eq!(relative_time(now - 30 * 86_400_000, now), "13 Sep");
        assert_eq!(relative_time(now - 400 * 86_400_000, now), "8 Sep 2025");
        assert_eq!(relative_time(0, now), "");
    }

    #[test]
    fn notes_sort_by_edit_name_and_folder() {
        let summary = |at| NoteSummary {
            edited_at_ms: at,
            ..NoteSummary::default()
        };
        let (a, b, c) = (summary(3), summary(1), summary(2));
        let mut notes = vec![
            HomeNote {
                path: "b/Zeta.md",
                title: "Zeta".into(),
                folder: "b".into(),
                summary: Some(&a),
            },
            HomeNote {
                path: "Alpha.md",
                title: "alpha".into(),
                folder: String::new(),
                summary: Some(&b),
            },
            HomeNote {
                path: "a/Mid.md",
                title: "Mid".into(),
                folder: "a".into(),
                summary: Some(&c),
            },
        ];
        let order =
            |notes: &[HomeNote<'_>]| notes.iter().map(|n| n.path.to_string()).collect::<Vec<_>>();
        sort_notes(&mut notes, Sort::Edited);
        assert_eq!(order(&notes), ["b/Zeta.md", "a/Mid.md", "Alpha.md"]);
        sort_notes(&mut notes, Sort::Name);
        assert_eq!(order(&notes), ["Alpha.md", "a/Mid.md", "b/Zeta.md"]);
        sort_notes(&mut notes, Sort::Folder);
        assert_eq!(order(&notes), ["Alpha.md", "a/Mid.md", "b/Zeta.md"]);
        assert_eq!(Sort::parse("folder"), Sort::Folder);
        assert_eq!(Sort::parse("bogus"), Sort::Edited);
    }

    #[test]
    fn untitled_notes_use_their_file_name() {
        assert_eq!(display_title("  ", "Work/Ideas.md"), "Ideas");
        assert_eq!(display_title("Plan", "Work/Ideas.md"), "Plan");
    }
}
