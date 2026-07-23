#[derive(Clone)]
pub struct PendingSelection {
    pub path: String,
    pub doc_id: DocId,
    pub revision: u64,
}

#[derive(Clone)]
pub struct MissingNoteRequest {
    pub raw_target: String,
    pub proposed_path: String,
    pub source_path: String,
    pub resolution: knowledge::WikilinkResolution,
}

pub mod explorer;
pub mod indexed_db;
pub mod ink_runtime;
pub mod knowledge;
pub mod local_graph;
pub mod markdown_render;
pub mod math_render;
pub mod search;
pub mod storage;
pub mod workspace_db;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use inkstone_client::{ClientEvent, ClientState, Effect, SessionPhase};
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::document::{BlockContent, InkstoneDocument};
use inkstone_core::ids::{BlockId, DocId, UpdateId};
use inkstone_workspace::{WorkspaceEntry, WorkspaceEntryKind, WorkspacePath, WorkspaceStore};

use indexed_db::IndexedDb;
use workspace_db::WorkspaceDb;

thread_local! {
    // Replacing the DOM blurs active editors. Ignore that synthetic focusout while
    // render holds the driver borrow; user-initiated focus changes still render.
    static RENDERING: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentMode {
    Edit,
    Preview,
}

#[derive(Clone)]
pub struct DriverInner {
    pub client_state: ClientState,
    pub db: Option<IndexedDb>,
    pub workspace_db: Option<WorkspaceDb>,
    pub persistence_status: String,
    pub storage_error: Option<String>,
    pub selected_doc_id: DocId,
    pub entries: Vec<WorkspaceEntry>,
    pub selected_path: Option<String>,
    pub workspace_loaded: bool,
    pub expanded_folders: HashSet<String>,
    pub renaming_path: Option<String>,
    pub rename_value: String,
    pub show_create: bool,
    pub create_value: String,
    pub create_folder: Option<String>,
    pub document_mode: DocumentMode,
    pub link_message: Option<String>,
    pub tag_filter: Option<String>,
    pub search_query: String,
    pub search_indexing: bool,
    pub search_documents: Vec<search::SearchDocument>,
    pub search_results: Vec<search::SearchResult>,
    pub search_highlighted_index: Option<usize>,
    pub recent_notes: Vec<String>,
    pub knowledge: knowledge::KnowledgeIndex,
    pub knowledge_tab: String,
    pub inspector_open: bool,
    pub selection_revision: u64,
    pub pending_selection: Option<PendingSelection>,
    pub recent_notes_revision: u64,
    pub missing_note: Option<MissingNoteRequest>,
    pub ink_tool: String,
    pub ink_brush_width: f32,
    pub ink_brush_color: (u8, u8, u8),
    pub expanded_ink_block: Option<BlockId>,
    pub sidebar_open: bool,
}

#[derive(Clone)]
pub struct Driver(
    Rc<RefCell<DriverInner>>,
    Rc<RefCell<ink_runtime::InkRuntimeRegistry>>,
);

impl Driver {
    pub fn new(doc_id: DocId) -> Self {
        Driver(
            Rc::new(RefCell::new(DriverInner {
                client_state: ClientState::new(doc_id),
                db: None,
                workspace_db: None,
                persistence_status: "Initializing...".to_string(),
                storage_error: None,
                selected_doc_id: doc_id,
                entries: Vec::new(),
                selected_path: None,
                workspace_loaded: false,
                expanded_folders: HashSet::new(),
                renaming_path: None,
                rename_value: String::new(),
                show_create: false,
                create_value: String::new(),
                create_folder: None,
                document_mode: DocumentMode::Edit,
                link_message: None,
                tag_filter: None,
                search_query: String::new(),
                search_indexing: false,
                search_documents: Vec::new(),
                search_results: Vec::new(),
                search_highlighted_index: None,
                recent_notes: Vec::new(),
                knowledge: knowledge::KnowledgeIndex::new(),
                knowledge_tab: "Outline".to_string(),
                inspector_open: false,
                selection_revision: 0,
                pending_selection: None,
                recent_notes_revision: 0,
                missing_note: None,
                ink_tool: "pen".to_string(),
                ink_brush_width: 3.0,
                ink_brush_color: (31, 41, 55),
                expanded_ink_block: None,
                sidebar_open: false,
            })),
            Rc::new(RefCell::new(ink_runtime::InkRuntimeRegistry::default())),
        )
    }

    fn set_ink_save_state(&self, block_id: BlockId, text: &str, state: &str) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let selector = format!(".ink-save-state[data-block-id=\"{}\"]", block_id);
        if let Ok(Some(element)) = document.query_selector(&selector) {
            element.set_text_content(Some(text));
            let _ = element.set_attribute("data-state", state);
        }
    }

    fn report_ink_error(&self, context: &str, error: JsValue) {
        let message = format!("{}: {:?}", context, error);
        self.0.borrow_mut().persistence_status = message.clone();
        web_sys::console::error_1(&JsValue::from_str(&message));
    }

    fn dispatch_ink_snapshot(&self, snapshot: ink_runtime::InkSnapshot) {
        let is_current = {
            let inner = self.0.borrow();
            inner.selected_doc_id == snapshot.doc_id
                && matches!(
                    inner
                        .client_state
                        .effective_view_doc()
                        .get_block(snapshot.block_id)
                        .map(|block| &block.content),
                    Some(BlockContent::Ink { .. })
                )
        };
        if !is_current {
            return;
        }

        self.set_ink_save_state(snapshot.block_id, "Saving drawing...", "saving");
        self.dispatch(ClientEvent::LocalIntent {
            op: DocumentOp::ReplaceBlockContent {
                block_id: snapshot.block_id,
                content: BlockContent::Ink {
                    stonepen_json: snapshot.stonepen_json,
                },
            },
            update_id: UpdateId::new_v4(),
            applied_at_ms: js_sys::Date::now() as i64,
        });
        self.set_ink_save_state(snapshot.block_id, "Saved locally", "saved");
    }

    fn save_ink_block(&self, block_id: BlockId) {
        let result = { self.1.borrow_mut().export_changed(block_id) };
        match result {
            Ok(Some(snapshot)) => self.dispatch_ink_snapshot(snapshot),
            Ok(None) => self.set_ink_save_state(block_id, "Saved locally", "saved"),
            Err(error) => self.report_ink_error("Failed to export Ink block", error),
        }
    }

    fn save_all_ink_blocks(&self) {
        let result = { self.1.borrow_mut().export_all_changed() };
        match result {
            Ok(snapshots) => {
                for snapshot in snapshots {
                    self.dispatch_ink_snapshot(snapshot);
                }
            }
            Err(error) => self.report_ink_error("Failed to export Ink blocks", error),
        }
    }

    fn undo_ink_block(&self, block_id: BlockId) {
        let result = { self.1.borrow_mut().undo(block_id) };
        match result {
            Ok(Some(snapshot)) => self.dispatch_ink_snapshot(snapshot),
            Ok(None) => {}
            Err(error) => self.report_ink_error("Failed to undo Ink block", error),
        }
    }

    fn redo_ink_block(&self, block_id: BlockId) {
        let result = { self.1.borrow_mut().redo(block_id) };
        match result {
            Ok(Some(snapshot)) => self.dispatch_ink_snapshot(snapshot),
            Ok(None) => {}
            Err(error) => self.report_ink_error("Failed to redo Ink block", error),
        }
    }

    fn flush_ink_runtimes(&self) {
        let result = { self.1.borrow_mut().destroy_all() };
        match result {
            Ok(snapshots) => {
                for snapshot in snapshots {
                    self.dispatch_ink_snapshot(snapshot);
                }
            }
            Err(error) => self.report_ink_error("Failed to flush Ink blocks", error),
        }
    }

    fn flush_ink_before_transition(&self) {
        let was_rendering = RENDERING.with(|rendering| {
            let was_rendering = rendering.get();
            rendering.set(true);
            was_rendering
        });
        self.flush_ink_runtimes();
        RENDERING.with(|rendering| rendering.set(was_rendering));
    }

    fn set_document_mode(&self, mode: DocumentMode) {
        if self.0.borrow().document_mode == mode {
            return;
        }
        self.flush_ink_before_transition();
        {
            let mut inner = self.0.borrow_mut();
            inner.document_mode = mode;
            inner.expanded_ink_block = None;
            inner.link_message = None;
        }
        self.render();
    }

    fn mount_ink_blocks(
        &self,
        document: &InkstoneDocument,
        tool: &str,
        brush_width: f32,
        brush_color: (u8, u8, u8),
        read_only: bool,
    ) {
        let Some(dom_document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };

        for block in &document.blocks {
            let BlockContent::Ink { stonepen_json } = &block.content else {
                continue;
            };
            let canvas_id = format!("ink-canvas-{}", block.id);
            if dom_document.get_element_by_id(&canvas_id).is_none() {
                continue;
            }

            let result = self.1.borrow_mut().mount(
                document.id,
                block.id,
                &canvas_id,
                stonepen_json,
                tool,
                brush_width,
                brush_color,
                read_only,
            );
            match result {
                Ok(Some(snapshot)) => self.dispatch_ink_snapshot(snapshot),
                Ok(None) if !read_only => {
                    self.set_ink_save_state(block.id, "Saved locally", "saved")
                }
                Ok(None) => {}
                Err(error) => self.report_ink_error("Failed to mount Ink block", error),
            }
        }
    }

    fn select_entry(&self, path: &str) {
        self.flush_ink_before_transition();
        let path_str = path.to_string();
        let is_markdown = path_str.to_lowercase().ends_with(".md")
            && !path_str.starts_with('.')
            && !path_str.contains("/.");

        let selection_rev = {
            let mut inner = self.0.borrow_mut();
            inner.selection_revision += 1;
            inner.renaming_path = None;
            inner.show_create = false;
            inner.create_folder = None;
            inner.sidebar_open = false;
            inner.inspector_open = false;
            let ancestors = markdown_render::ancestor_folders(&path_str);
            for folder in &ancestors {
                inner.expanded_folders.insert(folder.clone());
            }
            inner.selection_revision
        };

        let is_asset = {
            let inner = self.0.borrow();
            inner
                .entries
                .iter()
                .any(|e| e.path.as_str() == path_str && e.kind == WorkspaceEntryKind::Asset)
        };
        if is_asset {
            let mut inner = self.0.borrow_mut();
            inner.selected_path = Some(path_str);
            drop(inner);
            self.render();
            return;
        }
        let this = self.clone();
        let ws = self.0.borrow().workspace_db.clone();
        let ws = match ws {
            Some(w) => w,
            None => return,
        };

        wasm_bindgen_futures::spawn_local(async move {
            match ws.get_doc_id(&path_str).await {
                Ok(Some(existing_id)) => {
                    let is_current = { this.0.borrow().selection_revision == selection_rev };
                    if !is_current {
                        return;
                    }
                    if is_markdown {
                        let mut inner = this.0.borrow_mut();
                        inner.pending_selection = Some(PendingSelection {
                            path: path_str.clone(),
                            doc_id: DocId(existing_id),
                            revision: selection_rev,
                        });
                    }
                    let doc_id = DocId(existing_id);
                    {
                        let mut inner = this.0.borrow_mut();
                        inner.selected_doc_id = doc_id;
                        inner.selected_path = Some(path_str);
                        inner.client_state = ClientState::new(doc_id);
                    }
                    this.dispatch(ClientEvent::OpenDocument { doc_id });
                    this.render();
                }
                Ok(None) => {
                    let new_id = uuid::Uuid::new_v4();
                    let doc_id = DocId(new_id);
                    if let Err(e) = ws.set_doc_id(&path_str, new_id).await {
                        this.0.borrow_mut().storage_error =
                            Some(format!("Failed to save document mapping: {:?}", e));
                        this.render();
                        return;
                    }
                    let is_current = { this.0.borrow().selection_revision == selection_rev };
                    if !is_current {
                        return;
                    }
                    if is_markdown {
                        let mut inner = this.0.borrow_mut();
                        inner.pending_selection = Some(PendingSelection {
                            path: path_str.clone(),
                            doc_id,
                            revision: selection_rev,
                        });
                    }
                    {
                        let mut inner = this.0.borrow_mut();
                        inner.selected_doc_id = doc_id;
                        inner.selected_path = Some(path_str.clone());
                        inner.client_state = ClientState::new(doc_id);
                    }
                    this.dispatch(ClientEvent::OpenDocument { doc_id });
                    this.render();
                }
                Err(e) => {
                    let is_current = { this.0.borrow().selection_revision == selection_rev };
                    if !is_current {
                        return;
                    }
                    this.0.borrow_mut().storage_error =
                        Some(format!("Failed to read document mapping: {:?}", e));
                    this.render();
                }
            }
        });
    }

    fn create_file(&self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let full_path = {
            let inner = self.0.borrow();
            match &inner.create_folder {
                Some(folder) => format!("{}/{}", folder, name),
                None => name.clone(),
            }
        };
        let path = match WorkspacePath::new(&full_path) {
            Ok(p) => p,
            Err(_) => {
                self.0.borrow_mut().storage_error =
                    Some(format!("Invalid file name: {}", full_path));
                self.render();
                return;
            }
        };
        let path_str = path.as_str().to_string();
        let doc_id = DocId::new_v4();
        let this = self.clone();
        let ws = self.0.borrow().workspace_db.clone();
        let ws = match ws {
            Some(w) => w,
            None => return,
        };
        wasm_bindgen_futures::spawn_local(async move {
            match ws.read_file(&path).await {
                Ok(Some(_)) => {
                    this.0.borrow_mut().storage_error =
                        Some(format!("File already exists: {}", name));
                    this.render();
                    return;
                }
                Err(e) => {
                    this.0.borrow_mut().storage_error =
                        Some(format!("Create file check failed: {:?}", e));
                    this.render();
                    return;
                }
                Ok(None) => {}
            }
            match ws.write_file(&path, &[]).await {
                Ok(()) => match ws.set_doc_id(&path_str, doc_id.0).await {
                    Ok(()) => {
                        this.0.borrow_mut().create_folder = None;
                        let ancestors = markdown_render::ancestor_folders(&path_str);
                        {
                            let mut inner = this.0.borrow_mut();
                            for folder in &ancestors {
                                inner.expanded_folders.insert(folder.clone());
                            }
                        }
                        this.refresh_entries();
                        this.index_workspace();
                        this.select_entry(&path_str);
                    }
                    Err(e) => {
                        let msg = match ws.delete_file(&path).await {
                            Ok(()) => {
                                format!("Create file mapping failed: {:?}", e)
                            }
                            Err(rollback_err) => {
                                format!(
                                    "Create file mapping failed: {:?}; rollback also failed: {:?}",
                                    e, rollback_err
                                )
                            }
                        };
                        this.0.borrow_mut().storage_error = Some(msg);
                        this.render();
                    }
                },
                Err(e) => {
                    this.0.borrow_mut().storage_error =
                        Some(format!("Create file failed: {:?}", e));
                    this.render();
                }
            }
        });
    }
    fn delete_entry(&self, path: &str) {
        let path_str = path.to_string();
        let path_wp = match WorkspacePath::new(&path_str) {
            Ok(p) => p,
            Err(_) => return,
        };
        let this = self.clone();
        let ws = self.0.borrow().workspace_db.clone();
        let ws = match ws {
            Some(w) => w,
            None => return,
        };
        let is_selected = self.0.borrow().selected_path.as_deref() == Some(&path_str);
        if is_selected {
            self.flush_ink_before_transition();
        }
        wasm_bindgen_futures::spawn_local(async move {
            let doc_id = match ws.get_doc_id(&path_str).await {
                Ok(id) => id,
                Err(e) => {
                    this.0.borrow_mut().storage_error =
                        Some(format!("Delete lookup failed: {:?}", e));
                    this.render();
                    return;
                }
            };
            match ws.delete_file(&path_wp).await {
                Ok(()) => {}
                Err(e) => {
                    this.0.borrow_mut().storage_error = Some(format!("Delete failed: {:?}", e));
                    this.render();
                    return;
                }
            }
            if let Some(did) = doc_id {
                if let Err(e) = ws.delete_document_record(&did).await {
                    this.0.borrow_mut().storage_error =
                        Some(format!("Delete document record failed: {:?}", e));
                    if is_selected {
                        this.0.borrow_mut().selected_path = None;
                    }
                    this.refresh_entries();
                    this.render();
                    return;
                }
            }
            this.handle_delete_recent_note(&path_str);
            if is_selected {
                this.0.borrow_mut().selected_path = None;
            }
            this.refresh_entries();
            this.index_workspace();
            this.render();
        });
    }

    fn rename_entry(&self, old_path: &str, new_name: &str) {
        let old_path = old_path.to_string();
        let new_name = new_name.trim().to_string();
        if new_name.is_empty() {
            return;
        }
        let new_path = match WorkspacePath::new(&new_name) {
            Ok(p) => p,
            Err(_) => {
                self.0.borrow_mut().storage_error =
                    Some(format!("Invalid file name: {}", new_name));
                self.0.borrow_mut().renaming_path = None;
                self.render();
                return;
            }
        };
        let old_path_wp = match WorkspacePath::new(&old_path) {
            Ok(p) => p,
            Err(_) => {
                self.0.borrow_mut().renaming_path = None;
                return;
            }
        };
        let this = self.clone();
        let ws = self.0.borrow().workspace_db.clone();
        let ws = match ws {
            Some(w) => w,
            None => return,
        };
        let is_selected = self.0.borrow().selected_path.as_deref() == Some(&old_path);
        wasm_bindgen_futures::spawn_local(async move {
            match ws.rename_file(&old_path_wp, &new_path).await {
                Ok(()) => {
                    if is_selected {
                        this.0.borrow_mut().selected_path = Some(new_name.clone());
                    }
                    this.0.borrow_mut().renaming_path = None;
                    this.handle_rename_recent_note(&old_path, &new_name);
                    this.refresh_entries();
                    this.index_workspace();
                    this.render();
                }
                Err(e) => {
                    this.0.borrow_mut().storage_error = Some(format!("Rename failed: {:?}", e));
                    this.0.borrow_mut().renaming_path = None;
                    this.render();
                }
            }
        });
    }

    fn navigate_to_link(&self, target_path: &str) {
        let entry = {
            let inner = self.0.borrow();
            inner
                .entries
                .iter()
                .find(|e| e.path.as_str() == target_path)
                .cloned()
        };
        match entry {
            Some(e) if e.kind == WorkspaceEntryKind::Markdown => {
                let ancestors = markdown_render::ancestor_folders(target_path);
                {
                    let mut inner = self.0.borrow_mut();
                    for folder in &ancestors {
                        inner.expanded_folders.insert(folder.clone());
                    }
                }
                self.select_entry(target_path);
            }
            Some(_) => {
                // Asset or internal entry — do not navigate
                self.0.borrow_mut().link_message = Some(format!(
                    "Cannot navigate: not a Markdown note ({})",
                    target_path
                ));
                self.render();
            }
            None => {
                self.0.borrow_mut().link_message =
                    Some(format!("Linked note not found: {}", target_path));
                self.render();
            }
        }
    }

    fn request_missing_note(&self, raw_target: &str, source_path: &str) {
        let (resolution, proposal) = {
            let inner = self.0.borrow();
            let resolution = inner.knowledge.resolve_wikilink(source_path, raw_target);
            (
                resolution,
                knowledge::propose_missing_note(source_path, raw_target),
            )
        };
        let proposed_path = match (&resolution, proposal) {
            (knowledge::WikilinkResolution::Unresolved, Some(path)) => path,
            (knowledge::WikilinkResolution::Ambiguous(_), _) => {
                self.0.borrow_mut().link_message =
                    Some(format!("Ambiguous note link: {}", raw_target));
                self.render();
                return;
            }
            _ => {
                self.0.borrow_mut().link_message =
                    Some(format!("Cannot create note: {}", raw_target));
                self.render();
                return;
            }
        };
        self.0.borrow_mut().missing_note = Some(MissingNoteRequest {
            raw_target: raw_target.to_string(),
            proposed_path,
            source_path: source_path.to_string(),
            resolution,
        });
        self.render();
    }

    fn refresh_entries(&self) {
        let ws = match self.0.borrow().workspace_db.clone() {
            Some(w) => w,
            None => return,
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match ws.list_entries().await {
                Ok(mut entries) => {
                    entries.sort_by(|a, b| a.path.cmp(&b.path));
                    this.0.borrow_mut().entries = entries;
                    this.0.borrow_mut().workspace_loaded = true;

                    // Clean and load recent notes
                    let recents = {
                        let inner = this.0.borrow();
                        inner.recent_notes.clone()
                    };
                    this.clean_and_set_recent_notes(recents);

                    this.render();

                    // Re-index search database
                    this.index_workspace();
                }
                Err(e) => {
                    this.0.borrow_mut().storage_error =
                        Some(format!("List entries failed: {:?}", e));
                    this.render();
                }
            }
        });
    }

    fn index_workspace(&self) {
        let ws = match self.0.borrow().workspace_db.clone() {
            Some(w) => w,
            None => return,
        };
        let db_opt = self.0.borrow().db.clone();

        let this = self.clone();
        this.0.borrow_mut().search_indexing = true;
        this.render();

        wasm_bindgen_futures::spawn_local(async move {
            let db = match db_opt {
                Some(d) => d,
                None => match IndexedDb::open("inkstone_local_db", 1).await {
                    Ok(d) => {
                        this.0.borrow_mut().db = Some(d.clone());
                        d
                    }
                    Err(e) => {
                        this.0.borrow_mut().search_indexing = false;
                        this.0.borrow_mut().storage_error =
                            Some(format!("Search index fail-closed (db open): {:?}", e));
                        this.render();
                        return;
                    }
                },
            };

            let markdown_files: Vec<String> = this
                .0
                .borrow()
                .entries
                .iter()
                .filter(|e| e.kind == WorkspaceEntryKind::Markdown)
                .map(|e| e.path.as_str().to_string())
                .collect();

            let mut search_docs = Vec::new();

            // Rebuild knowledge index completely from scratch
            let mut new_knowledge = knowledge::KnowledgeIndex::new();
            // First pass: register all paths for wikilink resolution
            for path in &markdown_files {
                new_knowledge.register_path(path);
            }

            for path in markdown_files {
                let file_name = explorer::file_name_only(&path).to_string();
                let doc_id_opt = match ws.get_doc_id(&path).await {
                    Ok(Some(uuid)) => Some(DocId(uuid)),
                    Ok(None) => None,
                    Err(e) => {
                        this.0.borrow_mut().storage_error =
                            Some(format!("Search index fail-closed (get_doc_id): {:?}", e));
                        this.0.borrow_mut().search_indexing = false;
                        this.render();
                        return;
                    }
                };

                let mut title = String::new();
                let mut blocks = Vec::new();
                let mut md_blocks_for_knowledge = Vec::new();

                if let Some(doc_id) = doc_id_opt {
                    let record_result = db.get_document_record(&doc_id.to_string()).await;
                    match record_result {
                        Ok(Some(record_json)) => match storage::deserialize_record(&record_json) {
                            Ok(record) => {
                                let mut doc = record.confirmed_doc;
                                for update in record.pending_updates {
                                    if let Err(e) =
                                        inkstone_core::doc_ops::apply_update(&mut doc, update)
                                    {
                                        this.0.borrow_mut().storage_error = Some(format!(
                                            "Search index fail-closed (apply_update): {:?}",
                                            e
                                        ));
                                        this.0.borrow_mut().search_indexing = false;
                                        this.render();
                                        return;
                                    }
                                }
                                title = doc.title.clone();
                                md_blocks_for_knowledge =
                                    inkstone_core::markdown::index_document_blocks(&doc);
                                for block in doc.blocks {
                                    if let inkstone_core::document::BlockContent::Markdown {
                                        text,
                                    } = block.content
                                    {
                                        blocks.push(text);
                                    }
                                }
                            }
                            Err(e) => {
                                this.0.borrow_mut().storage_error = Some(format!(
                                    "Search index fail-closed (deserialize): {:?}",
                                    e
                                ));
                                this.0.borrow_mut().search_indexing = false;
                                this.render();
                                return;
                            }
                        },
                        Ok(None) => {}
                        Err(e) => {
                            this.0.borrow_mut().storage_error = Some(format!(
                                "Search index fail-closed (get_document_record): {:?}",
                                e
                            ));
                            this.0.borrow_mut().search_indexing = false;
                            this.render();
                            return;
                        }
                    }
                }

                search_docs.push(search::SearchDocument {
                    path: path.clone(),
                    file_name,
                    doc_id: doc_id_opt,
                    title,
                    blocks,
                });

                new_knowledge.index_document(&path, &md_blocks_for_knowledge);
            }

            {
                let mut inner = this.0.borrow_mut();
                inner.search_documents = search_docs;
                inner.knowledge = new_knowledge;
                inner.search_indexing = false;
            }
            this.perform_search_and_render();
        });
    }

    fn update_search_index_for_active_doc(&self, path: &str) {
        let updated = {
            let mut inner = self.0.borrow_mut();
            if let Some(pos) = inner
                .search_documents
                .iter()
                .position(|doc| doc.path == path)
            {
                let effective_doc = inner.client_state.effective_view_doc().clone();
                let mut blocks = Vec::new();
                for block in &effective_doc.blocks {
                    if let inkstone_core::document::BlockContent::Markdown { text } = &block.content
                    {
                        blocks.push(text.clone());
                    }
                }
                inner.search_documents[pos].title = effective_doc.title.clone();
                inner.search_documents[pos].blocks = blocks;

                let md_blocks_for_knowledge =
                    inkstone_core::markdown::index_document_blocks(&effective_doc);
                inner
                    .knowledge
                    .index_document(path, &md_blocks_for_knowledge);

                !inner.search_query.trim().is_empty()
            } else {
                let effective_doc = inner.client_state.effective_view_doc().clone();
                let blocks = effective_doc
                    .blocks
                    .iter()
                    .filter_map(|block| match &block.content {
                        inkstone_core::document::BlockContent::Markdown { text } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let md_blocks_for_knowledge =
                    inkstone_core::markdown::index_document_blocks(&effective_doc);
                inner.search_documents.push(search::SearchDocument {
                    path: path.to_string(),
                    file_name: explorer::file_name_only(path),
                    doc_id: Some(effective_doc.id),
                    title: effective_doc.title.clone(),
                    blocks,
                });
                inner.knowledge.register_path(path);
                inner
                    .knowledge
                    .index_document(path, &md_blocks_for_knowledge);
                !inner.search_query.trim().is_empty() || inner.tag_filter.is_some()
            }
        };
        if updated {
            self.perform_search_and_render();
        }
    }

    fn update_search_query(&self, val: &str) {
        {
            let mut inner = self.0.borrow_mut();
            inner.search_query = val.to_string();
            if val.trim().is_empty() {
                inner.search_results.clear();
                inner.search_highlighted_index = None;
            }
        }
        self.perform_search_and_render();
    }

    fn perform_search_and_render(&self) {
        // Compute results, update state, then render – all in separate borrow windows
        let (query, search_docs) = {
            let inner = self.0.borrow();
            (inner.search_query.clone(), inner.search_documents.clone())
        };
        let results = search::rank_results(&search_docs, &query);
        {
            let mut inner = self.0.borrow_mut();
            let highlighted = if results.is_empty() {
                None
            } else {
                let current = inner.search_highlighted_index.unwrap_or(0);
                Some(if current >= results.len() { 0 } else { current })
            };
            inner.search_results = results;
            inner.search_highlighted_index = highlighted;
        }
        self.render();
    }

    fn search_move_highlight(&self, delta: isize) {
        let mut inner = self.0.borrow_mut();
        let len = inner.search_results.len();
        if len == 0 {
            return;
        }
        let current = inner.search_highlighted_index.unwrap_or(0) as isize;
        let mut next = current + delta;
        if next < 0 {
            next = (len - 1) as isize;
        } else if next >= len as isize {
            next = 0;
        }
        inner.search_highlighted_index = Some(next as usize);
        drop(inner);
        self.render();
    }

    fn search_open_highlighted(&self) {
        let path_opt = {
            let inner = self.0.borrow();
            inner
                .search_highlighted_index
                .and_then(|idx| inner.search_results.get(idx))
                .map(|r| r.path.clone())
        };
        if let Some(path) = path_opt {
            self.select_entry(&path);
            self.search_clear_query_only();
        }
    }

    fn search_clear(&self) {
        let mut inner = self.0.borrow_mut();
        inner.search_query.clear();
        inner.tag_filter = None;
        inner.search_results.clear();
        inner.search_highlighted_index = None;
        drop(inner);
        self.render();
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(search_el) = document.query_selector(".search-input").unwrap() {
                    if let Ok(search_input) = search_el.dyn_into::<web_sys::HtmlInputElement>() {
                        let _ = search_input.blur();
                    }
                }
            }
        }
    }

    fn search_clear_query_only(&self) {
        let mut inner = self.0.borrow_mut();
        inner.search_query.clear();
        inner.tag_filter = None;
        inner.search_results.clear();
        inner.search_highlighted_index = None;
        drop(inner);
        self.render();
    }

    fn clean_and_set_recent_notes(&self, mut recents: Vec<String>) {
        let valid_paths: std::collections::HashSet<String> = {
            let inner = self.0.borrow();
            inner
                .entries
                .iter()
                .filter(|e| e.kind == WorkspaceEntryKind::Markdown)
                .map(|e| e.path.as_str().to_string())
                .collect()
        };

        let old_recents = recents.clone();

        let mut i = 0;
        let mut seen = std::collections::HashSet::new();
        while i < recents.len() {
            let path = &recents[i];
            let is_valid = valid_paths.contains(path)
                && path.to_lowercase().ends_with(".md")
                && !path.starts_with('.')
                && !path.contains("/.");

            if !is_valid || !seen.insert(path.clone()) {
                recents.remove(i);
            } else {
                i += 1;
            }
        }

        if recents.len() > 8 {
            recents.truncate(8);
        }

        let changed = recents != old_recents;

        if changed {
            let ws = self.0.borrow().workspace_db.clone();
            let this = self.clone();
            let rev = {
                let mut inner = self.0.borrow_mut();
                inner.recent_notes_revision += 1;
                inner.recent_notes_revision
            };
            if let Some(ws) = ws {
                wasm_bindgen_futures::spawn_local(async move {
                    match ws.save_recent_notes(&recents).await {
                        Ok(()) => {
                            let mut inner = this.0.borrow_mut();
                            if inner.recent_notes_revision == rev {
                                inner.recent_notes = recents;
                                drop(inner);
                                this.render();
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().storage_error =
                                Some(format!("Failed to normalize recent notes: {:?}", e));
                            this.render();
                        }
                    }
                });
            }
        } else {
            self.0.borrow_mut().recent_notes = recents;
        }
    }

    fn complete_pending_recent(&self, doc_id: DocId) {
        let (path, rev) = {
            let mut inner = self.0.borrow_mut();
            if let Some(pending) = inner.pending_selection.take() {
                if pending.revision == inner.selection_revision && pending.doc_id == doc_id {
                    (pending.path, pending.revision)
                } else {
                    return;
                }
            } else {
                return;
            }
        };
        self.add_recent_note(&path, rev);
    }

    fn add_recent_note(&self, path: &str, revision: u64) {
        let mut inner = self.0.borrow_mut();
        if inner.selection_revision != revision {
            return;
        }

        let mut recents = inner.recent_notes.clone();
        recents.retain(|p| p != path);
        recents.insert(0, path.to_string());
        recents.truncate(8);

        inner.recent_notes = recents.clone();
        inner.recent_notes_revision += 1;
        let recent_revision = inner.recent_notes_revision;

        let ws = inner.workspace_db.clone();
        let this = self.clone();
        drop(inner);

        if let Some(ws) = ws {
            wasm_bindgen_futures::spawn_local(async move {
                match ws.save_recent_notes(&recents).await {
                    Ok(()) => {
                        if this.0.borrow().recent_notes_revision == recent_revision {
                            this.render();
                        }
                    }
                    Err(e) => {
                        this.0.borrow_mut().storage_error =
                            Some(format!("Failed to save recent notes: {:?}", e));
                        this.render();
                    }
                }
            });
        }
    }

    fn clear_recent_notes(&self) {
        let ws = self.0.borrow().workspace_db.clone();
        let this = self.clone();
        let rev = {
            let mut inner = self.0.borrow_mut();
            inner.recent_notes_revision += 1;
            inner.recent_notes.clear();
            inner.recent_notes_revision
        };
        if let Some(ws) = ws {
            wasm_bindgen_futures::spawn_local(async move {
                match ws.save_recent_notes(&[]).await {
                    Ok(()) => {
                        let inner = this.0.borrow_mut();
                        if inner.recent_notes_revision == rev {
                            drop(inner);
                            this.render();
                        }
                    }
                    Err(e) => {
                        this.0.borrow_mut().storage_error =
                            Some(format!("Failed to clear recent notes: {:?}", e));
                        this.render();
                    }
                }
            });
        }
    }

    fn handle_rename_recent_note(&self, old_path: &str, new_path: &str) {
        let mut recents = {
            let inner = self.0.borrow();
            inner.recent_notes.clone()
        };
        let mut changed = false;
        for path in recents.iter_mut() {
            if path == old_path {
                *path = new_path.to_string();
                changed = true;
            }
        }
        if changed {
            let ws = self.0.borrow().workspace_db.clone();
            let this = self.clone();
            let rev = {
                let mut inner = self.0.borrow_mut();
                inner.recent_notes_revision += 1;
                inner.recent_notes_revision
            };
            if let Some(ws) = ws {
                wasm_bindgen_futures::spawn_local(async move {
                    match ws.save_recent_notes(&recents).await {
                        Ok(()) => {
                            let mut inner = this.0.borrow_mut();
                            if inner.recent_notes_revision == rev {
                                inner.recent_notes = recents;
                                drop(inner);
                                this.render();
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().storage_error =
                                Some(format!("Failed to update recent notes on rename: {:?}", e));
                            this.render();
                        }
                    }
                });
            }
        }
    }

    fn handle_delete_recent_note(&self, path: &str) {
        let mut recents = {
            let inner = self.0.borrow();
            inner.recent_notes.clone()
        };
        if recents.contains(&path.to_string()) {
            recents.retain(|p| p != path);
            let ws = self.0.borrow().workspace_db.clone();
            let this = self.clone();
            let rev = {
                let mut inner = self.0.borrow_mut();
                inner.recent_notes = recents.clone();
                inner.recent_notes_revision += 1;
                inner.recent_notes_revision
            };
            if let Some(ws) = ws {
                wasm_bindgen_futures::spawn_local(async move {
                    match ws.save_recent_notes(&recents).await {
                        Ok(()) => {
                            if this.0.borrow().recent_notes_revision == rev {
                                this.render();
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().storage_error =
                                Some(format!("Failed to update recent notes on delete: {:?}", e));
                            this.render();
                        }
                    }
                });
            }
        }
    }

    pub fn dispatch(&self, event: ClientEvent) {
        let effects = {
            let mut inner = self.0.borrow_mut();
            inner.client_state.transition(event)
        };

        let is_rendering = RENDERING.with(|rendering| rendering.get());
        let should_render = !is_rendering && {
            let inner = self.0.borrow();
            let state = &inner.client_state;
            !state.has_pending_intent()
                && state.intent_queue_len() == 0
                && !self.has_editable_focus()
        };

        if should_render {
            self.render();
        }

        for effect in effects {
            self.execute_effect(effect);
        }

        // The persistence completion updates derived search and knowledge indexes.
        // Doing it synchronously here re-enters the driver from editor DOM events.
    }

    fn has_editable_focus(&self) -> bool {
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(el) = document.active_element() {
                    let class = el.class_name();
                    if class == "document-title-input"
                        || el.class_list().contains("block-textarea")
                        || class == "search-input"
                        || el.id().starts_with("ink-canvas-")
                    {
                        return true;
                    }
                    let element: &web_sys::Element = el.as_ref();
                    return element.closest(".ink-block").ok().flatten().is_some();
                }
            }
        }
        false
    }

    fn execute_effect(&self, effect: Effect) {
        let this = self.clone();
        match effect {
            Effect::LoadLocalState { lifecycle, doc_id } => {
                wasm_bindgen_futures::spawn_local(async move {
                    let db = match IndexedDb::open("inkstone_local_db", 1).await {
                        Ok(database) => {
                            this.0.borrow_mut().db = Some(database.clone());
                            database
                        }
                        Err(e) => {
                            let err_str = format!("Failed to open IndexedDB: {:?}", e);
                            this.0.borrow_mut().persistence_status = format!("Error: {}", err_str);
                            this.0.borrow_mut().storage_error = Some(err_str);
                            this.render();
                            return;
                        }
                    };

                    let record_result = db.get_document_record(&doc_id.to_string()).await;
                    if this.0.borrow().selected_doc_id != doc_id {
                        return;
                    }

                    match record_result {
                        Ok(Some(record_json)) => match storage::deserialize_record(&record_json) {
                            Ok(record) => {
                                this.0.borrow_mut().persistence_status =
                                    "Loaded from IndexedDB".to_string();
                                this.dispatch(ClientEvent::LocalHydrationLoaded {
                                    lifecycle,
                                    confirmed_doc: Some(record.confirmed_doc),
                                    confirmed_seq: record.confirmed_seq,
                                    pending_updates: record.pending_updates,
                                });
                                this.complete_pending_recent(doc_id);
                            }
                            Err(e) => {
                                let err_str = format!("Parse Error: {:?}", e);
                                this.0.borrow_mut().persistence_status =
                                    format!("Error: {}", err_str);
                                this.0.borrow_mut().storage_error = Some(err_str);
                                this.render();
                            }
                        },
                        Ok(None) => {
                            this.0.borrow_mut().persistence_status =
                                "No local state. Creating default doc.".to_string();
                            this.dispatch(ClientEvent::LocalHydrationLoaded {
                                lifecycle,
                                confirmed_doc: None,
                                confirmed_seq: 0,
                                pending_updates: Vec::new(),
                            });
                            this.complete_pending_recent(doc_id);

                            let doc_is_empty =
                                this.0.borrow().client_state.view_doc().blocks.is_empty();
                            if doc_is_empty {
                                let update_id = UpdateId::new_v4();
                                let applied_at_ms = js_sys::Date::now() as i64;
                                let block_id = BlockId::new_v4();
                                let block = inkstone_core::document::DocumentBlock::new(
                                    block_id,
                                    inkstone_core::document::BlockContent::Markdown {
                                        text: "# Welcome to Inkstone\n\nStart writing..."
                                            .to_string(),
                                    },
                                );
                                this.dispatch(ClientEvent::LocalIntent {
                                    op: DocumentOp::InsertBlock { idx: 0, block },
                                    update_id,
                                    applied_at_ms,
                                });
                            }
                        }
                        Err(e) => {
                            let err_str = format!("IndexedDB Read Error: {:?}", e);
                            this.0.borrow_mut().persistence_status = format!("Error: {}", err_str);
                            this.0.borrow_mut().storage_error = Some(err_str);
                            this.render();
                        }
                    }
                });
            }
            Effect::PersistLocalUpdate { update_id, update } => {
                this.0.borrow_mut().persistence_status = "Saving update...".to_string();
                wasm_bindgen_futures::spawn_local(async move {
                    let doc_id = update.doc_id;
                    let db = {
                        let inner = this.0.borrow();
                        inner.db.clone()
                    };

                    let db = match db {
                        Some(d) => d,
                        None => {
                            this.0.borrow_mut().storage_error =
                                Some("IndexedDB handle unavailable for update".to_string());
                            this.0.borrow_mut().persistence_status =
                                "Storage error: IndexedDB handle unavailable".to_string();
                            this.dispatch(ClientEvent::LocalPersistenceFailed { update_id });
                            this.render();
                            return;
                        }
                    };

                    let record_opt = match db.get_document_record(&doc_id.to_string()).await {
                        Ok(Some(json)) => match storage::deserialize_record(&json) {
                            Ok(rec) => Some(rec),
                            Err(e) => {
                                this.0.borrow_mut().persistence_status =
                                    format!("Error: parse failed on update: {:?}", e);
                                this.0.borrow_mut().storage_error = Some(format!(
                                    "Parse error on persisted update record: {:?}",
                                    e
                                ));
                                this.dispatch(ClientEvent::LocalPersistenceFailed { update_id });
                                this.render();
                                return;
                            }
                        },
                        Ok(None) => None,
                        Err(e) => {
                            this.0.borrow_mut().persistence_status =
                                format!("Error: read failed: {:?}", e);
                            this.0.borrow_mut().storage_error =
                                Some(format!("IndexedDB read error during update: {:?}", e));
                            this.dispatch(ClientEvent::LocalPersistenceFailed { update_id });
                            this.render();
                            return;
                        }
                    };

                    let mut record = record_opt.unwrap_or_else(|| storage::new_record(doc_id));
                    storage::append_pending_update(&mut record, update);

                    match serde_json::to_string(&record) {
                        Ok(json) => {
                            match db.put_document_record(&doc_id.to_string(), &json).await {
                                Ok(()) => {
                                    this.0.borrow_mut().persistence_status =
                                        "Saved to IndexedDB".to_string();
                                    let current_path = {
                                        let inner = this.0.borrow();
                                        (inner.selected_doc_id == doc_id)
                                            .then(|| inner.selected_path.clone())
                                            .flatten()
                                    };
                                    if let Some(path) = current_path {
                                        this.dispatch(ClientEvent::LocalUpdatePersisted {
                                            update_id,
                                        });
                                        this.update_search_index_for_active_doc(&path);
                                    } else {
                                        this.index_workspace();
                                    }
                                }
                                Err(e) => {
                                    this.0.borrow_mut().persistence_status =
                                        "Failed to save update".to_string();
                                    this.0.borrow_mut().storage_error = Some(format!(
                                        "IndexedDB write error during update: {:?}",
                                        e
                                    ));
                                    this.dispatch(ClientEvent::LocalPersistenceFailed {
                                        update_id,
                                    });
                                    this.render();
                                }
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().persistence_status =
                                "Failed to serialize update record".to_string();
                            this.0.borrow_mut().storage_error =
                                Some(format!("Record serialization error during update: {:?}", e));
                            this.dispatch(ClientEvent::LocalPersistenceFailed { update_id });
                            this.render();
                        }
                    }
                });
            }
            Effect::PersistConfirmation { token, checkpoint } => {
                this.0.borrow_mut().persistence_status = "Saving confirmation...".to_string();
                wasm_bindgen_futures::spawn_local(async move {
                    let doc_id = checkpoint.doc_id;
                    let db = {
                        let inner = this.0.borrow();
                        inner.db.clone()
                    };

                    let db = match db {
                        Some(d) => d,
                        None => {
                            this.0.borrow_mut().storage_error =
                                Some("IndexedDB handle unavailable for confirmation".to_string());
                            this.0.borrow_mut().persistence_status =
                                "Storage error: IndexedDB handle unavailable".to_string();
                            this.dispatch(ClientEvent::ConfirmationPersistenceFailed { token });
                            this.render();
                            return;
                        }
                    };

                    let record_opt = match db.get_document_record(&doc_id.to_string()).await {
                        Ok(Some(json)) => match storage::deserialize_record(&json) {
                            Ok(rec) => Some(rec),
                            Err(e) => {
                                this.0.borrow_mut().persistence_status =
                                    format!("Error: parse failed on confirmation: {:?}", e);
                                this.0.borrow_mut().storage_error =
                                    Some(format!("Parse error on confirmation record: {:?}", e));
                                this.dispatch(ClientEvent::ConfirmationPersistenceFailed { token });
                                this.render();
                                return;
                            }
                        },
                        Ok(None) => None,
                        Err(e) => {
                            this.0.borrow_mut().persistence_status =
                                format!("Error: read failed: {:?}", e);
                            this.0.borrow_mut().storage_error =
                                Some(format!("IndexedDB read error during confirmation: {:?}", e));
                            this.dispatch(ClientEvent::ConfirmationPersistenceFailed { token });
                            this.render();
                            return;
                        }
                    };

                    let mut record = record_opt.unwrap_or_else(|| storage::new_record(doc_id));
                    storage::apply_confirmation(&mut record, checkpoint);

                    match serde_json::to_string(&record) {
                        Ok(json) => {
                            match db.put_document_record(&doc_id.to_string(), &json).await {
                                Ok(()) => {
                                    this.0.borrow_mut().persistence_status =
                                        "Saved confirmation".to_string();
                                    let current_path = {
                                        let inner = this.0.borrow();
                                        (inner.selected_doc_id == doc_id)
                                            .then(|| inner.selected_path.clone())
                                            .flatten()
                                    };
                                    if let Some(path) = current_path {
                                        this.dispatch(ClientEvent::ConfirmationPersisted { token });
                                        this.update_search_index_for_active_doc(&path);
                                    } else {
                                        this.index_workspace();
                                    }
                                }
                                Err(e) => {
                                    this.0.borrow_mut().persistence_status =
                                        "Failed to save confirmation".to_string();
                                    this.0.borrow_mut().storage_error = Some(format!(
                                        "IndexedDB write error during confirmation: {:?}",
                                        e
                                    ));
                                    this.dispatch(ClientEvent::ConfirmationPersistenceFailed {
                                        token,
                                    });
                                    this.render();
                                }
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().persistence_status =
                                "Failed to serialize confirmation record".to_string();
                            this.0.borrow_mut().storage_error = Some(format!(
                                "Record serialization error during confirmation: {:?}",
                                e
                            ));
                            this.dispatch(ClientEvent::ConfirmationPersistenceFailed { token });
                            this.render();
                        }
                    }
                });
            }
            Effect::ConnectSocket { .. } => {}
            Effect::DisconnectSocket => {}
            Effect::FetchUpdates { .. } => {}
            Effect::SendUpdate(_) => {}
            Effect::UploadSnapshot(_) => {}
            Effect::ScheduleReconnect { .. } => {}
            Effect::CancelReconnect => {}
            Effect::NotifyError(msg) => {
                web_sys::console::error_1(&JsValue::from_str(&msg));
            }
        }
    }

    fn render(&self) {
        RENDERING.with(|rendering| rendering.set(true));
        self.flush_ink_runtimes();
        let window = web_sys::window().expect("No global window found");
        let document = window.document().expect("No global document found");
        let root = document
            .get_element_by_id("root")
            .expect("No element with id 'root' found");

        let active_element = document.active_element();
        let focus_info = active_element.and_then(|el| {
            let class = el.class_name();
            if class == "document-title-input" {
                let input: web_sys::HtmlInputElement = el.unchecked_into();
                Some((
                    "title".to_string(),
                    input.selection_start().ok().flatten().unwrap_or(0),
                    input.selection_end().ok().flatten().unwrap_or(0),
                ))
            } else if el.class_list().contains("block-textarea") {
                let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                let block_id = textarea.get_attribute("data-block-id").unwrap_or_default();
                Some((
                    format!("block:{}", block_id),
                    textarea.selection_start().ok().flatten().unwrap_or(0),
                    textarea.selection_end().ok().flatten().unwrap_or(0),
                ))
            } else if class == "search-input" {
                let input: web_sys::HtmlInputElement = el.unchecked_into();
                Some((
                    "search".to_string(),
                    input.selection_start().ok().flatten().unwrap_or(0),
                    input.selection_end().ok().flatten().unwrap_or(0),
                ))
            } else {
                None
            }
        });

        root.set_inner_html("");

        let inner = self.0.borrow().clone();
        let client_state = &inner.client_state;
        let view_doc = client_state.effective_view_doc();
        let phase = client_state.phase();
        let persistence_status = &inner.persistence_status;
        let storage_error = &inner.storage_error;
        let entries = &inner.entries;
        let selected_path = &inner.selected_path;

        let editing_allowed = storage_error.is_none()
            && phase != SessionPhase::HydratingLocal
            && phase != SessionPhase::Closed
            && phase != SessionPhase::Error;

        let app_container = document.create_element("div").unwrap();
        app_container.set_class_name(if inner.sidebar_open {
            "app-container sidebar-open"
        } else {
            "app-container"
        });

        let sidebar = document.create_element("div").unwrap();
        sidebar.set_class_name("sidebar");
        let brand = document.create_element("div").unwrap();
        brand.set_class_name("brand");
        brand.set_text_content(Some("Inkstone"));
        sidebar.append_child(&brand).unwrap();
        let workspace_name = document.create_element("div").unwrap();
        workspace_name.set_class_name("workspace-name");
        workspace_name.set_text_content(Some("Local knowledge workspace"));
        sidebar.append_child(&workspace_name).unwrap();

        // Search Input
        let search_container = document.create_element("div").unwrap();
        search_container.set_class_name("search-container");

        if let Some(ref tag) = inner.tag_filter {
            let active_tag_div = document.create_element("div").unwrap();
            active_tag_div.set_class_name("active-tag-filter");

            let tag_label = document.create_element("span").unwrap();
            tag_label.set_text_content(Some(&format!("#{}", tag)));

            let clear_btn = document.create_element("button").unwrap();
            clear_btn.set_class_name("clear-tag-btn");
            clear_btn.set_text_content(Some("×"));

            active_tag_div.append_child(&tag_label).unwrap();
            active_tag_div.append_child(&clear_btn).unwrap();
            search_container.append_child(&active_tag_div).unwrap();
        }

        let search_input = document.create_element("input").unwrap();
        let search_input: web_sys::HtmlInputElement = search_input.unchecked_into();
        search_input.set_type("text");
        search_input.set_class_name("search-input");
        search_input.set_placeholder("Search notes...");
        search_input.set_value(&inner.search_query);
        search_container.append_child(&search_input).unwrap();

        if inner.search_indexing {
            let status = document.create_element("div").unwrap();
            status.set_class_name("search-status");
            status.set_text_content(Some("Indexing notes..."));
            search_container.append_child(&status).unwrap();
        } else if !inner.search_query.trim().is_empty() {
            let status = document.create_element("div").unwrap();
            status.set_class_name("search-status");
            let count = inner.search_results.len();
            if count == 0 {
                status.set_text_content(Some("No matching notes"));
            } else {
                status.set_text_content(Some(&format!("{} results", count)));
            }
            search_container.append_child(&status).unwrap();
        }
        sidebar.append_child(&search_container).unwrap();

        let is_search_active = !inner.search_query.trim().is_empty() || inner.tag_filter.is_some();

        if !is_search_active && !inner.recent_notes.is_empty() {
            let recent_heading = document.create_element("div").unwrap();
            recent_heading.set_class_name("sidebar-heading recent-heading");

            let recent_title = document.create_element("span").unwrap();
            recent_title.set_text_content(Some("Recent"));
            recent_heading.append_child(&recent_title).unwrap();

            let clear_btn = document.create_element("button").unwrap();
            clear_btn.set_class_name("recent-clear-btn");
            clear_btn.set_text_content(Some("Clear"));
            recent_heading.append_child(&clear_btn).unwrap();

            sidebar.append_child(&recent_heading).unwrap();

            let recent_list = document.create_element("div").unwrap();
            recent_list.set_class_name("recent-list");

            for path in &inner.recent_notes {
                let recent_item = document.create_element("div").unwrap();
                let is_active = selected_path.as_deref() == Some(path);
                let recent_class = if is_active {
                    "recent-item active"
                } else {
                    "recent-item"
                };
                recent_item.set_class_name(recent_class);
                recent_item.set_attribute("data-path", path).unwrap();

                let file_name = explorer::file_name_only(path).to_string();

                let name_span = document.create_element("span").unwrap();
                name_span.set_class_name("recent-item-name");
                name_span.set_text_content(Some(&file_name));
                recent_item.append_child(&name_span).unwrap();

                let path_span = document.create_element("span").unwrap();
                path_span.set_class_name("recent-item-path");
                path_span.set_text_content(Some(path));
                recent_item.append_child(&path_span).unwrap();

                recent_list.append_child(&recent_item).unwrap();
            }

            sidebar.append_child(&recent_list).unwrap();
        }

        let sidebar_heading = document.create_element("div").unwrap();
        sidebar_heading.set_class_name("sidebar-heading");
        sidebar_heading.set_text_content(Some("Files"));
        sidebar.append_child(&sidebar_heading).unwrap();

        let file_list = document.create_element("div").unwrap();
        file_list.set_class_name("file-list");

        if is_search_active {
            let tag_paths = inner
                .tag_filter
                .as_ref()
                .and_then(|tag| inner.knowledge.tag_to_paths.get(tag));
            if inner.search_query.trim().is_empty() && tag_paths.is_some() {
                for path in tag_paths.unwrap() {
                    let item_div = document.create_element("div").unwrap();
                    item_div.set_class_name("search-result-item");
                    item_div.set_attribute("data-path", path).unwrap();
                    item_div.set_text_content(Some(path));
                    file_list.append_child(&item_div).unwrap();
                }
            } else if inner.search_results.is_empty() {
                let empty_results = document.create_element("div").unwrap();
                empty_results.set_class_name("search-no-results");
                empty_results.set_text_content(Some("No matching notes"));
                file_list.append_child(&empty_results).unwrap();
            } else {
                for (idx, result) in inner.search_results.iter().enumerate() {
                    let item_div = document.create_element("div").unwrap();
                    let is_active = selected_path.as_deref() == Some(&result.path);
                    let is_highlighted = inner.search_highlighted_index == Some(idx);

                    let mut class_name = "search-result-item".to_string();
                    if is_active {
                        class_name.push_str(" active");
                    }
                    if is_highlighted {
                        class_name.push_str(" highlighted");
                    }
                    item_div.set_class_name(&class_name);
                    item_div.set_attribute("data-path", &result.path).unwrap();

                    let name_el = document.create_element("div").unwrap();
                    name_el.set_class_name("search-result-name");
                    Driver::highlight_text_into(&name_el, &result.file_name, &inner.search_query);
                    item_div.append_child(&name_el).unwrap();

                    let path_el = document.create_element("div").unwrap();
                    path_el.set_class_name("search-result-path");
                    Driver::highlight_text_into(&path_el, &result.path, &inner.search_query);
                    item_div.append_child(&path_el).unwrap();

                    if let Some(ref snippet) = result.snippet {
                        let snippet_el = document.create_element("div").unwrap();
                        snippet_el.set_class_name("search-result-snippet");
                        Driver::highlight_text_into(&snippet_el, snippet, &inner.search_query);
                        item_div.append_child(&snippet_el).unwrap();
                    } else if !result.title.is_empty() {
                        let title_el = document.create_element("div").unwrap();
                        title_el.set_class_name("search-result-snippet");
                        Driver::highlight_text_into(&title_el, &result.title, &inner.search_query);
                        item_div.append_child(&title_el).unwrap();
                    }

                    file_list.append_child(&item_div).unwrap();
                }
            }
        } else {
            if inner.show_create {
                let create_row = document.create_element("div").unwrap();
                create_row.set_class_name("explorer-create-row");
                let create_input = document.create_element("input").unwrap();
                let create_input: web_sys::HtmlInputElement = create_input.unchecked_into();
                create_input.set_class_name("explorer-create-input");
                create_input.set_placeholder("filename.md");
                create_input.set_value(&inner.create_value);
                create_row.append_child(&create_input).unwrap();
                file_list.append_child(&create_row).unwrap();
            } else {
                let new_btn = document.create_element("button").unwrap();
                new_btn.set_class_name("explorer-new-btn");
                new_btn.set_text_content(Some("+ New File"));
                file_list.append_child(&new_btn).unwrap();
            }

            let flat_tree = explorer::build_flat_tree(entries, &inner.expanded_folders);
            for flat_node in &flat_tree {
                let indent_px = 8 + flat_node.depth * 20;
                if flat_node.is_folder {
                    let folder_div = document.create_element("div").unwrap();
                    let is_target = inner.create_folder.as_deref() == Some(&flat_node.path);
                    let folder_class = match (flat_node.is_expanded, is_target) {
                        (true, true) => "explorer-folder expanded active-target",
                        (true, false) => "explorer-folder expanded",
                        (false, true) => "explorer-folder active-target",
                        (false, false) => "explorer-folder",
                    };
                    folder_div.set_class_name(folder_class);
                    folder_div
                        .set_attribute("data-path", &flat_node.path)
                        .unwrap();
                    folder_div
                        .set_attribute("data-depth", &flat_node.depth.to_string())
                        .unwrap();
                    let toggle = document.create_element("span").unwrap();
                    toggle.set_class_name("explorer-toggle");
                    toggle.set_text_content(Some(if flat_node.is_expanded {
                        "\u{25bc}"
                    } else {
                        "\u{25b6}"
                    }));
                    folder_div.append_child(&toggle).unwrap();
                    let name_span = document.create_element("span").unwrap();
                    name_span.set_class_name("folder-name");
                    name_span.set_text_content(Some(&flat_node.name));
                    folder_div.append_child(&name_span).unwrap();
                    folder_div
                        .set_attribute("style", &format!("padding-left: {}px", indent_px))
                        .unwrap();
                    file_list.append_child(&folder_div).unwrap();
                } else {
                    let is_active = selected_path.as_deref() == Some(&flat_node.path);
                    let is_renaming = inner.renaming_path.as_deref() == Some(&flat_node.path);
                    let file_entry = document.create_element("div").unwrap();
                    let entry_class = if is_active {
                        "file-entry active"
                    } else {
                        "file-entry"
                    };
                    file_entry.set_class_name(entry_class);
                    file_entry
                        .set_attribute("data-path", &flat_node.path)
                        .unwrap();
                    file_entry
                        .set_attribute("data-depth", &flat_node.depth.to_string())
                        .unwrap();
                    file_entry
                        .set_attribute("style", &format!("padding-left: {}px", indent_px))
                        .unwrap();

                    if is_renaming {
                        let rename_input = document.create_element("input").unwrap();
                        let rename_input: web_sys::HtmlInputElement = rename_input.unchecked_into();
                        rename_input.set_class_name("rename-input");
                        rename_input.set_value(&inner.rename_value);
                        file_entry.append_child(&rename_input).unwrap();
                    } else {
                        let file_name = document.create_element("span").unwrap();
                        file_name.set_class_name("file-name");
                        file_name.set_text_content(Some(&flat_node.name));
                        file_entry.append_child(&file_name).unwrap();
                    }

                    let file_actions = document.create_element("span").unwrap();
                    file_actions.set_class_name("file-actions");

                    let rename_btn = document.create_element("button").unwrap();
                    rename_btn.set_class_name("rename-btn");
                    rename_btn
                        .set_attribute("data-path", &flat_node.path)
                        .unwrap();
                    rename_btn.set_text_content(Some("Rename"));
                    file_actions.append_child(&rename_btn).unwrap();

                    let delete_btn = document.create_element("button").unwrap();
                    delete_btn.set_class_name("delete-btn");
                    delete_btn
                        .set_attribute("data-path", &flat_node.path)
                        .unwrap();
                    delete_btn.set_text_content(Some("Delete"));
                    file_actions.append_child(&delete_btn).unwrap();

                    file_entry.append_child(&file_actions).unwrap();
                    file_list.append_child(&file_entry).unwrap();
                }
            }
        }
        sidebar.append_child(&file_list).unwrap();

        let sidebar_footer = document.create_element("div").unwrap();
        sidebar_footer.set_class_name("sidebar-footer");

        let phase_badge = document.create_element("span").unwrap();
        phase_badge.set_class_name(&format!("phase-badge phase-{:?}", phase).to_lowercase());
        phase_badge.set_text_content(Some(&format!("{:?}", phase)));
        sidebar_footer.append_child(&phase_badge).unwrap();

        let status_area = document.create_element("div").unwrap();
        status_area.set_class_name("status-area");
        status_area.set_text_content(Some(persistence_status));
        sidebar_footer.append_child(&status_area).unwrap();

        sidebar.append_child(&sidebar_footer).unwrap();
        app_container.append_child(&sidebar).unwrap();

        let main_content = document.create_element("div").unwrap();
        main_content.set_class_name(if inner.document_mode == DocumentMode::Preview {
            "main-content preview-mode"
        } else {
            "main-content"
        });

        let topbar = document.create_element("header").unwrap();
        topbar.set_class_name("workspace-topbar");
        let nav_toggle = document.create_element("button").unwrap();
        nav_toggle.set_class_name("sidebar-toggle-btn");
        nav_toggle.set_text_content(Some("Workspace"));
        nav_toggle
            .set_attribute("aria-label", "Toggle workspace navigation")
            .unwrap();
        topbar.append_child(&nav_toggle).unwrap();
        let breadcrumb = document.create_element("div").unwrap();
        breadcrumb.set_class_name("breadcrumb");
        breadcrumb.set_text_content(Some(selected_path.as_deref().unwrap_or("Workspace")));
        topbar.append_child(&breadcrumb).unwrap();
        let topbar_actions = document.create_element("div").unwrap();
        topbar_actions.set_class_name("topbar-actions");
        let quick_search = document.create_element("button").unwrap();
        quick_search.set_class_name("topbar-search-btn");
        quick_search.set_text_content(Some("Search pages"));
        topbar_actions.append_child(&quick_search).unwrap();
        let save_status = document.create_element("span").unwrap();
        save_status.set_class_name("topbar-save-status");
        save_status.set_text_content(Some(persistence_status));
        topbar_actions.append_child(&save_status).unwrap();
        if selected_path.is_some() {
            let mode_bar = document.create_element("div").unwrap();
            mode_bar.set_class_name("mode-bar");
            let edit_btn = document.create_element("button").unwrap();
            edit_btn.set_class_name("mode-toggle-btn");
            edit_btn.set_text_content(Some("Edit"));
            edit_btn
                .set_attribute(
                    "aria-pressed",
                    if inner.document_mode == DocumentMode::Edit {
                        "true"
                    } else {
                        "false"
                    },
                )
                .unwrap();
            if inner.document_mode == DocumentMode::Edit {
                edit_btn.set_attribute("data-active", "true").unwrap();
            }
            mode_bar.append_child(&edit_btn).unwrap();
            let preview_btn = document.create_element("button").unwrap();
            preview_btn.set_class_name("mode-toggle-btn");
            preview_btn.set_attribute("data-mode", "preview").unwrap();
            preview_btn.set_text_content(Some("Preview"));
            preview_btn
                .set_attribute(
                    "aria-pressed",
                    if inner.document_mode == DocumentMode::Preview {
                        "true"
                    } else {
                        "false"
                    },
                )
                .unwrap();
            if inner.document_mode == DocumentMode::Preview {
                preview_btn.set_attribute("data-active", "true").unwrap();
            }
            mode_bar.append_child(&preview_btn).unwrap();
            let inspector_toggle = document.create_element("button").unwrap();
            inspector_toggle.set_class_name("mode-toggle-btn toggle-inspector-btn");
            inspector_toggle.set_text_content(Some("Inspector"));
            mode_bar.append_child(&inspector_toggle).unwrap();
            topbar_actions.append_child(&mode_bar).unwrap();
        }
        topbar.append_child(&topbar_actions).unwrap();
        main_content.append_child(&topbar).unwrap();

        if selected_path.is_none() && storage_error.is_none() {
            let empty_state = document.create_element("div").unwrap();
            empty_state.set_class_name("empty-state");
            empty_state.set_text_content(Some("Select a file or create a new one."));
            main_content.append_child(&empty_state).unwrap();
        }

        if let Some(err) = storage_error {
            let err_div = document.create_element("div").unwrap();
            err_div.set_class_name("storage-error");
            let err_p = document.create_element("p").unwrap();
            err_p.set_text_content(Some(&format!("Local storage error: {}", err)));
            err_div.append_child(&err_p).unwrap();
            main_content.append_child(&err_div).unwrap();
        } else if phase == SessionPhase::HydratingLocal {
            let loading_div = document.create_element("div").unwrap();
            loading_div.set_class_name("loading-state");
            loading_div.set_text_content(Some("Loading local document..."));
            main_content.append_child(&loading_div).unwrap();
        } else if phase == SessionPhase::Closed && selected_path.is_some() {
            let closed_div = document.create_element("div").unwrap();
            closed_div.set_class_name("closed-state");
            closed_div.set_text_content(Some("No document open."));
            main_content.append_child(&closed_div).unwrap();
        }

        if editing_allowed && selected_path.is_some() {
            let is_asset = entries.iter().any(|e| {
                selected_path.as_deref() == Some(e.path.as_str())
                    && e.kind == WorkspaceEntryKind::Asset
            });

            if is_asset {
                let asset_view = document.create_element("div").unwrap();
                asset_view.set_class_name("asset-view");
                if let Some(entry) = entries
                    .iter()
                    .find(|e| selected_path.as_deref() == Some(e.path.as_str()))
                {
                    let name_p = document.create_element("p").unwrap();
                    name_p.set_text_content(Some(&format!(
                        "Name: {}",
                        explorer::file_name_only(entry.path.as_str())
                    )));
                    asset_view.append_child(&name_p).unwrap();
                    let path_p = document.create_element("p").unwrap();
                    path_p.set_text_content(Some(&format!("Path: {}", entry.path.as_str())));
                    asset_view.append_child(&path_p).unwrap();
                    let size_p = document.create_element("p").unwrap();
                    size_p.set_text_content(Some(&format!("Size: {} bytes", entry.size_bytes)));
                    asset_view.append_child(&size_p).unwrap();
                }
                main_content.append_child(&asset_view).unwrap();
            } else {
                let editor_container = document.create_element("div").unwrap();
                editor_container.set_class_name(if inner.document_mode == DocumentMode::Preview {
                    "editor-container preview-reader"
                } else {
                    "editor-container"
                });

                // ── Link message (non-fatal) ────────────────────
                if let Some(ref msg) = inner.link_message {
                    let msg_div = document.create_element("div").unwrap();
                    msg_div.set_class_name("link-message");
                    let msg_span = document.create_element("span").unwrap();
                    msg_span.set_text_content(Some(msg));
                    msg_div.append_child(&msg_span).unwrap();
                    let dismiss_btn = document.create_element("button").unwrap();
                    dismiss_btn.set_class_name("link-message-dismiss");
                    dismiss_btn.set_text_content(Some("×"));
                    msg_div.append_child(&dismiss_btn).unwrap();
                    editor_container.append_child(&msg_div).unwrap();
                }
                if let Some(request) = &inner.missing_note {
                    let prompt = document.create_element("div").unwrap();
                    prompt.set_class_name("link-message missing-note-confirmation");
                    prompt
                        .set_text_content(Some(&format!("Note not found: {}", request.raw_target)));
                    let create = document.create_element("button").unwrap();
                    create.set_class_name("create-missing-note-btn");
                    create.set_text_content(Some("Create note"));
                    let dismiss = document.create_element("button").unwrap();
                    dismiss.set_class_name("dismiss-missing-note-btn");
                    dismiss.set_text_content(Some("Dismiss"));
                    prompt.append_child(&create).unwrap();
                    prompt.append_child(&dismiss).unwrap();
                    editor_container.append_child(&prompt).unwrap();
                }

                let document_header = document.create_element("section").unwrap();
                document_header.set_class_name(if inner.document_mode == DocumentMode::Preview {
                    "preview-document-header"
                } else {
                    "document-header-card"
                });
                if inner.document_mode == DocumentMode::Edit {
                    let title_input = document.create_element("input").unwrap();
                    let title_input: web_sys::HtmlInputElement = title_input.unchecked_into();
                    title_input.set_type("text");
                    title_input.set_class_name("document-title-input");
                    title_input.set_placeholder("Untitled note");
                    title_input.set_value(&view_doc.title);
                    document_header.append_child(&title_input).unwrap();
                } else {
                    let document_type = document.create_element("span").unwrap();
                    document_type.set_class_name("preview-document-type");
                    document_type.set_text_content(Some("MARKDOWN"));
                    document_header.append_child(&document_type).unwrap();
                    let title = document.create_element("h1").unwrap();
                    title.set_id("document-title");
                    title.set_class_name("document-title");
                    title.set_text_content(Some(if view_doc.title.is_empty() {
                        "Untitled note"
                    } else {
                        &view_doc.title
                    }));
                    document_header.append_child(&title).unwrap();
                }

                let metadata = document.create_element("div").unwrap();
                metadata.set_class_name("document-metadata");
                let created_at = view_doc
                    .blocks
                    .iter()
                    .map(|block| block.created_at_ms)
                    .min();
                let updated_at = view_doc
                    .blocks
                    .iter()
                    .map(|block| block.updated_at_ms)
                    .max();
                let mut metadata_values = vec![
                    (
                        "Created",
                        created_at
                            .map(Driver::format_timestamp)
                            .unwrap_or_else(|| "New note".to_string()),
                    ),
                    (
                        "Updated",
                        updated_at
                            .map(Driver::format_timestamp)
                            .unwrap_or_else(|| "No edits yet".to_string()),
                    ),
                ];
                if inner.document_mode == DocumentMode::Edit {
                    metadata_values.push(("Blocks", view_doc.blocks.len().to_string()));
                }
                for (label, value) in metadata_values {
                    let item = document.create_element("div").unwrap();
                    item.set_class_name("metadata-item");
                    let key = document.create_element("span").unwrap();
                    key.set_class_name("metadata-label");
                    key.set_text_content(Some(label));
                    let text = document.create_element("span").unwrap();
                    text.set_class_name("metadata-value");
                    text.set_text_content(Some(&value));
                    item.append_child(&key).unwrap();
                    item.append_child(&text).unwrap();
                    metadata.append_child(&item).unwrap();
                }
                let tags_item = document.create_element("div").unwrap();
                tags_item.set_class_name("metadata-item metadata-tags");
                let tags_label = document.create_element("span").unwrap();
                tags_label.set_class_name("metadata-label");
                tags_label.set_text_content(Some("Tags"));
                tags_item.append_child(&tags_label).unwrap();
                let mut current_tags: Vec<String> = selected_path
                    .as_ref()
                    .and_then(|path| inner.knowledge.path_to_tags.get(path))
                    .map(|tags| tags.iter().cloned().collect())
                    .unwrap_or_default();
                current_tags.sort();
                if current_tags.is_empty() {
                    let empty = document.create_element("span").unwrap();
                    empty.set_class_name("metadata-value metadata-empty");
                    empty.set_text_content(Some("No parsed tags"));
                    tags_item.append_child(&empty).unwrap();
                } else {
                    for tag in current_tags {
                        let chip = document.create_element("button").unwrap();
                        chip.set_class_name("metadata-tag");
                        chip.set_attribute("data-tag", &tag).unwrap();
                        chip.set_text_content(Some(&format!("#{}", tag)));
                        tags_item.append_child(&chip).unwrap();
                    }
                }
                metadata.append_child(&tags_item).unwrap();
                document_header.append_child(&metadata).unwrap();
                if inner.document_mode == DocumentMode::Preview {
                    let rule = document.create_element("hr").unwrap();
                    rule.set_class_name("preview-header-rule");
                    document_header.append_child(&rule).unwrap();
                }
                editor_container.append_child(&document_header).unwrap();

                if inner.document_mode == DocumentMode::Edit {
                    let block_list = document.create_element("div").unwrap();
                    block_list.set_class_name("block-list");

                    for (block_index, block) in view_doc.blocks.iter().enumerate() {
                        let block_id = block.id;
                        let (kind, label) = match &block.content {
                            BlockContent::Markdown { .. } => ("markdown", "MARKDOWN"),
                            BlockContent::Math { .. } => ("math", "MATH"),
                            BlockContent::Ink { .. } => ("ink", "INK"),
                            BlockContent::Divider => ("divider", "DIVIDER"),
                        };
                        let block_wrapper = document.create_element("article").unwrap();
                        let expanded = inner.expanded_ink_block == Some(block_id);
                        let wrapper_class = if expanded {
                            format!("block-wrapper block-{} expanded", kind)
                        } else {
                            format!("block-wrapper block-{}", kind)
                        };
                        block_wrapper.set_class_name(&wrapper_class);
                        block_wrapper
                            .set_attribute("data-block-id", &block_id.to_string())
                            .unwrap();
                        block_wrapper
                            .set_attribute("data-block-type", kind)
                            .unwrap();

                        let block_header = document.create_element("header").unwrap();
                        block_header.set_class_name("block-header");
                        let block_label = document.create_element("span").unwrap();
                        block_label.set_class_name("block-label");
                        block_label.set_text_content(Some(label));
                        block_header.append_child(&block_label).unwrap();
                        let block_actions = document.create_element("div").unwrap();
                        block_actions.set_class_name("block-actions");
                        for (action, text, disabled) in [
                            ("move-up", "Up", block_index == 0),
                            (
                                "move-down",
                                "Down",
                                block_index + 1 == view_doc.blocks.len(),
                            ),
                            ("delete", "Delete", false),
                        ] {
                            let button = document.create_element("button").unwrap();
                            button.set_class_name("block-action-btn");
                            button.set_attribute("data-block-action", action).unwrap();
                            button
                                .set_attribute("data-block-id", &block_id.to_string())
                                .unwrap();
                            button.set_text_content(Some(text));
                            if disabled {
                                button.set_attribute("disabled", "").unwrap();
                            }
                            block_actions.append_child(&button).unwrap();
                        }
                        block_header.append_child(&block_actions).unwrap();
                        block_wrapper.append_child(&block_header).unwrap();

                        match &block.content {
                            BlockContent::Markdown { text } => {
                                let textarea = document.create_element("textarea").unwrap();
                                let textarea: web_sys::HtmlTextAreaElement =
                                    textarea.unchecked_into();
                                textarea.set_class_name("block-textarea markdown-source-input");
                                textarea.set_placeholder("Write Markdown...");
                                textarea
                                    .set_attribute("data-block-id", &block_id.to_string())
                                    .unwrap();
                                textarea
                                    .set_attribute("data-block-kind", "markdown")
                                    .unwrap();
                                textarea.set_value(text);
                                block_wrapper.append_child(&textarea).unwrap();
                            }
                            BlockContent::Math { source } => {
                                let textarea = document.create_element("textarea").unwrap();
                                let textarea: web_sys::HtmlTextAreaElement =
                                    textarea.unchecked_into();
                                textarea.set_class_name("block-textarea math-source-input");
                                textarea.set_placeholder("Enter TeX or mathematical source...");
                                textarea
                                    .set_attribute("data-block-id", &block_id.to_string())
                                    .unwrap();
                                textarea.set_attribute("data-block-kind", "math").unwrap();
                                textarea.set_value(source);
                                block_wrapper.append_child(&textarea).unwrap();
                                let source_preview = document.create_element("div").unwrap();
                                source_preview.set_class_name("math-source-preview");
                                source_preview.set_text_content(Some(if source.is_empty() {
                                    "Mathematical source preview"
                                } else {
                                    source
                                }));
                                block_wrapper.append_child(&source_preview).unwrap();
                            }
                            BlockContent::Ink { .. } => {
                                let ink_block = document.create_element("div").unwrap();
                                ink_block.set_class_name("ink-block");
                                ink_block
                                    .set_attribute("data-block-id", &block_id.to_string())
                                    .unwrap();
                                let toolbar = document.create_element("div").unwrap();
                                toolbar.set_class_name("ink-toolbar");
                                for (tool, text) in [
                                    ("pen", "Pen"),
                                    ("eraser", "Eraser"),
                                    ("lasso", "Lasso"),
                                    ("select", "Select"),
                                ] {
                                    let button = document.create_element("button").unwrap();
                                    button.set_class_name(if inner.ink_tool == tool {
                                        "ink-tool-btn active"
                                    } else {
                                        "ink-tool-btn"
                                    });
                                    button.set_attribute("data-ink-action", "tool").unwrap();
                                    button.set_attribute("data-tool", tool).unwrap();
                                    button
                                        .set_attribute("data-block-id", &block_id.to_string())
                                        .unwrap();
                                    button.set_text_content(Some(text));
                                    toolbar.append_child(&button).unwrap();
                                }
                                for (action, text) in [("undo", "Undo"), ("redo", "Redo")] {
                                    let button = document.create_element("button").unwrap();
                                    button.set_class_name("ink-action-btn");
                                    button.set_attribute("data-ink-action", action).unwrap();
                                    button
                                        .set_attribute("data-block-id", &block_id.to_string())
                                        .unwrap();
                                    button.set_text_content(Some(text));
                                    toolbar.append_child(&button).unwrap();
                                }
                                let width = document.create_element("input").unwrap();
                                let width: web_sys::HtmlInputElement = width.unchecked_into();
                                width.set_type("range");
                                width.set_class_name("ink-width-input");
                                width.set_min("1");
                                width.set_max("24");
                                width.set_step("1");
                                width.set_value(&inner.ink_brush_width.to_string());
                                width.set_attribute("aria-label", "Brush width").unwrap();
                                width.set_attribute("data-ink-action", "width").unwrap();
                                width
                                    .set_attribute("data-block-id", &block_id.to_string())
                                    .unwrap();
                                toolbar.append_child(&width).unwrap();
                                for (name, rgb) in [
                                    ("Ink", "31,41,55"),
                                    ("Blue", "37,99,235"),
                                    ("Red", "190,52,52"),
                                ] {
                                    let color = document.create_element("button").unwrap();
                                    color.set_class_name("ink-color-btn");
                                    color.set_attribute("data-ink-action", "color").unwrap();
                                    color.set_attribute("data-color", rgb).unwrap();
                                    color
                                        .set_attribute("data-block-id", &block_id.to_string())
                                        .unwrap();
                                    color.set_attribute("aria-label", name).unwrap();
                                    color.set_attribute("title", name).unwrap();
                                    toolbar.append_child(&color).unwrap();
                                }
                                let expand = document.create_element("button").unwrap();
                                expand.set_class_name("ink-action-btn ink-expand-btn");
                                expand.set_attribute("data-ink-action", "expand").unwrap();
                                expand
                                    .set_attribute("data-block-id", &block_id.to_string())
                                    .unwrap();
                                expand
                                    .set_attribute(
                                        "aria-label",
                                        if expanded {
                                            "Collapse Ink drawing"
                                        } else {
                                            "Expand Ink drawing"
                                        },
                                    )
                                    .unwrap();
                                expand
                                    .set_attribute(
                                        "aria-expanded",
                                        if expanded { "true" } else { "false" },
                                    )
                                    .unwrap();
                                expand.set_text_content(Some(if expanded {
                                    "Collapse"
                                } else {
                                    "Expand"
                                }));
                                toolbar.append_child(&expand).unwrap();
                                let save_state = document.create_element("span").unwrap();
                                save_state.set_class_name("ink-save-state");
                                save_state
                                    .set_attribute("data-block-id", &block_id.to_string())
                                    .unwrap();
                                save_state.set_attribute("data-state", "saved").unwrap();
                                save_state.set_text_content(Some("Saved locally"));
                                toolbar.append_child(&save_state).unwrap();
                                ink_block.append_child(&toolbar).unwrap();
                                let canvas_host = document.create_element("div").unwrap();
                                canvas_host.set_class_name("ink-canvas-host");
                                let canvas = document.create_element("canvas").unwrap();
                                canvas.set_id(&format!("ink-canvas-{}", block_id));
                                canvas.set_attribute("tabindex", "0").unwrap();
                                canvas
                                    .set_attribute("data-block-id", &block_id.to_string())
                                    .unwrap();
                                canvas
                                    .set_attribute("aria-label", "Stonepen drawing canvas")
                                    .unwrap();
                                canvas_host.append_child(&canvas).unwrap();
                                ink_block.append_child(&canvas_host).unwrap();
                                block_wrapper.append_child(&ink_block).unwrap();
                            }
                            BlockContent::Divider => {
                                let divider = document.create_element("hr").unwrap();
                                divider.set_class_name("document-divider");
                                block_wrapper.append_child(&divider).unwrap();
                            }
                        }
                        block_list.append_child(&block_wrapper).unwrap();
                    }

                    if view_doc.blocks.is_empty() {
                        let empty = document.create_element("div").unwrap();
                        empty.set_class_name("document-empty-state");
                        empty.set_text_content(Some(
                            "Start with a Markdown, Math, Ink, or Divider block.",
                        ));
                        block_list.append_child(&empty).unwrap();
                    }
                    editor_container.append_child(&block_list).unwrap();

                    let insert = document.create_element("div").unwrap();
                    insert.set_class_name("block-insert-control");
                    let insert_label = document.create_element("span").unwrap();
                    insert_label.set_class_name("block-insert-label");
                    insert_label.set_text_content(Some("Add block"));
                    insert.append_child(&insert_label).unwrap();
                    for (kind, text) in [
                        ("markdown", "Markdown"),
                        ("math", "Math"),
                        ("ink", "Ink"),
                        ("divider", "Divider"),
                    ] {
                        let button = document.create_element("button").unwrap();
                        button.set_class_name("add-block-btn");
                        button.set_attribute("data-block-type", kind).unwrap();
                        button.set_text_content(Some(text));
                        insert.append_child(&button).unwrap();
                    }
                    editor_container.append_child(&insert).unwrap();

                    if !client_state.errors().is_empty() {
                        let error_list = document.create_element("div").unwrap();
                        error_list.set_class_name("error-list");
                        for err in client_state.errors() {
                            let err_item = document.create_element("div").unwrap();
                            err_item.set_class_name("error-item");
                            err_item.set_text_content(Some(err));
                            error_list.append_child(&err_item).unwrap();
                        }
                        editor_container.append_child(&error_list).unwrap();
                    }
                } else {
                    let preview_div = document.create_element("div").unwrap();
                    preview_div.set_class_name("preview-content document-mode-preview");
                    let mut heading_counts = std::collections::HashMap::new();
                    for block in &view_doc.blocks {
                        let preview_block = document.create_element("section").unwrap();
                        preview_block.set_class_name("preview-block");
                        preview_block
                            .set_attribute("data-block-id", &block.id.to_string())
                            .unwrap();
                        match &block.content {
                            BlockContent::Markdown { text } => {
                                preview_block.set_class_name("preview-block preview-markdown");
                                preview_block
                                    .set_attribute("data-block-type", "markdown")
                                    .unwrap();
                                let html = markdown_render::render_with_knowledge_and_counts(
                                    text,
                                    selected_path.as_deref().unwrap_or(""),
                                    &inner.knowledge,
                                    &mut heading_counts,
                                );
                                preview_block.set_inner_html(&html);
                            }
                            BlockContent::Math { source } => {
                                preview_block.set_class_name("preview-block preview-math");
                                preview_block
                                    .set_attribute("data-block-type", "math")
                                    .unwrap();
                                let label = document.create_element("span").unwrap();
                                label.set_class_name("preview-block-label");
                                label.set_text_content(Some("EQUATION"));
                                let math = document.create_element("div").unwrap();
                                math.set_class_name("math-display");
                                math.set_inner_html(&math_render::render_mathml(source));
                                preview_block.append_child(&label).unwrap();
                                preview_block.append_child(&math).unwrap();
                            }
                            BlockContent::Ink { .. } => {
                                let expanded = inner.expanded_ink_block == Some(block.id);
                                preview_block.set_class_name(if expanded {
                                    "preview-block preview-ink expanded"
                                } else {
                                    "preview-block preview-ink"
                                });
                                preview_block
                                    .set_attribute("data-block-type", "ink")
                                    .unwrap();
                                let header = document.create_element("header").unwrap();
                                header.set_class_name("preview-ink-header");
                                let label = document.create_element("span").unwrap();
                                label.set_class_name("preview-block-label");
                                label.set_text_content(Some("INK"));
                                header.append_child(&label).unwrap();
                                let expand = document.create_element("button").unwrap();
                                expand.set_class_name("preview-ink-expand");
                                expand.set_attribute("data-ink-action", "expand").unwrap();
                                expand
                                    .set_attribute("data-block-id", &block.id.to_string())
                                    .unwrap();
                                expand
                                    .set_attribute(
                                        "aria-label",
                                        if expanded {
                                            "Collapse Ink drawing"
                                        } else {
                                            "Expand Ink drawing"
                                        },
                                    )
                                    .unwrap();
                                expand
                                    .set_attribute(
                                        "aria-expanded",
                                        if expanded { "true" } else { "false" },
                                    )
                                    .unwrap();
                                expand.set_text_content(Some(if expanded {
                                    "Collapse"
                                } else {
                                    "Expand"
                                }));
                                header.append_child(&expand).unwrap();
                                preview_block.append_child(&header).unwrap();
                                let ink_block = document.create_element("div").unwrap();
                                ink_block.set_class_name("ink-block ink-preview");
                                ink_block
                                    .set_attribute("data-block-id", &block.id.to_string())
                                    .unwrap();
                                let canvas_host = document.create_element("div").unwrap();
                                canvas_host.set_class_name("ink-canvas-host");
                                let canvas = document.create_element("canvas").unwrap();
                                canvas.set_id(&format!("ink-canvas-{}", block.id));
                                canvas
                                    .set_attribute("data-block-id", &block.id.to_string())
                                    .unwrap();
                                canvas.set_attribute("role", "img").unwrap();
                                canvas
                                    .set_attribute("aria-label", "Read-only Stonepen drawing")
                                    .unwrap();
                                canvas_host.append_child(&canvas).unwrap();
                                ink_block.append_child(&canvas_host).unwrap();
                                preview_block.append_child(&ink_block).unwrap();
                            }
                            BlockContent::Divider => {
                                preview_block.set_class_name("preview-block preview-divider");
                                preview_block
                                    .set_attribute("data-block-type", "divider")
                                    .unwrap();
                                let divider = document.create_element("hr").unwrap();
                                divider.set_class_name("document-divider");
                                preview_block.append_child(&divider).unwrap();
                            }
                        }
                        preview_div.append_child(&preview_block).unwrap();
                    }
                    editor_container.append_child(&preview_div).unwrap();
                }

                main_content.append_child(&editor_container).unwrap();
            }
        } else if phase == SessionPhase::Error {
            let err_state = document.create_element("div").unwrap();
            err_state.set_class_name("error-state");
            let errors = client_state.errors();
            if errors.is_empty() {
                let p = document.create_element("p").unwrap();
                p.set_text_content(Some("The document entered an error state."));
                err_state.append_child(&p).unwrap();
            } else {
                let ul = document.create_element("ul").unwrap();
                for err in errors {
                    let li = document.create_element("li").unwrap();
                    li.set_text_content(Some(err));
                    ul.append_child(&li).unwrap();
                }
                err_state.append_child(&ul).unwrap();
            }
            main_content.append_child(&err_state).unwrap();
        }

        app_container.append_child(&main_content).unwrap();

        let inspector = document.create_element("div").unwrap();
        if inner.inspector_open {
            inspector.set_class_name("inspector open");
        } else {
            inspector.set_class_name("inspector");
        }

        let tabs_div = document.create_element("div").unwrap();
        tabs_div.set_class_name("inspector-tabs");

        for tab_name in &["Backlinks", "Outline", "Tags", "Graph"] {
            let tab = document.create_element("button").unwrap();
            if &inner.knowledge_tab == tab_name {
                tab.set_class_name("inspector-tab active");
            } else {
                tab.set_class_name("inspector-tab");
            }
            tab.set_attribute("data-tab", tab_name).unwrap();
            tab.set_text_content(Some(tab_name));
            tabs_div.append_child(&tab).unwrap();
        }
        let inspector_close = document.create_element("button").unwrap();
        inspector_close.set_class_name("toggle-inspector-btn inspector-close-btn");
        inspector_close.set_text_content(Some("Hide"));
        tabs_div.append_child(&inspector_close).unwrap();
        inspector.append_child(&tabs_div).unwrap();

        let section = document.create_element("div").unwrap();
        section.set_class_name("inspector-section");
        section
            .set_attribute("data-section", &inner.knowledge_tab.to_lowercase())
            .unwrap();

        if inner.knowledge_tab == "Outline" {
            if let Some(path) = selected_path {
                if let Some(knowledge) = inner.knowledge.path_to_doc.get(path) {
                    for heading in &knowledge.headings {
                        let item = document.create_element("div").unwrap();
                        item.set_class_name("outline-item");
                        item.set_text_content(Some(&heading.text));
                        item.set_attribute("data-anchor", &heading.anchor).unwrap();
                        item.set_attribute("data-block-id", &heading.block_id.to_string())
                            .unwrap();
                        item.set_attribute("data-heading-line", &heading.line.to_string())
                            .unwrap();
                        item.set_attribute("data-heading-level", &heading.level.to_string())
                            .unwrap();
                        let pad = if heading.level > 0 {
                            (heading.level - 1) * 12
                        } else {
                            0
                        };
                        item.set_attribute("style", &format!("padding-left: {}px", pad))
                            .unwrap();
                        section.append_child(&item).unwrap();
                    }
                    if knowledge.headings.is_empty() {
                        let empty = document.create_element("div").unwrap();
                        empty.set_class_name("outline-item");
                        empty.set_text_content(Some("No headings"));
                        empty
                            .set_attribute("style", "color: var(--text-muted); cursor: default;")
                            .unwrap();
                        section.append_child(&empty).unwrap();
                    }
                }
            }
        } else if inner.knowledge_tab == "Backlinks" {
            // Outgoing Links
            let out_header = document.create_element("h4").unwrap();
            out_header.set_text_content(Some("Outgoing Links"));
            section.append_child(&out_header).unwrap();

            if let Some(path) = selected_path {
                if let Some(knowledge) = inner.knowledge.path_to_doc.get(path) {
                    let mut has_links = false;
                    for outlink in &knowledge.outgoing_links {
                        has_links = true;
                        let item = document.create_element("div").unwrap();
                        let resolved_path = if outlink.is_wikilink {
                            inner.knowledge.resolve_wikilink(path, &outlink.target_raw)
                        } else {
                            inner
                                .knowledge
                                .resolve_markdown_link(path, &outlink.target_raw)
                        };
                        if let knowledge::WikilinkResolution::Resolved(resolved) = resolved_path {
                            item.set_class_name("link-item");
                            item.set_text_content(Some(
                                outlink.alias.as_deref().unwrap_or(&outlink.target_raw),
                            ));
                            item.set_attribute("data-path", &resolved).unwrap();
                        } else if matches!(
                            resolved_path,
                            knowledge::WikilinkResolution::Ambiguous(_)
                        ) {
                            item.set_class_name("link-item ambiguous");
                            item.set_text_content(Some(
                                outlink.alias.as_deref().unwrap_or(&outlink.target_raw),
                            ));
                        } else {
                            item.set_class_name("link-item missing");
                            item.set_text_content(Some(&outlink.target_raw));
                            item.set_attribute("data-missing-target", &outlink.target_raw)
                                .unwrap();
                        }
                        section.append_child(&item).unwrap();
                    }
                    if !has_links {
                        let empty = document.create_element("div").unwrap();
                        empty.set_class_name("outline-item");
                        empty.set_text_content(Some("No outgoing links"));
                        empty
                            .set_attribute("style", "color: var(--text-muted); cursor: default;")
                            .unwrap();
                        section.append_child(&empty).unwrap();
                    }
                }
            }

            // Backlinks
            let back_header = document.create_element("h4").unwrap();
            back_header.set_text_content(Some("Backlinks"));
            section.append_child(&back_header).unwrap();

            if let Some(path) = selected_path {
                let backlinks = inner.knowledge.get_backlinks(path);
                if backlinks.is_empty() {
                    let empty = document.create_element("div").unwrap();
                    empty.set_class_name("outline-item");
                    empty.set_text_content(Some("No backlinks"));
                    empty
                        .set_attribute("style", "color: var(--text-muted); cursor: default;")
                        .unwrap();
                    section.append_child(&empty).unwrap();
                } else {
                    for backlink in backlinks {
                        let item = document.create_element("div").unwrap();
                        item.set_class_name("link-item");
                        item.set_text_content(Some(&backlink.source_name));
                        item.set_attribute("data-path", &backlink.source_path)
                            .unwrap();
                        let detail = document.create_element("div").unwrap();
                        detail.set_class_name("backlink-context");
                        detail.set_text_content(Some(&format!(
                            "{}\n{}",
                            backlink.source_path,
                            backlink.context_snippet.unwrap_or_default()
                        )));
                        item.append_child(&detail).unwrap();
                        section.append_child(&item).unwrap();
                    }
                }
            }
        } else if inner.knowledge_tab == "Tags" {
            let current = selected_path
                .as_ref()
                .and_then(|path| inner.knowledge.path_to_tags.get(path));
            let current_header = document.create_element("h4").unwrap();
            current_header.set_text_content(Some("Current Note Tags"));
            section.append_child(&current_header).unwrap();
            if let Some(tags) = current {
                let mut tags: Vec<&String> = tags.iter().collect();
                tags.sort();
                for tag in tags {
                    let item = document.create_element("div").unwrap();
                    item.set_class_name("tag-item");
                    item.set_attribute("data-tag", tag).unwrap();
                    item.set_text_content(Some(&format!("#{}", tag)));
                    section.append_child(&item).unwrap();
                }
            }
            let workspace_header = document.create_element("h4").unwrap();
            workspace_header.set_text_content(Some("Workspace Tags"));
            section.append_child(&workspace_header).unwrap();
            let tags = inner.knowledge.get_all_tags_with_counts();
            if tags.is_empty() {
                let empty = document.create_element("div").unwrap();
                empty.set_class_name("outline-item");
                empty.set_text_content(Some("No tags in workspace"));
                empty
                    .set_attribute("style", "color: var(--text-muted); cursor: default;")
                    .unwrap();
                section.append_child(&empty).unwrap();
            } else {
                for (tag, count) in tags {
                    let item = document.create_element("div").unwrap();
                    item.set_class_name("tag-item");
                    item.set_attribute("data-tag", &tag).unwrap();

                    let name_span = document.create_element("span").unwrap();
                    name_span.set_class_name("tag-name");
                    name_span.set_text_content(Some(&format!("#{}", tag)));

                    let count_span = document.create_element("span").unwrap();
                    count_span.set_class_name("tag-count");
                    count_span.set_text_content(Some(&count.to_string()));

                    item.append_child(&name_span).unwrap();
                    item.append_child(&count_span).unwrap();
                    section.append_child(&item).unwrap();
                }
            }
        } else if inner.knowledge_tab == "Graph" {
            let graph = selected_path
                .as_ref()
                .map(|path| local_graph::build_local_graph(&inner.knowledge, path))
                .unwrap_or_default();
            let graph_header = document.create_element("div").unwrap();
            graph_header.set_class_name("graph-header");
            graph_header.set_text_content(Some("Active note neighborhood"));
            section.append_child(&graph_header).unwrap();
            if graph.nodes.len() <= 1 {
                let empty = document.create_element("div").unwrap();
                empty.set_class_name("graph-empty-state");
                empty.set_text_content(Some("No linked notes yet. Add a link or backlink."));
                section.append_child(&empty).unwrap();
            }

            let svg_ns = Some("http://www.w3.org/2000/svg");
            let graph_wrap = document.create_element("div").unwrap();
            graph_wrap.set_class_name("local-graph-wrap");
            let svg = document.create_element_ns(svg_ns, "svg").unwrap();
            svg.set_attribute("class", "local-graph").unwrap();
            svg.set_attribute("viewBox", "0 0 300 264").unwrap();
            svg.set_attribute("role", "img").unwrap();
            svg.set_attribute("aria-label", "Linked note graph")
                .unwrap();
            for edge in &graph.edges {
                let Some(source) = graph.nodes.iter().find(|node| node.path == edge.source) else {
                    continue;
                };
                let Some(target) = graph.nodes.iter().find(|node| node.path == edge.target) else {
                    continue;
                };
                let line = document.create_element_ns(svg_ns, "line").unwrap();
                line.set_attribute("class", "local-graph-edge").unwrap();
                line.set_attribute("x1", &source.x.to_string()).unwrap();
                line.set_attribute("y1", &source.y.to_string()).unwrap();
                line.set_attribute("x2", &target.x.to_string()).unwrap();
                line.set_attribute("y2", &target.y.to_string()).unwrap();
                line.set_attribute("data-source", &edge.source).unwrap();
                line.set_attribute("data-target", &edge.target).unwrap();
                svg.append_child(&line).unwrap();
            }
            for node in &graph.nodes {
                let group = document.create_element_ns(svg_ns, "g").unwrap();
                let kind = match node.kind {
                    local_graph::GraphNodeKind::Active => "active",
                    local_graph::GraphNodeKind::Outgoing => "outgoing",
                    local_graph::GraphNodeKind::Backlink => "backlink",
                    local_graph::GraphNodeKind::Connected => "connected",
                };
                group
                    .set_attribute("class", &format!("local-graph-node {}", kind))
                    .unwrap();
                group.set_attribute("data-path", &node.path).unwrap();
                group.set_attribute("data-graph-path", &node.path).unwrap();
                let circle = document.create_element_ns(svg_ns, "circle").unwrap();
                circle.set_attribute("cx", &node.x.to_string()).unwrap();
                circle.set_attribute("cy", &node.y.to_string()).unwrap();
                circle.set_attribute("data-graph-path", &node.path).unwrap();
                circle
                    .set_attribute(
                        "r",
                        if node.kind == local_graph::GraphNodeKind::Active {
                            "27"
                        } else {
                            "13"
                        },
                    )
                    .unwrap();
                group.append_child(&circle).unwrap();
                let text = document.create_element_ns(svg_ns, "text").unwrap();
                text.set_attribute("x", &node.x.to_string()).unwrap();
                text.set_attribute("y", &(node.y + 30.0).to_string())
                    .unwrap();
                text.set_attribute("text-anchor", "middle").unwrap();
                let label: String = node.label.chars().take(18).collect();
                text.set_text_content(Some(&label));
                group.append_child(&text).unwrap();
                svg.append_child(&group).unwrap();
            }
            graph_wrap.append_child(&svg).unwrap();
            for node in &graph.nodes {
                let hit_target = document.create_element("button").unwrap();
                hit_target.set_class_name("graph-node-hit");
                hit_target
                    .set_attribute("data-graph-path", &node.path)
                    .unwrap();
                hit_target.set_attribute("data-path", &node.path).unwrap();
                hit_target
                    .set_attribute("aria-label", &format!("Open {}", node.label))
                    .unwrap();
                let size = if node.kind == local_graph::GraphNodeKind::Active {
                    58
                } else {
                    32
                };
                hit_target
                    .set_attribute(
                        "style",
                        &format!(
                            "left:{}%;top:{}%;width:{}px;height:{}px",
                            node.x / 3.0,
                            node.y / 2.64,
                            size,
                            size
                        ),
                    )
                    .unwrap();
                graph_wrap.append_child(&hit_target).unwrap();
            }
            section.append_child(&graph_wrap).unwrap();
        }

        inspector.append_child(&section).unwrap();
        app_container.append_child(&inspector).unwrap();
        if inner.sidebar_open || inner.inspector_open {
            let backdrop = document.create_element("button").unwrap();
            backdrop.set_class_name("drawer-backdrop");
            backdrop
                .set_attribute("aria-label", "Close panels")
                .unwrap();
            app_container.append_child(&backdrop).unwrap();
        }
        root.append_child(&app_container).unwrap();
        self.mount_ink_blocks(
            view_doc,
            &inner.ink_tool,
            inner.ink_brush_width,
            inner.ink_brush_color,
            inner.document_mode == DocumentMode::Preview,
        );

        // Focus rename input if in rename mode
        if inner.renaming_path.is_some() {
            if let Some(el) = document.query_selector(".rename-input").unwrap() {
                let input: web_sys::HtmlInputElement = el.unchecked_into();
                let _ = input.focus();
            }
        } else if inner.show_create {
            if let Some(el) = document.query_selector(".explorer-create-input").unwrap() {
                let input: web_sys::HtmlInputElement = el.unchecked_into();
                let _ = input.focus();
            }
        } else if let Some((target, start, end)) = focus_info {
            if target == "search" {
                // Restore search input focus and cursor
                if let Some(el) = document.query_selector(".search-input").unwrap() {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    let _ = input.focus();
                    let _ = input.set_selection_start(Some(start));
                    let _ = input.set_selection_end(Some(end));
                }
            } else if editing_allowed {
                if target == "title" {
                    if let Some(el) = document.query_selector(".document-title-input").unwrap() {
                        let input: web_sys::HtmlInputElement = el.unchecked_into();
                        let _ = input.focus();
                        let _ = input.set_selection_start(Some(start));
                        let _ = input.set_selection_end(Some(end));
                    }
                } else if target.starts_with("block:") {
                    let block_id = target.strip_prefix("block:").unwrap();
                    let selector = format!(".block-textarea[data-block-id=\"{}\"]", block_id);
                    if let Some(el) = document.query_selector(&selector).unwrap() {
                        let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                        let _ = textarea.focus();
                        let _ = textarea.set_selection_start(Some(start));
                        let _ = textarea.set_selection_end(Some(end));
                    }
                }
            }
        }
        RENDERING.with(|rendering| rendering.set(false));
    }

    fn format_timestamp(timestamp_ms: i64) -> String {
        let date = js_sys::Date::new(&JsValue::from_f64(timestamp_ms as f64));
        let iso = String::from(date.to_iso_string());
        iso.chars().take(16).collect::<String>().replace('T', " ")
    }

    // Safe DOM text helper – appends text nodes and <mark> elements; no innerHTML.
    // Unicode-safe: uses char_indices so byte slicing is always on valid boundaries.
    fn highlight_text_into(el: &web_sys::Element, text: &str, query: &str) {
        let document = match web_sys::window().and_then(|w| w.document()) {
            Some(d) => d,
            None => return,
        };
        if query.is_empty() {
            let _ = el.append_child(&document.create_text_node(text));
            return;
        }
        let query_lower = query.to_lowercase();
        let mut byte_pos: usize = 0;
        for (start, end) in search::find_case_insensitive_ranges(text, &query_lower) {
            if start > byte_pos {
                let _ = el.append_child(&document.create_text_node(&text[byte_pos..start]));
            }
            let mark = match document.create_element("mark") {
                Ok(m) => m,
                Err(_) => return,
            };
            mark.set_class_name("search-highlight");
            let _ = mark.append_child(&document.create_text_node(&text[start..end]));
            let _ = el.append_child(&mark);
            byte_pos = end;
        }
        if byte_pos < text.len() {
            let _ = el.append_child(&document.create_text_node(&text[byte_pos..]));
        }
    }
}

fn setup_event_delegation(driver: &Driver) -> Result<(), JsValue> {
    let window = web_sys::window().expect("No global window found");
    let document = window.document().expect("No global document found");
    let root = document
        .get_element_by_id("root")
        .expect("No element with id 'root' found");

    let d_input = driver.clone();
    let on_input = Closure::wrap(Box::new(move |event: web_sys::InputEvent| {
        let target = event.target();
        if let Some(target) = target {
            if let Ok(el) = target.dyn_into::<web_sys::HtmlElement>() {
                let class = el.class_name();

                // ── Explorer rename input ────────────────────────
                if class == "rename-input" {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    d_input.0.borrow_mut().rename_value = input.value();
                    return;
                }

                // ── Explorer create input ────────────────────────
                if class == "explorer-create-input" {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    d_input.0.borrow_mut().create_value = input.value();
                    return;
                }

                // ── Search input ─────────────────────────────────
                if class == "search-input" {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    d_input.update_search_query(&input.value());
                    return;
                }

                if class == "ink-width-input" {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    let Some(block_id) = input
                        .get_attribute("data-block-id")
                        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                        .map(BlockId)
                    else {
                        return;
                    };
                    let width = input.value_as_number() as f32;
                    d_input.0.borrow_mut().ink_brush_width = width;
                    d_input.1.borrow_mut().set_brush_width(block_id, width);
                    return;
                }

                let editing_allowed = {
                    let inner = d_input.0.borrow();
                    let state = &inner.client_state;
                    let phase = state.phase();
                    inner.storage_error.is_none()
                        && phase != SessionPhase::HydratingLocal
                        && phase != SessionPhase::Closed
                        && phase != SessionPhase::Error
                };
                if !editing_allowed {
                    return;
                }

                if class == "document-title-input" {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    let new_title = input.value();

                    let update_id = UpdateId::new_v4();
                    let applied_at_ms = js_sys::Date::now() as i64;

                    d_input.dispatch(ClientEvent::LocalIntent {
                        op: DocumentOp::SetTitle { title: new_title },
                        update_id,
                        applied_at_ms,
                    });
                } else if el.class_list().contains("block-textarea") {
                    let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                    let text = textarea.value();
                    let block_kind = textarea
                        .get_attribute("data-block-kind")
                        .unwrap_or_else(|| "markdown".to_string());
                    if let Some(block_id_str) = textarea.get_attribute("data-block-id") {
                        if let Ok(uuid) = uuid::Uuid::parse_str(&block_id_str) {
                            let block_id = BlockId(uuid);
                            let update_id = UpdateId::new_v4();
                            let applied_at_ms = js_sys::Date::now() as i64;

                            let content = if block_kind == "math" {
                                BlockContent::Math { source: text }
                            } else {
                                BlockContent::Markdown { text }
                            };
                            d_input.dispatch(ClientEvent::LocalIntent {
                                op: DocumentOp::ReplaceBlockContent { block_id, content },
                                update_id,
                                applied_at_ms,
                            });
                        }
                    }
                }
            }
        }
    }) as Box<dyn FnMut(web_sys::InputEvent)>);

    root.add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())?;
    on_input.forget();

    let d_click = driver.clone();
    let click_document = document.clone();
    let visibility_document = document.clone();
    let on_click = Closure::wrap(Box::new(move |event: web_sys::MouseEvent| {
        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::Element>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.get_attribute("class").unwrap_or_default();
        let editing_allowed = {
            let inner = d_click.0.borrow();
            let has_err = inner.storage_error.is_some();
            !has_err
        };

        if !editing_allowed {
            return;
        }

        if let Some(path) = el.get_attribute("data-graph-path") {
            d_click.select_entry(&path);
            return;
        }

        if class == "sidebar-toggle-btn" {
            let mut inner = d_click.0.borrow_mut();
            inner.sidebar_open = !inner.sidebar_open;
            drop(inner);
            d_click.render();
            return;
        }
        if class == "drawer-backdrop" {
            let mut inner = d_click.0.borrow_mut();
            inner.sidebar_open = false;
            inner.inspector_open = false;
            drop(inner);
            d_click.render();
            return;
        }
        if class == "topbar-search-btn" {
            let mobile = web_sys::window()
                .and_then(|window| window.inner_width().ok())
                .and_then(|width| width.as_f64())
                .is_some_and(|width| width <= 760.0);
            if mobile {
                d_click.0.borrow_mut().sidebar_open = true;
                d_click.render();
            }
            if let Ok(Some(search)) = click_document.query_selector(".search-input") {
                if let Ok(input) = search.dyn_into::<web_sys::HtmlInputElement>() {
                    let _ = input.focus();
                    let _ = input.select();
                }
            }
            return;
        }

        // ── Search & Recent Clicks ──────────────────────────────
        if class.contains("search-result-item")
            || el
                .parent_element()
                .map(|p| p.class_name().contains("search-result-item"))
                .unwrap_or(false)
        {
            let path = el.get_attribute("data-path").or_else(|| {
                el.parent_element()
                    .and_then(|p| p.get_attribute("data-path"))
            });
            if let Some(p) = path {
                d_click.select_entry(&p);
                d_click.search_clear_query_only();
            }
            return;
        }
        if class.contains("recent-item")
            || el
                .parent_element()
                .map(|p| p.class_name().contains("recent-item"))
                .unwrap_or(false)
        {
            let path = el.get_attribute("data-path").or_else(|| {
                el.parent_element()
                    .and_then(|p| p.get_attribute("data-path"))
            });
            if let Some(p) = path {
                d_click.select_entry(&p);
            }
            return;
        }
        if class == "recent-clear-btn" {
            d_click.clear_recent_notes();
            return;
        }

        // ── Explorer: new file button (root level) ─────────────
        if class == "explorer-new-btn" {
            let mut inner = d_click.0.borrow_mut();
            inner.show_create = true;
            inner.create_value = String::new();
            inner.create_folder = None;
            drop(inner);
            d_click.render();
            return;
        }

        // ── Explorer: folder target (click row to target + expand) ──
        if class == "explorer-folder" || class == "explorer-folder expanded" {
            if let Some(path) = el.get_attribute("data-path") {
                let mut inner = d_click.0.borrow_mut();
                inner.expanded_folders.insert(path.clone());
                inner.create_folder = Some(path);
                inner.show_create = true;
                inner.create_value = String::new();
                drop(inner);
                d_click.render();
            }
            return;
        }
        if class == "explorer-toggle" {
            if let Some(parent) = el.parent_element() {
                if let Some(path) = parent.get_attribute("data-path") {
                    let mut inner = d_click.0.borrow_mut();
                    if inner.expanded_folders.contains(&path) {
                        inner.expanded_folders.remove(&path);
                    } else {
                        inner.expanded_folders.insert(path);
                    }
                    drop(inner);
                    d_click.render();
                }
            }
            return;
        }

        // ── Workspace: file entry click (select) ──────────────────
        if class == "file-entry" || class == "file-entry active" || class == "file-name" {
            let path = el.get_attribute("data-path").or_else(|| {
                el.parent_element()
                    .and_then(|p| p.get_attribute("data-path"))
            });
            if let Some(p) = path {
                d_click.select_entry(&p);
            }
            return;
        }

        // ── Workspace: rename button (inline rename) ─────────────
        if class == "rename-btn" {
            if let Some(path) = el.get_attribute("data-path") {
                let mut inner = d_click.0.borrow_mut();
                let file_name = explorer::file_name_only(&path);
                inner.renaming_path = Some(path);
                inner.rename_value = file_name;
                drop(inner);
                d_click.render();
            }
            return;
        }

        // ── Workspace: delete button ──────────────────────────────
        if class == "delete-btn" {
            if let Some(path) = el.get_attribute("data-path") {
                if let Some(window) = web_sys::window() {
                    let confirmed = window
                        .confirm_with_message(&format!(
                            "Delete \"{}\"? This cannot be undone.",
                            path
                        ))
                        .unwrap_or(false);
                    if confirmed {
                        d_click.delete_entry(&path);
                    }
                }
            }
            return;
        }

        if let Ok(Some(action_element)) = el.closest("[data-block-action]") {
            let Some(block_id) = action_element
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
            else {
                return;
            };
            let action = action_element
                .get_attribute("data-block-action")
                .unwrap_or_default();
            let block_position = {
                let inner = d_click.0.borrow();
                let document = inner.client_state.effective_view_doc();
                document
                    .block_index(block_id)
                    .map(|index| (index, document.blocks.len()))
            };
            let Some((index, block_count)) = block_position else {
                return;
            };

            d_click.flush_ink_before_transition();
            let op = match action.as_str() {
                "delete" => Some(DocumentOp::DeleteBlock { block_id }),
                "move-up" if index > 0 => Some(DocumentOp::MoveBlock {
                    block_id,
                    new_idx: index - 1,
                }),
                "move-down" if index + 1 < block_count => Some(DocumentOp::MoveBlock {
                    block_id,
                    new_idx: index + 1,
                }),
                _ => None,
            };
            if let Some(op) = op {
                d_click.dispatch(ClientEvent::LocalIntent {
                    op,
                    update_id: UpdateId::new_v4(),
                    applied_at_ms: js_sys::Date::now() as i64,
                });
                d_click.render();
            }
            return;
        }

        if let Ok(Some(action_element)) = el.closest("[data-ink-action]") {
            let Some(block_id) = action_element
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
            else {
                return;
            };
            let action = action_element
                .get_attribute("data-ink-action")
                .unwrap_or_default();
            match action.as_str() {
                "tool" => {
                    let tool = action_element
                        .get_attribute("data-tool")
                        .unwrap_or_else(|| "pen".to_string());
                    let (width, color) = {
                        let mut inner = d_click.0.borrow_mut();
                        inner.ink_tool = tool.clone();
                        (inner.ink_brush_width, inner.ink_brush_color)
                    };
                    {
                        let mut runtimes = d_click.1.borrow_mut();
                        runtimes.set_tool(block_id, &tool);
                        runtimes.set_brush_width(block_id, width);
                        runtimes.set_brush_color(block_id, color);
                    }
                    let active_selector =
                        format!(".ink-tool-btn.active[data-block-id=\"{}\"]", block_id);
                    if let Ok(Some(active)) = click_document.query_selector(&active_selector) {
                        active.set_class_name("ink-tool-btn");
                    }
                    action_element.set_class_name("ink-tool-btn active");
                }
                "color" => {
                    let color = action_element
                        .get_attribute("data-color")
                        .and_then(|value| {
                            let mut channels = value
                                .split(',')
                                .filter_map(|channel| channel.parse::<u8>().ok());
                            Some((channels.next()?, channels.next()?, channels.next()?))
                        });
                    if let Some(color) = color {
                        d_click.0.borrow_mut().ink_brush_color = color;
                        d_click.1.borrow_mut().set_brush_color(block_id, color);
                    }
                }
                "undo" => d_click.undo_ink_block(block_id),
                "redo" => d_click.redo_ink_block(block_id),
                "expand" => {
                    let expanded = {
                        let mut inner = d_click.0.borrow_mut();
                        if inner.expanded_ink_block == Some(block_id) {
                            inner.expanded_ink_block = None;
                            false
                        } else {
                            inner.expanded_ink_block = Some(block_id);
                            true
                        }
                    };
                    if let Ok(Some(wrapper)) =
                        action_element.closest(".block-wrapper, .preview-block")
                    {
                        if let Ok(Some(main_content)) = wrapper.closest(".main-content") {
                            if expanded {
                                let _ = main_content.class_list().add_1("ink-expanded");
                            } else {
                                let _ = main_content.class_list().remove_1("ink-expanded");
                            }
                        }
                        if expanded {
                            let _ = wrapper.class_list().add_1("expanded");
                            action_element.set_text_content(Some("Collapse"));
                            let _ =
                                action_element.set_attribute("aria-label", "Collapse Ink drawing");
                            let _ = action_element.set_attribute("aria-expanded", "true");
                        } else {
                            let _ = wrapper.class_list().remove_1("expanded");
                            action_element.set_text_content(Some("Expand"));
                            let _ =
                                action_element.set_attribute("aria-label", "Expand Ink drawing");
                            let _ = action_element.set_attribute("aria-expanded", "false");
                        }
                    }
                    d_click.1.borrow_mut().resize(block_id);
                }
                _ => {}
            }
            return;
        }

        // ── Mode toggle ────────────────────────────────────
        if class == "mode-toggle-btn" {
            let is_preview = el.get_attribute("data-mode").as_deref() == Some("preview");
            d_click.set_document_mode(if is_preview {
                DocumentMode::Preview
            } else {
                DocumentMode::Edit
            });
            return;
        }

        if class
            .split_whitespace()
            .any(|name| name == "toggle-inspector-btn")
        {
            let mut inner = d_click.0.borrow_mut();
            inner.inspector_open = !inner.inspector_open;
            drop(inner);
            d_click.render();
            return;
        }

        // ── Preview internal link ─────────────────────────
        let workspace_path = el.get_attribute("data-workspace-path").or_else(|| {
            el.closest("[data-workspace-path]")
                .ok()
                .flatten()
                .and_then(|p| p.get_attribute("data-workspace-path"))
        });
        if let Some(ref target_path) = workspace_path {
            event.prevent_default();
            d_click.navigate_to_link(target_path);
            return;
        }

        let missing_target = el.get_attribute("data-missing-target").or_else(|| {
            el.closest("[data-missing-target]")
                .ok()
                .flatten()
                .and_then(|p| p.get_attribute("data-missing-target"))
        });
        if let Some(target) = missing_target {
            event.prevent_default();
            let source = d_click.0.borrow().selected_path.clone().unwrap_or_default();
            d_click.request_missing_note(&target, &source);
            return;
        }

        // ── Dismiss link message ──────────────────────────
        if class == "link-message-dismiss" {
            d_click.0.borrow_mut().link_message = None;
            d_click.render();
            return;
        }

        // ── Inspector Tab Switch ───────────────────────────
        if class == "inspector-tab" || class == "inspector-tab active" {
            if let Some(tab_name) = el.get_attribute("data-tab") {
                let mut inner = d_click.0.borrow_mut();
                inner.knowledge_tab = tab_name;
                drop(inner);
                d_click.render();
            }
            return;
        }

        if class == "clear-tag-btn" {
            d_click.search_clear();
            return;
        }

        // ── Inspector Outline Click ────────────────────────
        if class == "outline-item" {
            let inner = d_click.0.borrow();
            let is_preview = inner.document_mode == DocumentMode::Preview;
            drop(inner);

            if is_preview {
                if let Some(anchor) = el.get_attribute("data-anchor") {
                    if let Some(heading_el) = document.get_element_by_id(&anchor) {
                        heading_el.scroll_into_view();
                    }
                }
            } else {
                if let Some(block_id) = el.get_attribute("data-block-id") {
                    let selector = format!(".block-textarea[data-block-id=\"{}\"]", block_id);
                    if let Ok(Some(textarea_el)) = click_document.query_selector(&selector) {
                        textarea_el.scroll_into_view();
                        if let Ok(textarea) = textarea_el.dyn_into::<web_sys::HtmlTextAreaElement>()
                        {
                            let _ = textarea.focus();
                            if let Some(line_str) = el.get_attribute("data-heading-line") {
                                if let Ok(line) = line_str.parse::<usize>() {
                                    let text = textarea.value();
                                    let mut current_line = 0;
                                    let mut offset = 0;
                                    for ch in text.chars() {
                                        if current_line == line {
                                            break;
                                        }
                                        offset += ch.len_utf16() as u32;
                                        if ch == '\n' {
                                            current_line += 1;
                                        }
                                    }
                                    let _ = textarea.set_selection_start(Some(offset));
                                    let _ = textarea.set_selection_end(Some(offset));
                                }
                            }
                        }
                    }
                }
            }
            el.set_class_name("outline-item active");
            return;
        }

        // ── Inspector Link Click ───────────────────────────
        let inspector_link_path = el.get_attribute("data-path").or_else(|| {
            el.closest(".link-item[data-path]")
                .ok()
                .flatten()
                .and_then(|row| row.get_attribute("data-path"))
        });
        if let Some(path) = inspector_link_path {
            d_click.select_entry(&path);
            return;
        }

        if class == "create-missing-note-btn" {
            let path = d_click
                .0
                .borrow_mut()
                .missing_note
                .take()
                .map(|request| request.proposed_path);
            if let Some(path) = path {
                d_click.create_file(&path);
            }
            return;
        }
        if class == "dismiss-missing-note-btn" {
            d_click.0.borrow_mut().missing_note = None;
            d_click.render();
            return;
        }

        // ── Inspector Tag Click ────────────────────────────
        if class == "tag-item"
            || class == "tag-name"
            || class == "tag-count"
            || class == "metadata-tag"
        {
            let tag = el.get_attribute("data-tag").or_else(|| {
                el.parent_element()
                    .and_then(|p| p.get_attribute("data-tag"))
            });
            if let Some(t) = tag {
                let mut inner = d_click.0.borrow_mut();
                inner.tag_filter = Some(t.to_lowercase());
                inner.search_query.clear();
                inner.search_results.clear();
                drop(inner);
                d_click.render();
            }
            return;
        }

        // ── Block insertion ───────────────────────────────────────
        let editor_editing_allowed = {
            let inner = d_click.0.borrow();
            let state = &inner.client_state;
            let phase = state.phase();
            inner.storage_error.is_none()
                && phase != SessionPhase::HydratingLocal
                && phase != SessionPhase::Closed
                && phase != SessionPhase::Error
                && inner.selected_path.is_some()
        };
        if !editor_editing_allowed {
            return;
        }
        if class == "add-block-btn" {
            let update_id = UpdateId::new_v4();
            let applied_at_ms = js_sys::Date::now() as i64;
            let block_id = BlockId::new_v4();
            let block_type = el
                .get_attribute("data-block-type")
                .unwrap_or_else(|| "markdown".to_string());
            let content = match block_type.as_str() {
                "math" => BlockContent::Math {
                    source: String::new(),
                },
                "ink" => BlockContent::Ink {
                    stonepen_json: String::new(),
                },
                "divider" => BlockContent::Divider,
                _ => BlockContent::Markdown {
                    text: String::new(),
                },
            };
            let block = inkstone_core::document::DocumentBlock {
                id: block_id,
                created_at_ms: applied_at_ms,
                updated_at_ms: applied_at_ms,
                content,
            };

            let insert_idx = d_click
                .0
                .borrow()
                .client_state
                .effective_view_doc()
                .blocks
                .len();

            d_click.dispatch(ClientEvent::LocalIntent {
                op: DocumentOp::InsertBlock {
                    idx: insert_idx,
                    block,
                },
                update_id,
                applied_at_ms,
            });
            d_click.render();
            return;
        }
    }) as Box<dyn FnMut(web_sys::MouseEvent)>);

    root.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())?;
    on_click.forget();

    let d_pointerup = driver.clone();
    let on_pointerup = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        if !element.id().starts_with("ink-canvas-") {
            return;
        }
        if let Some(block_id) = element
            .get_attribute("data-block-id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        {
            d_pointerup.save_ink_block(block_id);
        }
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    root.add_event_listener_with_callback("pointerup", on_pointerup.as_ref().unchecked_ref())?;
    on_pointerup.forget();

    let d_pointercancel = driver.clone();
    let on_pointercancel = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        if !element.id().starts_with("ink-canvas-") {
            return;
        }
        if let Some(block_id) = element
            .get_attribute("data-block-id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        {
            d_pointercancel.save_ink_block(block_id);
        }
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    root.add_event_listener_with_callback(
        "pointercancel",
        on_pointercancel.as_ref().unchecked_ref(),
    )?;
    on_pointercancel.forget();

    let d_visibility = driver.clone();
    let visibility_target = visibility_document.clone();
    let on_visibility = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if visibility_document.hidden() {
            d_visibility.save_all_ink_blocks();
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    visibility_target.add_event_listener_with_callback(
        "visibilitychange",
        on_visibility.as_ref().unchecked_ref(),
    )?;
    on_visibility.forget();

    let d_keydown = driver.clone();
    let on_keydown = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
        if event.is_composing() {
            return;
        }
        if event.key() == "Escape" {
            let expanded = d_keydown.0.borrow_mut().expanded_ink_block.take();
            if let Some(block_id) = expanded {
                event.prevent_default();
                if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                    let selector = format!(".expanded[data-block-id=\"{}\"]", block_id);
                    if let Ok(Some(wrapper)) = document.query_selector(&selector) {
                        let _ = wrapper.class_list().remove_1("expanded");
                        if let Ok(Some(main_content)) = wrapper.closest(".main-content") {
                            let _ = main_content.class_list().remove_1("ink-expanded");
                        }
                        if let Ok(Some(button)) =
                            wrapper.query_selector("[data-ink-action=\"expand\"]")
                        {
                            button.set_text_content(Some("Expand"));
                            let _ = button.set_attribute("aria-label", "Expand Ink drawing");
                            let _ = button.set_attribute("aria-expanded", "false");
                            if let Ok(button) = button.dyn_into::<web_sys::HtmlElement>() {
                                let _ = button.focus();
                            }
                        }
                    }
                }
                d_keydown.1.borrow_mut().resize(block_id);
                return;
            }
        }
        // Global Ctrl+K / Cmd+K check
        let is_k = event.key() == "k" || event.key() == "K";
        let is_ctrl_cmd = event.ctrl_key() || event.meta_key();
        if is_ctrl_cmd && is_k {
            event.prevent_default();
            let mobile = web_sys::window()
                .and_then(|window| window.inner_width().ok())
                .and_then(|width| width.as_f64())
                .is_some_and(|width| width <= 760.0);
            if mobile {
                d_keydown.0.borrow_mut().sidebar_open = true;
                d_keydown.render();
            }
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    if let Some(search_el) = document.query_selector(".search-input").unwrap() {
                        if let Ok(search_input) = search_el.dyn_into::<web_sys::HtmlInputElement>()
                        {
                            let _ = search_input.focus();
                            let _ = search_input.select();
                        }
                    }
                }
            }
            return;
        }

        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::HtmlElement>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.class_name();

        // ── Search input keydown ──────────────────────────────────
        if class == "search-input" {
            if event.key() == "ArrowDown" {
                event.prevent_default();
                d_keydown.search_move_highlight(1);
            } else if event.key() == "ArrowUp" {
                event.prevent_default();
                d_keydown.search_move_highlight(-1);
            } else if event.key() == "Enter" {
                event.prevent_default();
                d_keydown.search_open_highlighted();
            } else if event.key() == "Escape" {
                event.prevent_default();
                d_keydown.search_clear();
            }
            return;
        }

        // ── Create input ──────────────────────────────────────────
        if class == "explorer-create-input" {
            let input: web_sys::HtmlInputElement = el.unchecked_into();
            if event.key() == "Enter" {
                event.prevent_default();
                let val = input.value();
                if !val.trim().is_empty() {
                    d_keydown.create_file(&val);
                }
            } else if event.key() == "Escape" {
                event.prevent_default();
                d_keydown.0.borrow_mut().show_create = false;
                d_keydown.0.borrow_mut().create_value = String::new();
                d_keydown.0.borrow_mut().create_folder = None;
                d_keydown.render();
            }
            return;
        }

        // ── Rename input ─────────────────────────────────────────
        if class == "rename-input" {
            let input: web_sys::HtmlInputElement = el.unchecked_into();
            if event.key() == "Enter" {
                event.prevent_default();
                let new_name = input.value();
                let old_path = d_keydown.0.borrow().renaming_path.clone();
                if let Some(ref path) = old_path {
                    if !new_name.trim().is_empty() {
                        // Update path using parent folder
                        let full_new_path = if let Some(parent) = explorer::parent_folder(path) {
                            if parent.is_empty() {
                                new_name.trim().to_string()
                            } else {
                                format!("{}/{}", parent, new_name.trim())
                            }
                        } else {
                            new_name.trim().to_string()
                        };
                        d_keydown.rename_entry(path, &full_new_path);
                    }
                }
            } else if event.key() == "Escape" {
                event.prevent_default();
                d_keydown.0.borrow_mut().renaming_path = None;
                d_keydown.0.borrow_mut().rename_value = String::new();
                d_keydown.render();
            }
            return;
        }
    }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
    root.add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())?;
    on_keydown.forget();

    let on_focusout = Closure::wrap(Box::new(move |event: web_sys::FocusEvent| {
        if RENDERING.with(|rendering| rendering.get()) {
            return;
        }
        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::HtmlElement>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.class_name();
        if class != "document-title-input" && !el.class_list().contains("block-textarea") {
            return;
        }
        if let Some(related) = event.related_target() {
            if let Ok(rel_el) = related.dyn_into::<web_sys::HtmlElement>() {
                let rel_class = rel_el.class_name();
                if rel_class == "document-title-input"
                    || rel_el.class_list().contains("block-textarea")
                {
                    return;
                }
            }
        }
        // Input dispatch persists canonical document changes. Rendering here would
        // replace the clicked control during its mousedown/focusout sequence.
    }) as Box<dyn FnMut(web_sys::FocusEvent)>);

    root.add_event_listener_with_callback("focusout", on_focusout.as_ref().unchecked_ref())?;
    on_focusout.forget();

    Ok(())
}

