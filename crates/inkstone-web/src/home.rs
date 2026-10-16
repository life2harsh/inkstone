use inkstone_core::document::{BlockContent, InkstoneDocument};

use wasm_bindgen::JsCast;

use crate::{Driver, icons};

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

pub fn store(key: &str, value: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(key, value);
    }
}

pub fn set_view(value: &str) {
    store(VIEW_KEY, value);
}

pub fn set_sort(value: &str) {
    store(SORT_KEY, value);
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
pub fn apply_filter(home: &web_sys::Element, query: &str) {
    let query = query.trim().to_lowercase();
    let Ok(items) = home.query_selector_all("[data-filter]") else {
        return;
    };
    let mut shown = 0;
    for index in 0..items.length() {
        let Some(item) = items
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let matches = query.is_empty()
            || item
                .get_attribute("data-filter")
                .is_some_and(|text| query.split_whitespace().all(|word| text.contains(word)));
        if matches {
            let _ = item.remove_attribute("hidden");
            if item.has_attribute("data-note") {
                shown += 1;
            }
        } else {
            let _ = item.set_attribute("hidden", "");
        }
    }
    if let Ok(Some(count)) = home.query_selector(".home-all-count") {
        count.set_text_content(Some(&shown.to_string()));
    }
    if let Ok(Some(none)) = home.query_selector(".home-no-match") {
        if shown == 0 {
            let _ = none.remove_attribute("hidden");
        } else {
            let _ = none.set_attribute("hidden", "");
        }
    }
    if let Ok(Some(recent)) = home.query_selector(".home-recent-section") {
        if query.is_empty() {
            let _ = recent.remove_attribute("hidden");
        } else {
            let _ = recent.set_attribute("hidden", "");
        }
    }
}

fn el(document: &web_sys::Document, tag: &str, class: &str) -> web_sys::Element {
    let element = document.create_element(tag).unwrap();
    if !class.is_empty() {
        element.set_class_name(class);
    }
    element
}

fn text(document: &web_sys::Document, tag: &str, class: &str, value: &str) -> web_sys::Element {
    let element = el(document, tag, class);
    element.set_text_content(Some(value));
    element
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl Driver {
    pub(crate) fn render_home(&self, document: &web_sys::Document) -> web_sys::Element {
        let inner = self.0.borrow();
        let now = js_sys::Date::now() as i64;
        let home = el(document, "section", "home");
        home.set_attribute("aria-label", "Home").unwrap();

        let tagged = inner.home_tag.as_ref().and_then(|tag| {
            inner
                .knowledge
                .tag_to_paths
                .get(tag)
                .map(|paths| (tag.clone(), paths))
        });
        let mut notes: Vec<HomeNote<'_>> = inner
            .search_documents
            .iter()
            .filter(|doc| {
                tagged
                    .as_ref()
                    .is_none_or(|(_, paths)| paths.contains(&doc.path))
            })
            .map(|doc| HomeNote {
                path: &doc.path,
                title: display_title(&doc.title, &doc.path),
                folder: folder_of(&doc.path),
                summary: inner.note_summaries.get(&doc.path),
            })
            .collect();
        let sort = current_sort();
        sort_notes(&mut notes, sort);
        let list = list_view();
        let head = el(document, "header", "home-head");
        let heading = el(document, "div", "home-heading");
        heading
            .append_child(&text(document, "h1", "home-title", &inner.workspace_name))
            .unwrap();
        let folders = inner
            .entries
            .iter()
            .filter(|entry| entry.kind == inkstone_workspace::WorkspaceEntryKind::Folder)
            .count();
        let open_tasks: usize = inner
            .note_summaries
            .values()
            .map(|summary| summary.open_tasks.len())
            .sum();
        let inked = inner
            .note_summaries
            .values()
            .filter(|summary| summary.has_ink)
            .count();
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
        heading
            .append_child(&text(document, "p", "home-stats", &stats.join(" · ")))
            .unwrap();
        head.append_child(&heading).unwrap();
        if crate::can_edit_workspace(&inner) {
            let new = el(document, "button", "home-new-btn");
            new.append_child(&icons::icon(document, "plus")).unwrap();
            new.append_child(&text(document, "span", "", "New note"))
                .unwrap();
            head.append_child(&new).unwrap();
        }
        home.append_child(&head).unwrap();

        if inner.search_documents.is_empty() {
            let empty = el(document, "div", "home-empty");
            empty
                .append_child(&text(document, "h2", "", "Nothing here yet"))
                .unwrap();
            empty
                .append_child(&text(
                    document,
                    "p",
                    "",
                    "Start a note to write, sketch, or plan. Everything you make shows up here.",
                ))
                .unwrap();
            home.append_child(&empty).unwrap();
            return home;
        }
        let toolbar = el(document, "div", "home-toolbar");
        let filter_wrap = el(document, "label", "home-filter-wrap");
        filter_wrap
            .append_child(&icons::icon(document, "search"))
            .unwrap();
        let filter = el(document, "input", "home-filter");
        filter.set_attribute("type", "search").unwrap();
        filter.set_attribute("placeholder", "Filter notes").unwrap();
        filter.set_attribute("aria-label", "Filter notes").unwrap();
        filter
            .set_attribute("data-focus-key", "home-filter")
            .unwrap();
        filter
            .unchecked_ref::<web_sys::HtmlInputElement>()
            .set_value(&inner.home_filter);
        filter_wrap.append_child(&filter).unwrap();
        toolbar.append_child(&filter_wrap).unwrap();

        let controls = el(document, "div", "home-controls");
        let sort_group = el(document, "div", "home-segmented");
        sort_group.set_attribute("role", "group").unwrap();
        sort_group.set_attribute("aria-label", "Sort by").unwrap();
        for (option, key, label) in Sort::ALL {
            let button = text(document, "button", "home-seg", label);
            button.set_attribute("data-home-sort", key).unwrap();
            button
                .set_attribute(
                    "aria-pressed",
                    if option == sort { "true" } else { "false" },
                )
                .unwrap();
            sort_group.append_child(&button).unwrap();
        }
        controls.append_child(&sort_group).unwrap();
        let view_group = el(document, "div", "home-segmented");
        view_group.set_attribute("role", "group").unwrap();
        view_group.set_attribute("aria-label", "Show as").unwrap();
        for (key, icon, label) in [("cards", "grid", "Cards"), ("list", "list", "List")] {
            let button = el(document, "button", "home-seg home-seg-icon");
            button.append_child(&icons::icon(document, icon)).unwrap();
            button.set_attribute("data-home-view", key).unwrap();
            button.set_attribute("aria-label", label).unwrap();
            button.set_attribute("title", label).unwrap();
            button
                .set_attribute(
                    "aria-pressed",
                    if (key == "list") == list {
                        "true"
                    } else {
                        "false"
                    },
                )
                .unwrap();
            view_group.append_child(&button).unwrap();
        }
        controls.append_child(&view_group).unwrap();
        toolbar.append_child(&controls).unwrap();
        home.append_child(&toolbar).unwrap();

        let tags = inner.knowledge.get_all_tags_with_counts();
        if !tags.is_empty() {
            let chips = el(document, "div", "home-tags");
            let all = text(document, "button", "home-tag", "All");
            all.set_attribute("data-home-tag", "").unwrap();
            all.set_attribute(
                "aria-pressed",
                if inner.home_tag.is_none() {
                    "true"
                } else {
                    "false"
                },
            )
            .unwrap();
            chips.append_child(&all).unwrap();
            for (tag, count) in tags.iter().take(16) {
                let chip = el(document, "button", "home-tag");
                chip.append_child(&text(document, "span", "", &format!("#{tag}")))
                    .unwrap();
                chip.append_child(&text(
                    document,
                    "span",
                    "home-tag-count",
                    &count.to_string(),
                ))
                .unwrap();
                chip.set_attribute("data-home-tag", tag).unwrap();
                chip.set_attribute(
                    "aria-pressed",
                    if inner.home_tag.as_deref() == Some(tag.as_str()) {
                        "true"
                    } else {
                        "false"
                    },
                )
                .unwrap();
                chips.append_child(&chip).unwrap();
            }
            home.append_child(&chips).unwrap();
        }
        let recent: Vec<&HomeNote<'_>> = inner
            .recent_notes
            .iter()
            .filter_map(|path| notes.iter().find(|note| note.path == path))
            .take(RECENT_SHOWN)
            .collect();
        if !recent.is_empty() && tagged.is_none() && !list {
            let section = el(document, "section", "home-section home-recent-section");
            section
                .append_child(&text(document, "h2", "home-section-title", "Jump back in"))
                .unwrap();
            let row = el(document, "div", "home-recent");
            for note in recent {
                let item = el(document, "button", "home-recent-item");
                item.set_attribute("data-path", note.path).unwrap();
                item.append_child(&icons::icon(document, "file")).unwrap();
                let words = el(document, "span", "home-recent-text");
                words
                    .append_child(&text(document, "span", "home-recent-title", &note.title))
                    .unwrap();
                words
                    .append_child(&text(
                        document,
                        "span",
                        "home-recent-meta",
                        &relative_time(note.edited(), now),
                    ))
                    .unwrap();
                item.append_child(&words).unwrap();
                row.append_child(&item).unwrap();
            }
            section.append_child(&row).unwrap();
            home.append_child(&section).unwrap();
        }
        let section = el(document, "section", "home-section");
        let title = el(document, "h2", "home-section-title");
        title
            .append_child(&document.create_text_node(&match &tagged {
                Some((tag, _)) => format!("Tagged #{tag} "),
                None => "All notes ".to_string(),
            }))
            .unwrap();
        title
            .append_child(&text(
                document,
                "span",
                "home-all-count",
                &notes.len().to_string(),
            ))
            .unwrap();
        section.append_child(&title).unwrap();
        if list {
            let table = el(document, "table", "home-table");
            let head_row = el(document, "tr", "");
            for (label, class) in [
                ("Name", ""),
                ("Folder", "home-col-folder"),
                ("Edited", ""),
                ("Tasks", "home-col-num"),
                ("Words", "home-col-num home-col-words"),
            ] {
                let cell = text(document, "th", class, label);
                cell.set_attribute("scope", "col").unwrap();
                head_row.append_child(&cell).unwrap();
            }
            let thead = el(document, "thead", "");
            thead.append_child(&head_row).unwrap();
            table.append_child(&thead).unwrap();
            let body = el(document, "tbody", "");
            for note in &notes {
                let row = el(document, "tr", "home-row");
                row.set_attribute("data-path", note.path).unwrap();
                row.set_attribute("data-filter", &note.haystack()).unwrap();
                row.set_attribute("data-note", "").unwrap();
                let cell = el(document, "td", "");
                let name = el(document, "div", "home-row-name");
                let open = text(document, "button", "home-open", &note.title);
                open.set_attribute("data-path", note.path).unwrap();
                name.append_child(&open).unwrap();
                if note.summary.is_some_and(|summary| summary.has_ink) {
                    name.append_child(&text(document, "span", "home-badge", "Ink"))
                        .unwrap();
                }
                cell.append_child(&name).unwrap();
                row.append_child(&cell).unwrap();
                row.append_child(&text(document, "td", "home-col-folder", &note.folder))
                    .unwrap();
                row.append_child(&text(
                    document,
                    "td",
                    "home-col-edited",
                    &relative_time(note.edited(), now),
                ))
                .unwrap();
                let tasks = note
                    .summary
                    .filter(|summary| summary.total_tasks() > 0)
                    .map(|summary| format!("{}/{}", summary.done_tasks, summary.total_tasks()))
                    .unwrap_or_default();
                row.append_child(&text(document, "td", "home-col-num", &tasks))
                    .unwrap();
                row.append_child(&text(
                    document,
                    "td",
                    "home-col-num home-col-words",
                    &note
                        .summary
                        .map(|summary| summary.words.to_string())
                        .unwrap_or_default(),
                ))
                .unwrap();
                body.append_child(&row).unwrap();
            }
            table.append_child(&body).unwrap();
            let scroller = el(document, "div", "home-table-wrap");
            scroller.append_child(&table).unwrap();
            section.append_child(&scroller).unwrap();
        } else {
            let grid = el(document, "div", "home-grid");
            for note in &notes {
                let card = el(document, "button", "home-card");
                card.set_attribute("data-path", note.path).unwrap();
                card.set_attribute("data-filter", &note.haystack()).unwrap();
                card.set_attribute("data-note", "").unwrap();
                if !note.folder.is_empty() {
                    card.append_child(&text(document, "span", "home-card-folder", &note.folder))
                        .unwrap();
                }
                card.append_child(&text(document, "span", "home-card-title", &note.title))
                    .unwrap();
                let snippet = note
                    .summary
                    .map(|summary| summary.snippet.as_str())
                    .unwrap_or("");
                card.append_child(&text(
                    document,
                    "span",
                    if snippet.is_empty() {
                        "home-card-snippet home-card-blank"
                    } else {
                        "home-card-snippet"
                    },
                    if snippet.is_empty() {
                        "Empty page"
                    } else {
                        snippet
                    },
                ))
                .unwrap();
                let meta = el(document, "span", "home-card-meta");
                meta.append_child(&text(
                    document,
                    "span",
                    "",
                    &relative_time(note.edited(), now),
                ))
                .unwrap();
                if let Some(summary) = note.summary {
                    if summary.has_ink {
                        meta.append_child(&text(document, "span", "home-badge", "Ink"))
                            .unwrap();
                    }
                    if summary.total_tasks() > 0 {
                        let badge = text(
                            document,
                            "span",
                            "home-badge",
                            &format!("{}/{} done", summary.done_tasks, summary.total_tasks()),
                        );
                        if summary.open_tasks.is_empty() {
                            badge.set_attribute("data-done", "true").unwrap();
                        }
                        meta.append_child(&badge).unwrap();
                    }
                }
                card.append_child(&meta).unwrap();
                grid.append_child(&card).unwrap();
            }
            section.append_child(&grid).unwrap();
        }
        let none = text(
            document,
            "p",
            "home-no-match",
            "No notes match that filter.",
        );
        none.set_attribute("hidden", "").unwrap();
        section.append_child(&none).unwrap();
        home.append_child(&section).unwrap();
        let open: Vec<(&HomeNote<'_>, &String)> = notes
            .iter()
            .filter_map(|note| note.summary.map(|summary| (note, summary)))
            .flat_map(|(note, summary)| summary.open_tasks.iter().map(move |task| (note, task)))
            .collect();
        if !open.is_empty() {
            let section = el(document, "section", "home-section");
            let title = el(document, "h2", "home-section-title");
            title
                .append_child(&document.create_text_node("Open tasks "))
                .unwrap();
            title
                .append_child(&text(
                    document,
                    "span",
                    "home-all-count-static",
                    &open.len().to_string(),
                ))
                .unwrap();
            section.append_child(&title).unwrap();
            let list = el(document, "ul", "home-tasks");
            for (index, (note, task)) in open.iter().enumerate() {
                let item = el(document, "li", "");
                item.set_attribute(
                    "data-filter",
                    &format!("{} {}", task.to_lowercase(), note.haystack()),
                )
                .unwrap();
                if index >= TASKS_SHOWN {
                    item.set_attribute("data-overflow", "").unwrap();
                }
                let button = el(document, "button", "home-task");
                button.set_attribute("data-path", note.path).unwrap();
                button
                    .append_child(&el(document, "span", "home-task-box"))
                    .unwrap();
                button
                    .append_child(&text(document, "span", "home-task-text", task))
                    .unwrap();
                button
                    .append_child(&text(document, "span", "home-task-note", &note.title))
                    .unwrap();
                item.append_child(&button).unwrap();
                list.append_child(&item).unwrap();
            }
            section.append_child(&list).unwrap();
            if open.len() > TASKS_SHOWN {
                let more = text(
                    document,
                    "button",
                    "home-tasks-more",
                    &format!("Show all {}", open.len()),
                );
                section.append_child(&more).unwrap();
            }
            home.append_child(&section).unwrap();
        }
        if !inner.home_filter.trim().is_empty() {
            apply_filter(&home, &inner.home_filter);
        }
        home
    }
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
