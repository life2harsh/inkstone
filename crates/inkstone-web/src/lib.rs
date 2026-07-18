pub mod explorer;
pub mod indexed_db;
pub mod storage;
pub mod workspace_db;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use inkstone_client::{ClientEvent, ClientState, Effect, SessionPhase};
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::ids::{BlockId, DocId, UpdateId};
use inkstone_workspace::{WorkspaceEntry, WorkspaceEntryKind, WorkspacePath, WorkspaceStore};

use indexed_db::IndexedDb;
use workspace_db::WorkspaceDb;

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
}

#[derive(Clone)]
pub struct Driver(Rc<RefCell<DriverInner>>);

impl Driver {
    pub fn new(doc_id: DocId) -> Self {
        Driver(Rc::new(RefCell::new(DriverInner {
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
        })))
    }

    fn select_entry(&self, path: &str) {
        let this = self.clone();
        let path_str = path.to_string();
        let ws = self.0.borrow().workspace_db.clone();
        let ws = match ws {
            Some(w) => w,
            None => return,
        };
        {
            let mut inner = self.0.borrow_mut();
            inner.renaming_path = None;
            inner.show_create = false;
        }
        wasm_bindgen_futures::spawn_local(async move {
            match ws.get_doc_id(&path_str).await {
                Ok(Some(existing_id)) => {
                    let doc_id = DocId(existing_id);
                    {
                        let mut inner = this.0.borrow_mut();
                        inner.selected_doc_id = doc_id;
                        inner.selected_path = Some(path_str);
                        inner.client_state = ClientState::new(doc_id);
                    }
                    this.dispatch(ClientEvent::OpenDocument { doc_id });
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
                    {
                        let mut inner = this.0.borrow_mut();
                        inner.selected_doc_id = doc_id;
                        inner.selected_path = Some(path_str);
                        inner.client_state = ClientState::new(doc_id);
                    }
                    this.dispatch(ClientEvent::OpenDocument { doc_id });
                }
                Err(e) => {
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
        let path = match WorkspacePath::new(&name) {
            Ok(p) => p,
            Err(_) => {
                self.0.borrow_mut().storage_error = Some(format!("Invalid file name: {}", name));
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
                        this.refresh_entries();
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
            if is_selected {
                this.0.borrow_mut().selected_path = None;
            }
            this.refresh_entries();
            this.render();
        });
    }

    fn rename_entry(&self, old_path: &str, new_name: &str) {
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
        let old_path_wp = match WorkspacePath::new(old_path) {
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
        let is_selected = self.0.borrow().selected_path.as_deref() == Some(old_path);
        wasm_bindgen_futures::spawn_local(async move {
            match ws.rename_file(&old_path_wp, &new_path).await {
                Ok(()) => {
                    if is_selected {
                        this.0.borrow_mut().selected_path = Some(new_name.clone());
                    }
                    this.0.borrow_mut().renaming_path = None;
                    this.refresh_entries();
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
                    this.render();
                }
                Err(e) => {
                    this.0.borrow_mut().storage_error =
                        Some(format!("List entries failed: {:?}", e));
                    this.render();
                }
            }
        });
    }

    pub fn dispatch(&self, event: ClientEvent) {
        let effects = {
            let mut inner = self.0.borrow_mut();
            inner.client_state.transition(event)
        };

        // Suppress rerendering if there are in-flight user inputs
        // Also skip full DOM rebuild when user is actively editing (input/textarea focused).
        // Without this, render() calls set_inner_html("") which destroys the focused element,
        // and subsequent keystrokes end up on the wrong target.
        let should_render = {
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
    }

    fn has_editable_focus(&self) -> bool {
        if let Some(window) = web_sys::window() {
            if let Some(document) = window.document() {
                if let Some(el) = document.active_element() {
                    let class = el.class_name();
                    return class == "document-title-input" || class == "block-textarea";
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

                    match db.get_document_record(&doc_id.to_string()).await {
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
                    let (db, doc_id) = {
                        let inner = this.0.borrow();
                        (inner.db.clone(), inner.selected_doc_id)
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
                                    this.dispatch(ClientEvent::LocalUpdatePersisted { update_id });
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
                    let (db, doc_id) = {
                        let inner = this.0.borrow();
                        (inner.db.clone(), inner.selected_doc_id)
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
                                    this.dispatch(ClientEvent::ConfirmationPersisted { token });
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
        let window = web_sys::window().expect("No global window found");
        let document = window.document().expect("No global document found");
        let root = document
            .get_element_by_id("root")
            .expect("No element with id 'root' found");

        let active_element = document.active_element();
        let focus_info = active_element.and_then(|el| {
            if el.class_name() == "document-title-input" {
                let input: web_sys::HtmlInputElement = el.unchecked_into();
                Some((
                    "title".to_string(),
                    input.selection_start().ok().flatten().unwrap_or(0),
                    input.selection_end().ok().flatten().unwrap_or(0),
                ))
            } else if el.class_name() == "block-textarea" {
                let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                let block_id = textarea.get_attribute("data-block-id").unwrap_or_default();
                Some((
                    format!("block:{}", block_id),
                    textarea.selection_start().ok().flatten().unwrap_or(0),
                    textarea.selection_end().ok().flatten().unwrap_or(0),
                ))
            } else {
                None
            }
        });

        root.set_inner_html("");

        let inner = self.0.borrow();
        let client_state = &inner.client_state;
        let view_doc = client_state.view_doc();
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
        app_container.set_class_name("app-container");

        let sidebar = document.create_element("div").unwrap();
        sidebar.set_class_name("sidebar");

        let brand = document.create_element("div").unwrap();
        brand.set_class_name("brand");
        brand.set_text_content(Some("Inkstone"));
        sidebar.append_child(&brand).unwrap();

        let sidebar_heading = document.create_element("div").unwrap();
        sidebar_heading.set_class_name("sidebar-heading");
        sidebar_heading.set_text_content(Some("Files"));
        sidebar.append_child(&sidebar_heading).unwrap();

        let file_list = document.create_element("div").unwrap();
        file_list.set_class_name("file-list");

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
                let folder_class = if flat_node.is_expanded {
                    "explorer-folder expanded"
                } else {
                    "explorer-folder"
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
        main_content.set_class_name("main-content");

        if selected_path.is_none() && storage_error.is_none() {
            let empty_state = document.create_element("div").unwrap();
            empty_state.set_class_name("empty-state");
            empty_state.set_text_content(Some("Select a file or create a new one."));
            main_content.append_child(&empty_state).unwrap();
        }

        if let Some(path) = selected_path {
            let breadcrumb = document.create_element("div").unwrap();
            breadcrumb.set_class_name("breadcrumb");
            breadcrumb.set_text_content(Some(path));
            main_content.append_child(&breadcrumb).unwrap();
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
                editor_container.set_class_name("editor-container");

                let title_input = document.create_element("input").unwrap();
                let title_input: web_sys::HtmlInputElement = title_input.unchecked_into();
                title_input.set_type("text");
                title_input.set_class_name("document-title-input");
                title_input.set_placeholder("Document Title");
                title_input.set_value(&view_doc.title);
                editor_container.append_child(&title_input).unwrap();

                let block_list = document.create_element("div").unwrap();
                block_list.set_class_name("block-list");

                for block in &view_doc.blocks {
                    let block_id = block.id;
                    let block_wrapper = document.create_element("div").unwrap();
                    block_wrapper.set_class_name("block-wrapper");

                    let block_label = document.create_element("div").unwrap();
                    block_label.set_class_name("block-label");
                    block_label.set_text_content(Some("MARKDOWN"));
                    block_wrapper.append_child(&block_label).unwrap();

                    let textarea = document.create_element("textarea").unwrap();
                    let textarea: web_sys::HtmlTextAreaElement = textarea.unchecked_into();
                    textarea.set_class_name("block-textarea");
                    textarea.set_placeholder("Write markdown here...");
                    textarea
                        .set_attribute("data-block-id", &block_id.to_string())
                        .unwrap();

                    if let inkstone_core::document::BlockContent::Markdown { text } = &block.content
                    {
                        textarea.set_value(text);
                    }

                    block_wrapper.append_child(&textarea).unwrap();
                    block_list.append_child(&block_wrapper).unwrap();
                }

                editor_container.append_child(&block_list).unwrap();

                let add_btn = document.create_element("button").unwrap();
                let add_btn: web_sys::HtmlElement = add_btn.unchecked_into();
                add_btn.set_class_name("add-block-btn");
                add_btn.set_text_content(Some("+ Add Markdown Block"));
                editor_container.append_child(&add_btn).unwrap();

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
        root.append_child(&app_container).unwrap();

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
        } else if editing_allowed {
            if let Some((target, start, end)) = focus_info {
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
                } else if class == "block-textarea" {
                    let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                    let text = textarea.value();
                    if let Some(block_id_str) = textarea.get_attribute("data-block-id") {
                        if let Ok(uuid) = uuid::Uuid::parse_str(&block_id_str) {
                            let block_id = BlockId(uuid);
                            let update_id = UpdateId::new_v4();
                            let applied_at_ms = js_sys::Date::now() as i64;

                            d_input.dispatch(ClientEvent::LocalIntent {
                                op: DocumentOp::ReplaceBlockContent {
                                    block_id,
                                    content: inkstone_core::document::BlockContent::Markdown {
                                        text,
                                    },
                                },
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
    let on_click = Closure::wrap(Box::new(move |event: web_sys::MouseEvent| {
        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::HtmlElement>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.class_name();
        let editing_allowed = {
            let inner = d_click.0.borrow();
            let has_err = inner.storage_error.is_some();
            !has_err
        };

        if !editing_allowed {
            return;
        }

        // ── Explorer: new file button ──────────────────────────
        if class == "explorer-new-btn" {
            d_click.0.borrow_mut().show_create = true;
            d_click.0.borrow_mut().create_value = String::new();
            d_click.render();
            return;
        }

        // ── Explorer: folder expand/collapse ────────────────────
        if class == "explorer-folder" || class == "explorer-folder expanded" {
            if let Some(path) = el.get_attribute("data-path") {
                let mut inner = d_click.0.borrow_mut();
                if inner.expanded_folders.contains(&path) {
                    inner.expanded_folders.remove(&path);
                } else {
                    inner.expanded_folders.insert(path);
                }
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

        // ── Existing: add block button ────────────────────────────
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

            let block = inkstone_core::document::DocumentBlock::new(
                block_id,
                inkstone_core::document::BlockContent::Markdown {
                    text: "".to_string(),
                },
            );

            let insert_idx = d_click.0.borrow().client_state.view_doc().blocks.len();

            d_click.dispatch(ClientEvent::LocalIntent {
                op: DocumentOp::InsertBlock {
                    idx: insert_idx,
                    block,
                },
                update_id,
                applied_at_ms,
            });
        }
    }) as Box<dyn FnMut(web_sys::MouseEvent)>);

    root.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())?;
    on_click.forget();

    let d_keydown = driver.clone();
    let on_keydown = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::HtmlElement>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.class_name();

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

    let d_focusout = driver.clone();
    let on_focusout = Closure::wrap(Box::new(move |event: web_sys::FocusEvent| {
        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::HtmlElement>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.class_name();
        if class != "document-title-input" && class != "block-textarea" {
            return;
        }
        if let Some(related) = event.related_target() {
            if let Ok(rel_el) = related.dyn_into::<web_sys::HtmlElement>() {
                let rel_class = rel_el.class_name();
                if rel_class == "document-title-input" || rel_class == "block-textarea" {
                    return;
                }
            }
        }
        let needs_render = {
            let inner = d_focusout.0.borrow();
            !inner.client_state.has_pending_intent() && inner.client_state.intent_queue_len() == 0
        };
        if needs_render {
            d_focusout.render();
        }
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