thread_local! {
    static DRIVER: RefCell<Option<Driver>> = RefCell::new(None);
}

#[wasm_bindgen]
pub fn close_workspace_db() {
    DRIVER.with(|d| {
        if let Some(ref driver) = *d.borrow() {
            if let Some(ref ws) = driver.0.borrow().workspace_db {
                ws.close();
            }
        }
    });
}

#[wasm_bindgen]
pub fn close_local_db() {
    DRIVER.with(|d| {
        if let Some(ref driver) = *d.borrow() {
            let mut inner = driver.0.borrow_mut();
            if let Some(ref db) = inner.db {
                db.close();
            }
            inner.db = None;
        }
    });
}

#[wasm_bindgen(start)]
pub fn start_app() -> Result<(), JsValue> {
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));

    let driver = Driver::new(DocId::new_v4());
    DRIVER.with(|d| *d.borrow_mut() = Some(driver.clone()));
    setup_event_delegation(&driver)?;

    let d = driver.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match WorkspaceDb::open().await {
            Ok(ws) => {
                d.0.borrow_mut().workspace_db = Some(ws.clone());

                // Load recent notes from DB on startup – fail closed.
                // Missing = empty (Ok), malformed = storage error.
                match ws.get_recent_notes().await {
                    Ok(recents) => {
                        d.0.borrow_mut().recent_notes = recents;
                    }
                    Err(e) => {
                        d.0.borrow_mut().storage_error =
                            Some(format!("Failed to load recent notes: {:?}", e));
                        d.render();
                        return;
                    }
                }

                d.refresh_entries();
            }
            Err(e) => {
                d.0.borrow_mut().storage_error = Some(format!("Failed to open workspace: {:?}", e));
                d.render();
            }
        }
    });

    Ok(())
}
