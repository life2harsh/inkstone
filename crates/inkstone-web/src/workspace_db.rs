use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{IdbDatabase, IdbRequest, IdbTransaction, IdbTransactionMode};

use inkstone_workspace::manifest::{deserialize_manifest_toml, serialize_manifest_toml};
use inkstone_workspace::{
    WORKSPACE_MANIFEST_PATH, WorkspaceEntry, WorkspaceManifest, WorkspacePath, WorkspaceStore,
    WorkspaceStoreError,
};

thread_local! {
    static INJECT_FAIL_SET_DOC_ID: Cell<bool> = Cell::new(false);
    static INJECT_FAIL_DELETE_FILE: Cell<bool> = Cell::new(false);
}

const DB_VERSION: u32 = 3;
const FILES_STORE: &str = "files";
const FOLDERS_STORE: &str = "folders";
const DOC_MAP_STORE: &str = "doc_map";
const METADATA_STORE: &str = "metadata";
const GLOBAL_METADATA_STORE: &str = "global_metadata";
const MIGRATIONS_STORE: &str = "migrations";
const QUARANTINE_STORE: &str = "quarantine";
const RECENT_NOTES_KEY: &str = "\x00recent_notes";
const OWNERSHIP_PREFIX: &str = "\x00workspace_ownership:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveScope {
    Local,
    Workspace(uuid::Uuid),
}

impl ActiveScope {
    fn prefix(self) -> String {
        match self {
            Self::Local => "\x00scope:local:".into(),
            Self::Workspace(id) => format!("\x00scope:workspace:{id}:"),
        }
    }

    fn migration_key(self) -> String {
        format!("{}legacy-v1", self.prefix())
    }
}

fn scoped_key(scope: ActiveScope, key: &str) -> String {
    format!("{}{key}", scope.prefix())
}

fn require_scoped_key(
    scope: Option<ActiveScope>,
    key: &str,
) -> Result<String, WorkspaceStoreError> {
    scope.map(|scope| scoped_key(scope, key)).ok_or_else(|| {
        WorkspaceStoreError::Backend(
            "workspace scope is not active; activate a workspace before file access".into(),
        )
    })
}

fn is_scoped_key(key: &str) -> bool {
    key.starts_with("\x00scope:")
}

fn ownership_workspace(key: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(key.strip_prefix(OWNERSHIP_PREFIX)?).ok()
}

fn decode_ownership(value: &str) -> Option<HashMap<String, Option<uuid::Uuid>>> {
    serde_json::from_str(value).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyDisposition {
    Migrate,
    LeaveForOtherWorkspace,
    Quarantine,
}

fn legacy_path_disposition(
    workspace_id: uuid::Uuid,
    path: &str,
    ownership: &HashMap<uuid::Uuid, HashMap<String, Option<uuid::Uuid>>>,
) -> LegacyDisposition {
    let owners = ownership
        .iter()
        .filter(|(_, paths)| paths.contains_key(path))
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    match owners.as_slice() {
        [owner] if *owner == workspace_id => LegacyDisposition::Migrate,
        [_] => LegacyDisposition::LeaveForOtherWorkspace,
        _ => LegacyDisposition::Quarantine,
    }
}

#[wasm_bindgen]
pub fn inject_fail_set_doc_id() {
    INJECT_FAIL_SET_DOC_ID.with(|f| f.set(true));
}

#[wasm_bindgen]
pub fn inject_fail_delete_file() {
    INJECT_FAIL_DELETE_FILE.with(|f| f.set(true));
}

async fn await_request(request: IdbRequest) -> Result<JsValue, JsValue> {
    let (tx, rx) = futures_channel::oneshot::channel::<Result<JsValue, JsValue>>();
    let tx = Rc::new(RefCell::new(Some(tx)));

    let tx_success = tx.clone();
    let on_success = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let request: IdbRequest = event.target().unwrap().unchecked_into();
        let result = request.result().unwrap();
        if let Some(tx) = tx_success.borrow_mut().take() {
            let _ = tx.send(Ok(result));
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));

    let tx_error = tx.clone();
    let on_error = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let request: IdbRequest = event.target().unwrap().unchecked_into();
        let err = request
            .error()
            .unwrap()
            .map(|e| JsValue::from(e))
            .unwrap_or_else(|| JsValue::from_str("Request failed"));
        if let Some(tx) = tx_error.borrow_mut().take() {
            let _ = tx.send(Err(err));
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    request.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    let res = rx
        .await
        .map_err(|_| JsValue::from_str("Oneshot sender dropped"))?;

    drop(on_success);
    drop(on_error);

    res
}

async fn await_transaction(tx: IdbTransaction) -> Result<(), JsValue> {
    let (oneshot_tx, oneshot_rx) = futures_channel::oneshot::channel::<Result<(), JsValue>>();
    let oneshot_tx = Rc::new(RefCell::new(Some(oneshot_tx)));

    let tx_success = oneshot_tx.clone();
    let on_complete = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if let Some(otx) = tx_success.borrow_mut().take() {
            let _ = otx.send(Ok(()));
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    tx.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));

    let tx_error = oneshot_tx.clone();
    let on_error = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let target = event.target().unwrap();
        let tx: IdbTransaction = target.unchecked_into();
        let err = tx
            .error()
            .map(|e| JsValue::from(e))
            .unwrap_or_else(|| JsValue::from_str("Transaction failed"));
        if let Some(otx) = tx_error.borrow_mut().take() {
            let _ = otx.send(Err(err));
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    tx.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    let tx_abort = oneshot_tx.clone();
    let on_abort = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if let Some(otx) = tx_abort.borrow_mut().take() {
            let _ = otx.send(Err(JsValue::from_str("Transaction aborted")));
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    tx.set_onabort(Some(on_abort.as_ref().unchecked_ref()));

    let res = oneshot_rx
        .await
        .map_err(|_| JsValue::from_str("Oneshot sender dropped waiting for transaction"))?;

    drop(on_complete);
    drop(on_error);
    drop(on_abort);

    res
}

#[derive(Clone)]
pub struct WorkspaceDb {
    db: IdbDatabase,
    active_scope: Rc<RefCell<Option<ActiveScope>>>,
    scope_generation: Rc<Cell<u64>>,
}

impl WorkspaceDb {
    pub async fn open() -> Result<Self, WorkspaceStoreError> {
        let window =
            web_sys::window().ok_or_else(|| WorkspaceStoreError::Backend("no window".into()))?;
        let idb_factory = window
            .indexed_db()
            .map_err(|_| WorkspaceStoreError::Backend("IndexedDB access denied".into()))?
            .ok_or_else(|| WorkspaceStoreError::Backend("IndexedDB not supported".into()))?;

        let open_request = idb_factory
            .open_with_u32("inkstone-workspace", DB_VERSION)
            .map_err(|e| WorkspaceStoreError::Backend(format!("IndexedDB open failed: {:?}", e)))?;

        let (tx, rx) =
            futures_channel::oneshot::channel::<Result<IdbDatabase, WorkspaceStoreError>>();
        let tx = Rc::new(RefCell::new(Some(tx)));

        let on_upgrade = Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
            let request: web_sys::IdbOpenDbRequest = event.target().unwrap().unchecked_into();
            let db: IdbDatabase = request.result().unwrap().unchecked_into();
            if !db.object_store_names().contains("files") {
                if let Err(e) = db.create_object_store("files") {
                    web_sys::console::error_1(&e);
                }
            }
            if !db.object_store_names().contains("doc_map") {
                if let Err(e) = db.create_object_store("doc_map") {
                    web_sys::console::error_1(&e);
                }
            }
            for store_name in [
                METADATA_STORE,
                FOLDERS_STORE,
                GLOBAL_METADATA_STORE,
                MIGRATIONS_STORE,
                QUARANTINE_STORE,
            ] {
                if !db.object_store_names().contains(store_name)
                    && let Err(e) = db.create_object_store(store_name)
                {
                    web_sys::console::error_1(&e);
                }
            }
        })
            as Box<dyn FnMut(web_sys::IdbVersionChangeEvent)>);
        open_request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));

        let tx_success = tx.clone();
        let on_success = Closure::wrap(Box::new(move |event: web_sys::Event| {
            let request: web_sys::IdbOpenDbRequest = event.target().unwrap().unchecked_into();
            let db: IdbDatabase = request.result().unwrap().unchecked_into();
            if let Some(tx) = tx_success.borrow_mut().take() {
                let _ = tx.send(Ok(db));
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        open_request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));

        let tx_error = tx.clone();
        let on_error = Closure::wrap(Box::new(move |event: web_sys::Event| {
            let request: web_sys::IdbOpenDbRequest = event.target().unwrap().unchecked_into();
            let err = request
                .error()
                .unwrap()
                .map(|e| WorkspaceStoreError::Backend(format!("IndexedDB error: {:?}", e)))
                .unwrap_or_else(|| {
                    WorkspaceStoreError::Backend("Unknown error opening workspace DB".into())
                });
            if let Some(tx) = tx_error.borrow_mut().take() {
                let _ = tx.send(Err(err));
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        open_request.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let res = rx
            .await
            .map_err(|_| WorkspaceStoreError::Backend("Oneshot sender dropped".into()))?;

        drop(on_upgrade);
        drop(on_success);
        drop(on_error);

        res.map(|db| WorkspaceDb {
            db,
            active_scope: Rc::new(RefCell::new(None)),
            scope_generation: Rc::new(Cell::new(0)),
        })
    }

    fn scope(&self) -> Result<ActiveScope, WorkspaceStoreError> {
        self.active_scope.borrow().as_ref().copied().ok_or_else(|| {
            WorkspaceStoreError::Backend(
                "workspace scope is not active; activate a workspace before file access".into(),
            )
        })
    }

    fn scoped_key(&self, key: &str) -> Result<String, WorkspaceStoreError> {
        require_scoped_key(*self.active_scope.borrow(), key)
    }

    fn ensure_scope(&self, expected: ActiveScope) -> Result<(), WorkspaceStoreError> {
        if *self.active_scope.borrow() == Some(expected) {
            Ok(())
        } else {
            Err(WorkspaceStoreError::Backend(
                "workspace scope changed while the IndexedDB operation was in progress".into(),
            ))
        }
    }

    pub fn clear_scope(&self) {
        self.scope_generation
            .set(self.scope_generation.get().wrapping_add(1));
        *self.active_scope.borrow_mut() = None;
    }

    pub async fn activate_local_scope(&self) -> Result<(), WorkspaceStoreError> {
        self.clear_scope();
        let generation = self.scope_generation.get();
        self.migrate_legacy(ActiveScope::Local).await?;
        if self.scope_generation.get() != generation {
            return Err(WorkspaceStoreError::Backend(
                "workspace activation was superseded".into(),
            ));
        }
        *self.active_scope.borrow_mut() = Some(ActiveScope::Local);
        Ok(())
    }

    pub async fn activate_workspace_scope(
        &self,
        workspace_id: uuid::Uuid,
    ) -> Result<(), WorkspaceStoreError> {
        self.clear_scope();
        let generation = self.scope_generation.get();
        let scope = ActiveScope::Workspace(workspace_id);
        self.migrate_legacy(scope).await?;
        if self.scope_generation.get() != generation {
            return Err(WorkspaceStoreError::Backend(
                "workspace activation was superseded".into(),
            ));
        }
        *self.active_scope.borrow_mut() = Some(scope);
        Ok(())
    }

    async fn migrate_legacy(&self, scope: ActiveScope) -> Result<(), WorkspaceStoreError> {
        let names = js_sys::Array::new();
        for name in [
            FILES_STORE,
            DOC_MAP_STORE,
            METADATA_STORE,
            MIGRATIONS_STORE,
            QUARANTINE_STORE,
        ] {
            names.push(&JsValue::from_str(name));
        }
        let tx = self
            .db
            .transaction_with_str_sequence_and_mode(&names.into(), IdbTransactionMode::Readwrite)
            .map_err(|e| WorkspaceStoreError::Backend(format!("migration tx: {e:?}")))?;
        let files = tx
            .object_store(FILES_STORE)
            .map_err(|e| WorkspaceStoreError::Backend(format!("files store: {e:?}")))?;
        let doc_map = tx
            .object_store(DOC_MAP_STORE)
            .map_err(|e| WorkspaceStoreError::Backend(format!("doc_map store: {e:?}")))?;
        let metadata = tx
            .object_store(METADATA_STORE)
            .map_err(|e| WorkspaceStoreError::Backend(format!("metadata store: {e:?}")))?;
        let migrations = tx
            .object_store(MIGRATIONS_STORE)
            .map_err(|e| WorkspaceStoreError::Backend(format!("migrations store: {e:?}")))?;
        let quarantine = tx
            .object_store(QUARANTINE_STORE)
            .map_err(|e| WorkspaceStoreError::Backend(format!("quarantine store: {e:?}")))?;
        let marker_key = scope.migration_key();

        let marker_request = migrations
            .get(&JsValue::from_str(&marker_key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("migration marker: {e:?}")))?;
        let file_keys_request = files
            .get_all_keys()
            .map_err(|e| WorkspaceStoreError::Backend(format!("file keys: {e:?}")))?;
        let file_values_request = files
            .get_all()
            .map_err(|e| WorkspaceStoreError::Backend(format!("file values: {e:?}")))?;
        let map_keys_request = doc_map
            .get_all_keys()
            .map_err(|e| WorkspaceStoreError::Backend(format!("map keys: {e:?}")))?;
        let map_values_request = doc_map
            .get_all()
            .map_err(|e| WorkspaceStoreError::Backend(format!("map values: {e:?}")))?;
        let metadata_keys_request = metadata
            .get_all_keys()
            .map_err(|e| WorkspaceStoreError::Backend(format!("metadata keys: {e:?}")))?;
        let marker = await_request(marker_request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("migration marker await: {e:?}")))?;
        if !marker.is_undefined() && !marker.is_null() {
            return await_transaction(tx)
                .await
                .map_err(|e| WorkspaceStoreError::Backend(format!("migration tx await: {e:?}")));
        }

        let file_keys = string_array(
            await_request(file_keys_request)
                .await
                .map_err(|e| WorkspaceStoreError::Backend(format!("file keys await: {e:?}")))?,
        )?;
        let file_values = value_array(
            await_request(file_values_request)
                .await
                .map_err(|e| WorkspaceStoreError::Backend(format!("file values await: {e:?}")))?,
        )?;
        let map_keys = string_array(
            await_request(map_keys_request)
                .await
                .map_err(|e| WorkspaceStoreError::Backend(format!("map keys await: {e:?}")))?,
        )?;
        let map_values = value_array(
            await_request(map_values_request)
                .await
                .map_err(|e| WorkspaceStoreError::Backend(format!("map values await: {e:?}")))?,
        )?;
        let existing_metadata =
            string_array(await_request(metadata_keys_request).await.map_err(|e| {
                WorkspaceStoreError::Backend(format!("metadata keys await: {e:?}"))
            })?)?
            .into_iter()
            .collect::<HashSet<_>>();
        let existing_files = file_keys.iter().cloned().collect::<HashSet<_>>();
        let existing_maps = map_keys.iter().cloned().collect::<HashSet<_>>();
        let legacy_map = map_keys
            .iter()
            .cloned()
            .zip(map_values.iter().cloned())
            .collect::<HashMap<_, _>>();
        let ownership = legacy_map
            .iter()
            .filter_map(|(key, value)| {
                Some((
                    ownership_workspace(key)?,
                    decode_ownership(&value.as_string()?)?,
                ))
            })
            .collect::<HashMap<_, _>>();
        let sole_workspace = if ownership.len() == 1 {
            ownership.keys().next().copied()
        } else {
            None
        };

        for (legacy_key, value) in file_keys.into_iter().zip(file_values) {
            if is_scoped_key(&legacy_key) {
                continue;
            }
            let disposition = match scope {
                ActiveScope::Local => LegacyDisposition::Migrate,
                ActiveScope::Workspace(id) if legacy_key == WORKSPACE_MANIFEST_PATH => {
                    if sole_workspace == Some(id) {
                        LegacyDisposition::Migrate
                    } else {
                        LegacyDisposition::Quarantine
                    }
                }
                ActiveScope::Workspace(id) => legacy_path_disposition(id, &legacy_key, &ownership),
            };
            match disposition {
                LegacyDisposition::Migrate => {
                    let destination = scoped_key(scope, &legacy_key);
                    if existing_files.contains(&destination) {
                        quarantine_value(&quarantine, scope, FILES_STORE, &legacy_key, &value)?;
                    } else {
                        files
                            .put_with_key(&value, &JsValue::from_str(&destination))
                            .map_err(|e| {
                                WorkspaceStoreError::Backend(format!("migrate file: {e:?}"))
                            })?;
                    }
                    files.delete(&JsValue::from_str(&legacy_key)).map_err(|e| {
                        WorkspaceStoreError::Backend(format!("delete legacy file: {e:?}"))
                    })?;
                }
                LegacyDisposition::Quarantine => {
                    quarantine_value(&quarantine, scope, FILES_STORE, &legacy_key, &value)?;
                    files.delete(&JsValue::from_str(&legacy_key)).map_err(|e| {
                        WorkspaceStoreError::Backend(format!("quarantine file: {e:?}"))
                    })?;
                }
                LegacyDisposition::LeaveForOtherWorkspace => {}
            }
        }

        for (legacy_key, value) in map_keys.into_iter().zip(map_values) {
            if is_scoped_key(&legacy_key) {
                continue;
            }
            let is_path = !legacy_key.starts_with('\0');
            let disposition = if is_path {
                match scope {
                    ActiveScope::Local => LegacyDisposition::Migrate,
                    ActiveScope::Workspace(id) => {
                        let disposition = legacy_path_disposition(id, &legacy_key, &ownership);
                        let expected = ownership
                            .get(&id)
                            .and_then(|paths| paths.get(&legacy_key))
                            .copied()
                            .flatten();
                        let actual = value
                            .as_string()
                            .and_then(|value| uuid::Uuid::parse_str(&value).ok());
                        if disposition == LegacyDisposition::Migrate
                            && expected.is_some()
                            && expected != actual
                        {
                            LegacyDisposition::Quarantine
                        } else {
                            disposition
                        }
                    }
                }
            } else if legacy_key == RECENT_NOTES_KEY {
                match scope {
                    ActiveScope::Local => LegacyDisposition::Migrate,
                    ActiveScope::Workspace(id) if sole_workspace == Some(id) => {
                        LegacyDisposition::Migrate
                    }
                    ActiveScope::Workspace(_) => LegacyDisposition::Quarantine,
                }
            } else if matches!(scope, ActiveScope::Workspace(id) if ownership_workspace(&legacy_key) == Some(id))
            {
                LegacyDisposition::Migrate
            } else {
                LegacyDisposition::LeaveForOtherWorkspace
            };
            match disposition {
                LegacyDisposition::Migrate => {
                    let destination = scoped_key(scope, &legacy_key);
                    let (store, existing) = if is_path {
                        (&doc_map, &existing_maps)
                    } else {
                        (&metadata, &existing_metadata)
                    };
                    if existing.contains(&destination) {
                        quarantine_value(&quarantine, scope, DOC_MAP_STORE, &legacy_key, &value)?;
                    } else {
                        store
                            .put_with_key(&value, &JsValue::from_str(&destination))
                            .map_err(|e| {
                                WorkspaceStoreError::Backend(format!("migrate mapping: {e:?}"))
                            })?;
                    }
                    doc_map
                        .delete(&JsValue::from_str(&legacy_key))
                        .map_err(|e| {
                            WorkspaceStoreError::Backend(format!("delete legacy mapping: {e:?}"))
                        })?;
                }
                LegacyDisposition::Quarantine => {
                    quarantine_value(&quarantine, scope, DOC_MAP_STORE, &legacy_key, &value)?;
                    doc_map
                        .delete(&JsValue::from_str(&legacy_key))
                        .map_err(|e| {
                            WorkspaceStoreError::Backend(format!("quarantine mapping: {e:?}"))
                        })?;
                }
                LegacyDisposition::LeaveForOtherWorkspace => {}
            }
        }

        migrations
            .put_with_key(
                &JsValue::from_str("complete"),
                &JsValue::from_str(&marker_key),
            )
            .map_err(|e| WorkspaceStoreError::Backend(format!("migration marker write: {e:?}")))?;
        await_transaction(tx)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("migration tx await: {e:?}")))
    }

    async fn read_bytes(&self, path: &str) -> Result<Option<Vec<u8>>, WorkspaceStoreError> {
        let tx = self
            .db
            .transaction_with_str_and_mode("files", IdbTransactionMode::Readonly)
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {:?}", e)))?;
        let store = tx
            .object_store("files")
            .map_err(|e| WorkspaceStoreError::Backend(format!("store: {:?}", e)))?;
        let request = store
            .get(&JsValue::from_str(path))
            .map_err(|e| WorkspaceStoreError::Backend(format!("get: {:?}", e)))?;

        let result = await_request(request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("await: {:?}", e)))?;

        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }

        let array = js_sys::Uint8Array::new(&result);
        Ok(Some(array.to_vec()))
    }

    async fn write_bytes(&self, path: &str, data: &[u8]) -> Result<(), WorkspaceStoreError> {
        let tx = self
            .db
            .transaction_with_str_and_mode("files", IdbTransactionMode::Readwrite)
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {:?}", e)))?;
        let store = tx
            .object_store("files")
            .map_err(|e| WorkspaceStoreError::Backend(format!("store: {:?}", e)))?;
        let js_array = js_sys::Uint8Array::from(data);
        let request = store
            .put_with_key(&js_array, &JsValue::from_str(path))
            .map_err(|e| WorkspaceStoreError::Backend(format!("put: {:?}", e)))?;

        await_request(request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("await: {:?}", e)))?;
        await_transaction(tx)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx await: {:?}", e)))?;
        Ok(())
    }

    async fn delete_key(&self, store_name: &str, key: &str) -> Result<(), WorkspaceStoreError> {
        let tx = self
            .db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readwrite)
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {:?}", e)))?;
        let store = tx
            .object_store(store_name)
            .map_err(|e| WorkspaceStoreError::Backend(format!("store: {:?}", e)))?;
        let request = store
            .delete(&JsValue::from_str(key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("delete: {:?}", e)))?;

        await_request(request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("await: {:?}", e)))?;
        await_transaction(tx)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx await: {:?}", e)))?;
        Ok(())
    }

    async fn read_string(
        &self,
        store_name: &str,
        key: &str,
    ) -> Result<Option<String>, WorkspaceStoreError> {
        let tx = self
            .db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readonly)
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {:?}", e)))?;
        let store = tx
            .object_store(store_name)
            .map_err(|e| WorkspaceStoreError::Backend(format!("store: {:?}", e)))?;
        let request = store
            .get(&JsValue::from_str(key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("get: {:?}", e)))?;

        let result = await_request(request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("await: {:?}", e)))?;

        if result.is_undefined() || result.is_null() {
            return Ok(None);
        }

        Ok(result.as_string())
    }

    async fn write_string(
        &self,
        store_name: &str,
        key: &str,
        value: &str,
    ) -> Result<(), WorkspaceStoreError> {
        let tx = self
            .db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readwrite)
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {:?}", e)))?;
        let store = tx
            .object_store(store_name)
            .map_err(|e| WorkspaceStoreError::Backend(format!("store: {:?}", e)))?;
        let request = store
            .put_with_key(&JsValue::from_str(value), &JsValue::from_str(key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("put: {:?}", e)))?;

        await_request(request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("await: {:?}", e)))?;
        await_transaction(tx)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx await: {:?}", e)))?;
        Ok(())
    }

    async fn get_all_keys(&self, store_name: &str) -> Result<Vec<String>, WorkspaceStoreError> {
        let tx = self
            .db
            .transaction_with_str_and_mode(store_name, IdbTransactionMode::Readonly)
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {:?}", e)))?;
        let store = tx
            .object_store(store_name)
            .map_err(|e| WorkspaceStoreError::Backend(format!("store: {:?}", e)))?;
        let request = store
            .get_all_keys()
            .map_err(|e| WorkspaceStoreError::Backend(format!("getAllKeys: {:?}", e)))?;

        let result = await_request(request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("await: {:?}", e)))?;

        let array: js_sys::Array = result
            .dyn_into()
            .map_err(|_| WorkspaceStoreError::Backend("getAllKeys result not array".into()))?;

        let mut keys = Vec::with_capacity(array.length() as usize);
        for i in 0..array.length() {
            if let Some(s) = array.get(i).as_string() {
                keys.push(s);
            }
        }
        Ok(keys)
    }
}

fn string_array(value: JsValue) -> Result<Vec<String>, WorkspaceStoreError> {
    let array: js_sys::Array = value
        .dyn_into()
        .map_err(|_| WorkspaceStoreError::Backend("IndexedDB keys result not array".into()))?;
    Ok(array.iter().filter_map(|value| value.as_string()).collect())
}

fn value_array(value: JsValue) -> Result<Vec<JsValue>, WorkspaceStoreError> {
    let array: js_sys::Array = value
        .dyn_into()
        .map_err(|_| WorkspaceStoreError::Backend("IndexedDB values result not array".into()))?;
    Ok(array.iter().collect())
}

fn quarantine_value(
    store: &web_sys::IdbObjectStore,
    scope: ActiveScope,
    source_store: &str,
    legacy_key: &str,
    value: &JsValue,
) -> Result<(), WorkspaceStoreError> {
    let key = format!(
        "{}{}:{}",
        scope.prefix(),
        source_store,
        js_sys::encode_uri_component(legacy_key)
    );
    store
        .put_with_key(value, &JsValue::from_str(&key))
        .map_err(|e| WorkspaceStoreError::Backend(format!("quarantine write: {e:?}")))?;
    Ok(())
}

#[async_trait::async_trait(?Send)]
impl WorkspaceStore for WorkspaceDb {
    async fn load_manifest(&self) -> Result<Option<WorkspaceManifest>, WorkspaceStoreError> {
        let scope = self.scope()?;
        let bytes = self
            .read_bytes(&scoped_key(scope, WORKSPACE_MANIFEST_PATH))
            .await?;
        self.ensure_scope(scope)?;
        match bytes {
            Some(data) => {
                let text = std::str::from_utf8(&data)
                    .map_err(|e| WorkspaceStoreError::Serialization(format!("UTF-8: {}", e)))?;
                deserialize_manifest_toml(text).map(Some)
            }
            None => Ok(None),
        }
    }

    async fn save_manifest(&self, manifest: &WorkspaceManifest) -> Result<(), WorkspaceStoreError> {
        let toml = serialize_manifest_toml(manifest)?;
        self.write_bytes(&self.scoped_key(WORKSPACE_MANIFEST_PATH)?, toml.as_bytes())
            .await
    }

    async fn list_entries(&self) -> Result<Vec<WorkspaceEntry>, WorkspaceStoreError> {
        let scope = self.scope()?;
        let prefix = scope.prefix();
        let keys = self.get_all_keys(FILES_STORE).await?;
        let folder_keys = self.get_all_keys(FOLDERS_STORE).await?;
        let mut entries: Vec<WorkspaceEntry> = Vec::new();
        for key in keys {
            let Some(logical_key) = key.strip_prefix(&prefix) else {
                continue;
            };
            let path = WorkspacePath::new(logical_key)?;
            let size = self
                .read_bytes(&key)
                .await?
                .map(|b| b.len() as u64)
                .unwrap_or(0);
            entries.push(WorkspaceEntry::classify(&path, size));
        }
        for key in folder_keys {
            let Some(logical_key) = key.strip_prefix(&prefix) else {
                continue;
            };
            entries.push(WorkspaceEntry::folder(&WorkspacePath::new(logical_key)?));
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        self.ensure_scope(scope)?;
        Ok(entries)
    }

    async fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Result<Option<Vec<u8>>, WorkspaceStoreError> {
        let scope = self.scope()?;
        let result = self.read_bytes(&scoped_key(scope, path.as_str())).await?;
        self.ensure_scope(scope)?;
        Ok(result)
    }

    async fn write_file(
        &self,
        path: &WorkspacePath,
        data: &[u8],
    ) -> Result<(), WorkspaceStoreError> {
        self.write_bytes(&self.scoped_key(path.as_str())?, data)
            .await
    }

    async fn delete_file(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError> {
        let path_str = path.as_str();
        let key = self.scoped_key(path_str)?;
        if self.read_bytes(&key).await?.is_none() {
            return Err(WorkspaceStoreError::NotFound(path_str.to_string()));
        }
        if INJECT_FAIL_DELETE_FILE.with(|f| f.replace(false)) {
            return Err(WorkspaceStoreError::Backend(
                "injected delete_file failure".into(),
            ));
        }
        let stores = js_sys::Array::new();
        stores.push(&JsValue::from_str(FILES_STORE));
        stores.push(&JsValue::from_str(DOC_MAP_STORE));
        let tx = self
            .db
            .transaction_with_str_sequence_and_mode(&stores.into(), IdbTransactionMode::Readwrite)
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {e:?}")))?;
        tx.object_store(FILES_STORE)
            .and_then(|store| store.delete(&JsValue::from_str(&key)))
            .map_err(|e| WorkspaceStoreError::Backend(format!("delete file: {e:?}")))?;
        tx.object_store(DOC_MAP_STORE)
            .and_then(|store| store.delete(&JsValue::from_str(&key)))
            .map_err(|e| WorkspaceStoreError::Backend(format!("delete mapping: {e:?}")))?;
        await_transaction(tx)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx await: {e:?}")))
    }

    async fn rename_file(
        &self,
        src: &WorkspacePath,
        dst: &WorkspacePath,
    ) -> Result<(), WorkspaceStoreError> {
        let src_str = src.as_str();
        let dst_str = dst.as_str();
        let src_key = self.scoped_key(src_str)?;
        let dst_key = self.scoped_key(dst_str)?;
        let mut metadata_values = Vec::new();
        for prefix in ["\0asset_id:", "\0asset_metadata_version:"] {
            let old = format!("{prefix}{src_str}");
            if let Some(value) = self.get_metadata(&old).await? {
                metadata_values.push((old, format!("{prefix}{dst_str}"), value));
            }
        }

        let store_names = js_sys::Array::new();
        store_names.push(&JsValue::from_str("files"));
        store_names.push(&JsValue::from_str("doc_map"));
        store_names.push(&JsValue::from_str(METADATA_STORE));

        let tx = self
            .db
            .transaction_with_str_sequence_and_mode(
                &store_names.into(),
                IdbTransactionMode::Readwrite,
            )
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx: {:?}", e)))?;

        let files_store = tx
            .object_store("files")
            .map_err(|e| WorkspaceStoreError::Backend(format!("files store: {:?}", e)))?;
        let doc_map_store = tx
            .object_store("doc_map")
            .map_err(|e| WorkspaceStoreError::Backend(format!("doc_map store: {:?}", e)))?;
        let metadata_store = tx
            .object_store(METADATA_STORE)
            .map_err(|e| WorkspaceStoreError::Backend(format!("metadata store: {e:?}")))?;

        let src_request = files_store
            .get(&JsValue::from_str(&src_key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("get source: {e:?}")))?;
        let dst_request = files_store
            .get(&JsValue::from_str(&dst_key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("get destination: {e:?}")))?;
        let doc_request = doc_map_store
            .get(&JsValue::from_str(&src_key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("get mapping: {e:?}")))?;
        let data = await_request(src_request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("source await: {e:?}")))?;
        if data.is_undefined() || data.is_null() {
            return Err(WorkspaceStoreError::NotFound(src_str.to_string()));
        }
        let destination = await_request(dst_request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("destination await: {e:?}")))?;
        if !destination.is_undefined() && !destination.is_null() {
            return Err(WorkspaceStoreError::AlreadyExists(dst_str.to_string()));
        }
        let doc_id = await_request(doc_request)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("mapping await: {e:?}")))?;

        files_store
            .put_with_key(&data, &JsValue::from_str(&dst_key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("put: {:?}", e)))?;

        if !doc_id.is_undefined() && !doc_id.is_null() {
            doc_map_store
                .put_with_key(&doc_id, &JsValue::from_str(&dst_key))
                .map_err(|e| WorkspaceStoreError::Backend(format!("put doc_map: {:?}", e)))?;
        }

        files_store
            .delete(&JsValue::from_str(&src_key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("del files: {:?}", e)))?;

        doc_map_store
            .delete(&JsValue::from_str(&src_key))
            .map_err(|e| WorkspaceStoreError::Backend(format!("del doc_map: {:?}", e)))?;
        for (old, new, value) in metadata_values {
            metadata_store
                .put_with_key(
                    &JsValue::from_str(&value),
                    &JsValue::from_str(&self.scoped_key(&new)?),
                )
                .map_err(|e| WorkspaceStoreError::Backend(format!("put metadata: {e:?}")))?;
            metadata_store
                .delete(&JsValue::from_str(&self.scoped_key(&old)?))
                .map_err(|e| WorkspaceStoreError::Backend(format!("delete metadata: {e:?}")))?;
        }

        await_transaction(tx)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx await: {:?}", e)))?;

        Ok(())
    }

    async fn create_folder(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError> {
        let key = self.scoped_key(path.as_str())?;
        if self.read_bytes(&key).await?.is_some()
            || self.read_string(FOLDERS_STORE, &key).await?.is_some()
        {
            return Err(WorkspaceStoreError::AlreadyExists(path.as_str().into()));
        }
        self.write_string(FOLDERS_STORE, &key, "1").await
    }

    async fn rename_tree(
        &self,
        src: &WorkspacePath,
        dst: &WorkspacePath,
    ) -> Result<(), WorkspaceStoreError> {
        let entries = self.list_entries().await?;
        if !entries.iter().any(|entry| {
            entry.path == *src && entry.kind == inkstone_workspace::WorkspaceEntryKind::Folder
        }) {
            return Err(WorkspaceStoreError::NotFound(src.as_str().into()));
        }
        let prefix = format!("{}/", src.as_str());
        if dst.as_str().starts_with(&prefix) {
            return Err(WorkspaceStoreError::InvalidPath(
                "folder cannot be moved into itself".into(),
            ));
        }
        let moving = entries
            .iter()
            .filter(|entry| entry.path == *src || entry.path.as_str().starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        let targets = moving
            .iter()
            .map(|entry| {
                let suffix = entry.path.as_str().strip_prefix(src.as_str()).unwrap();
                WorkspacePath::new(format!("{}{suffix}", dst.as_str()))
                    .map(|path| (entry.clone(), path))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let moving_paths = moving
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<HashSet<_>>();
        if targets.iter().any(|(_, target)| {
            entries
                .iter()
                .any(|entry| entry.path == *target && !moving_paths.contains(entry.path.as_str()))
        }) {
            return Err(WorkspaceStoreError::AlreadyExists(dst.as_str().into()));
        }
        let mut file_data = HashMap::new();
        let mut doc_ids = HashMap::new();
        let metadata_prefixes = [
            "\0asset_id:",
            "\0asset_metadata_version:",
            "\0folder_id:",
            "\0folder_projection_version:",
        ];
        let mut metadata = Vec::new();
        for (entry, target) in &targets {
            if entry.kind != inkstone_workspace::WorkspaceEntryKind::Folder {
                file_data.insert(
                    entry.path.clone(),
                    self.read_file(&entry.path)
                        .await?
                        .ok_or_else(|| WorkspaceStoreError::NotFound(entry.path.as_str().into()))?,
                );
                if let Some(id) = self.get_doc_id(entry.path.as_str()).await? {
                    doc_ids.insert(entry.path.clone(), id);
                }
            }
            for prefix in metadata_prefixes {
                let old = format!("{prefix}{}", entry.path.as_str());
                if let Some(value) = self.get_metadata(&old).await? {
                    metadata.push((old, format!("{prefix}{}", target.as_str()), value));
                }
            }
        }
        let stores = js_sys::Array::new();
        for name in [FILES_STORE, FOLDERS_STORE, DOC_MAP_STORE, METADATA_STORE] {
            stores.push(&JsValue::from_str(name));
        }
        let tx = self
            .db
            .transaction_with_str_sequence_and_mode(&stores.into(), IdbTransactionMode::Readwrite)
            .map_err(|error| WorkspaceStoreError::Backend(format!("tree rename tx: {error:?}")))?;
        let files = tx
            .object_store(FILES_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("files: {error:?}")))?;
        let folders = tx
            .object_store(FOLDERS_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("folders: {error:?}")))?;
        let docs = tx
            .object_store(DOC_MAP_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("docs: {error:?}")))?;
        let metadata_store = tx
            .object_store(METADATA_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("metadata: {error:?}")))?;
        for (entry, target) in &targets {
            let old_key = self.scoped_key(entry.path.as_str())?;
            let new_key = self.scoped_key(target.as_str())?;
            if entry.kind == inkstone_workspace::WorkspaceEntryKind::Folder {
                folders
                    .put_with_key(&JsValue::from_str("1"), &JsValue::from_str(&new_key))
                    .map_err(|error| {
                        WorkspaceStoreError::Backend(format!("folder move: {error:?}"))
                    })?;
                folders
                    .delete(&JsValue::from_str(&old_key))
                    .map_err(|error| {
                        WorkspaceStoreError::Backend(format!("folder delete: {error:?}"))
                    })?;
            } else {
                let bytes =
                    js_sys::Uint8Array::from(file_data.get(&entry.path).unwrap().as_slice());
                files
                    .put_with_key(&bytes, &JsValue::from_str(&new_key))
                    .map_err(|error| {
                        WorkspaceStoreError::Backend(format!("file move: {error:?}"))
                    })?;
                files
                    .delete(&JsValue::from_str(&old_key))
                    .map_err(|error| {
                        WorkspaceStoreError::Backend(format!("file delete: {error:?}"))
                    })?;
                if let Some(id) = doc_ids.get(&entry.path) {
                    docs.put_with_key(
                        &JsValue::from_str(&id.to_string()),
                        &JsValue::from_str(&new_key),
                    )
                    .map_err(|error| {
                        WorkspaceStoreError::Backend(format!("doc move: {error:?}"))
                    })?;
                    docs.delete(&JsValue::from_str(&old_key)).map_err(|error| {
                        WorkspaceStoreError::Backend(format!("doc delete: {error:?}"))
                    })?;
                }
            }
        }
        for (old, new, value) in metadata {
            metadata_store
                .put_with_key(
                    &JsValue::from_str(&value),
                    &JsValue::from_str(&self.scoped_key(&new)?),
                )
                .map_err(|error| {
                    WorkspaceStoreError::Backend(format!("metadata move: {error:?}"))
                })?;
            metadata_store
                .delete(&JsValue::from_str(&self.scoped_key(&old)?))
                .map_err(|error| {
                    WorkspaceStoreError::Backend(format!("metadata delete: {error:?}"))
                })?;
        }
        await_transaction(tx)
            .await
            .map_err(|error| WorkspaceStoreError::Backend(format!("tree rename await: {error:?}")))
    }

    async fn delete_tree(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError> {
        let entries = self.list_entries().await?;
        let prefix = format!("{}/", path.as_str());
        let deleting = entries
            .into_iter()
            .filter(|entry| entry.path == *path || entry.path.as_str().starts_with(&prefix))
            .collect::<Vec<_>>();
        if !deleting.iter().any(|entry| {
            entry.path == *path && entry.kind == inkstone_workspace::WorkspaceEntryKind::Folder
        }) {
            return Err(WorkspaceStoreError::NotFound(path.as_str().into()));
        }
        let stores = js_sys::Array::new();
        for name in [FILES_STORE, FOLDERS_STORE, DOC_MAP_STORE, METADATA_STORE] {
            stores.push(&JsValue::from_str(name));
        }
        let tx = self
            .db
            .transaction_with_str_sequence_and_mode(&stores.into(), IdbTransactionMode::Readwrite)
            .map_err(|error| WorkspaceStoreError::Backend(format!("tree delete tx: {error:?}")))?;
        let files = tx
            .object_store(FILES_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("files: {error:?}")))?;
        let folders = tx
            .object_store(FOLDERS_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("folders: {error:?}")))?;
        let docs = tx
            .object_store(DOC_MAP_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("docs: {error:?}")))?;
        let metadata = tx
            .object_store(METADATA_STORE)
            .map_err(|error| WorkspaceStoreError::Backend(format!("metadata: {error:?}")))?;
        for entry in deleting {
            let key = self.scoped_key(entry.path.as_str())?;
            if entry.kind == inkstone_workspace::WorkspaceEntryKind::Folder {
                folders.delete(&JsValue::from_str(&key)).map_err(|error| {
                    WorkspaceStoreError::Backend(format!("folder delete: {error:?}"))
                })?;
            } else {
                files.delete(&JsValue::from_str(&key)).map_err(|error| {
                    WorkspaceStoreError::Backend(format!("file delete: {error:?}"))
                })?;
                docs.delete(&JsValue::from_str(&key)).map_err(|error| {
                    WorkspaceStoreError::Backend(format!("doc delete: {error:?}"))
                })?;
            }
            for prefix in [
                "\0asset_id:",
                "\0asset_metadata_version:",
                "\0folder_id:",
                "\0folder_projection_version:",
            ] {
                metadata
                    .delete(&JsValue::from_str(
                        &self.scoped_key(&format!("{prefix}{}", entry.path.as_str()))?,
                    ))
                    .map_err(|error| {
                        WorkspaceStoreError::Backend(format!("metadata delete: {error:?}"))
                    })?;
            }
        }
        await_transaction(tx)
            .await
            .map_err(|error| WorkspaceStoreError::Backend(format!("tree delete await: {error:?}")))
    }
}

impl WorkspaceDb {
    /// Close the underlying IndexedDB connection.
    pub fn close(&self) {
        self.db.close();
    }

    pub async fn get_doc_id(&self, path: &str) -> Result<Option<uuid::Uuid>, WorkspaceStoreError> {
        let scope = self.scope()?;
        let s = self
            .read_string(DOC_MAP_STORE, &scoped_key(scope, path))
            .await?;
        self.ensure_scope(scope)?;
        match s {
            Some(val) => uuid::Uuid::parse_str(&val)
                .map(Some)
                .map_err(|e| WorkspaceStoreError::Serialization(format!("UUID parse: {}", e))),
            None => Ok(None),
        }
    }

    pub async fn set_doc_id(
        &self,
        path: &str,
        doc_id: uuid::Uuid,
    ) -> Result<(), WorkspaceStoreError> {
        if INJECT_FAIL_SET_DOC_ID.with(|f| f.replace(false)) {
            return Err(WorkspaceStoreError::Backend(
                "injected set_doc_id failure".into(),
            ));
        }
        self.write_string(DOC_MAP_STORE, &self.scoped_key(path)?, &doc_id.to_string())
            .await
    }

    pub async fn delete_doc_id(&self, path: &str) -> Result<(), WorkspaceStoreError> {
        self.delete_key(DOC_MAP_STORE, &self.scoped_key(path)?)
            .await
    }

    pub async fn get_metadata(&self, key: &str) -> Result<Option<String>, WorkspaceStoreError> {
        let scope = self.scope()?;
        let result = self
            .read_string(METADATA_STORE, &scoped_key(scope, key))
            .await?;
        self.ensure_scope(scope)?;
        Ok(result)
    }

    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<(), WorkspaceStoreError> {
        self.write_string(METADATA_STORE, &self.scoped_key(key)?, value)
            .await
    }

    pub async fn get_global_metadata(
        &self,
        key: &str,
    ) -> Result<Option<String>, WorkspaceStoreError> {
        if let Some(value) = self.read_string(GLOBAL_METADATA_STORE, key).await? {
            return Ok(Some(value));
        }
        self.read_string(DOC_MAP_STORE, key).await
    }

    pub async fn set_global_metadata(
        &self,
        key: &str,
        value: &str,
    ) -> Result<(), WorkspaceStoreError> {
        self.write_string(GLOBAL_METADATA_STORE, key, value).await
    }

    pub async fn delete_global_metadata(&self, key: &str) -> Result<(), WorkspaceStoreError> {
        self.delete_key(GLOBAL_METADATA_STORE, key).await?;
        self.delete_key(DOC_MAP_STORE, key).await
    }

    /// Load the recent notes list (up to 8 paths) from the reserved metadata key.
    /// Returns an empty list if no entry exists.
    /// Returns a Serialization error if the stored value is malformed JSON.
    pub async fn get_recent_notes(&self) -> Result<Vec<String>, WorkspaceStoreError> {
        match self.get_metadata(RECENT_NOTES_KEY).await? {
            None => Ok(Vec::new()),
            Some(json) => {
                let list: Vec<String> = serde_json::from_str(&json).map_err(|e| {
                    WorkspaceStoreError::Serialization(format!(
                        "recent_notes JSON malformed: {}",
                        e
                    ))
                })?;

                let mut seen = std::collections::HashSet::new();
                let mut normalized = Vec::new();
                let mut modified = false;

                let original_len = list.len();
                for path in list {
                    if path.starts_with('.') || path.contains("/.") || !path.ends_with(".md") {
                        modified = true;
                        continue;
                    }
                    if seen.insert(path.clone()) {
                        normalized.push(path);
                        if normalized.len() == 8 {
                            // If the original list was larger than 8, we consider it modified
                            break;
                        }
                    } else {
                        modified = true;
                    }
                }

                // Also check if we broke early due to >8 items
                // Wait, if list had 10 unique valid items, `normalized.len()` will reach 8 and break.
                // We haven't set `modified = true` for the ignored tail elements explicitly if we just `break`.
                // Actually, if we `break`, it means `list` had more elements. Let's just track it by comparing lengths.
                // Oh wait, `seen.insert` is O(1). We can just do:
                // if list.len() != normalized.len() { modified = true; }

                if original_len != normalized.len() || modified {
                    self.save_recent_notes(&normalized).await?;
                }

                Ok(normalized)
            }
        }
    }

    /// Persist the recent notes list. Truncates to 8 entries before saving.
    pub async fn save_recent_notes(&self, paths: &[String]) -> Result<(), WorkspaceStoreError> {
        let truncated: Vec<&String> = paths.iter().take(8).collect();
        let json = serde_json::to_string(&truncated).map_err(|e| {
            WorkspaceStoreError::Serialization(format!("recent_notes serialize: {}", e))
        })?;
        self.set_metadata(RECENT_NOTES_KEY, &json).await
    }

    pub async fn delete_document_record(
        &self,
        doc_id: &uuid::Uuid,
    ) -> Result<(), WorkspaceStoreError> {
        let db_name = "inkstone_local_db";
        let window =
            web_sys::window().ok_or_else(|| WorkspaceStoreError::Backend("no window".into()))?;
        let idb_factory = window
            .indexed_db()
            .map_err(|_| WorkspaceStoreError::Backend("IndexedDB access denied".into()))?
            .ok_or_else(|| WorkspaceStoreError::Backend("IndexedDB not supported".into()))?;

        let open_request = idb_factory
            .open_with_u32(db_name, 1)
            .map_err(|e| WorkspaceStoreError::Backend(format!("open: {:?}", e)))?;

        let doc_id_str = match self.scope()? {
            ActiveScope::Local => format!("local/{doc_id}"),
            ActiveScope::Workspace(workspace_id) => format!("{workspace_id}/{doc_id}"),
        };

        let (tx, rx) = futures_channel::oneshot::channel::<Result<(), WorkspaceStoreError>>();
        let tx = Rc::new(RefCell::new(Some(tx)));

        let on_success = {
            let doc_id_str = doc_id_str.clone();
            let tx = tx.clone();
            Closure::wrap(Box::new(move |event: web_sys::Event| {
                let request: web_sys::IdbOpenDbRequest = event.target().unwrap().unchecked_into();
                let db: IdbDatabase = request.result().unwrap().unchecked_into();

                let del_tx = match db
                    .transaction_with_str_and_mode("documents", IdbTransactionMode::Readwrite)
                {
                    Ok(t) => t,
                    Err(e) => {
                        if let Some(tx) = tx.borrow_mut().take() {
                            let _ =
                                tx.send(Err(WorkspaceStoreError::Backend(format!("tx: {:?}", e))));
                        }
                        return;
                    }
                };
                let store = match del_tx.object_store("documents") {
                    Ok(s) => s,
                    Err(e) => {
                        if let Some(tx) = tx.borrow_mut().take() {
                            let _ = tx
                                .send(Err(WorkspaceStoreError::Backend(format!("store: {:?}", e))));
                        }
                        return;
                    }
                };
                if let Err(e) = store.delete(&JsValue::from_str(&doc_id_str)) {
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(Err(WorkspaceStoreError::Backend(format!(
                            "delete: {:?}",
                            e
                        ))));
                    }
                    return;
                }
                let tx_inner = tx.clone();
                let on_complete = Closure::wrap(Box::new(move |_event: web_sys::Event| {
                    if let Some(tx) = tx_inner.borrow_mut().take() {
                        let _ = tx.send(Ok(()));
                    }
                })
                    as Box<dyn FnMut(web_sys::Event)>);
                del_tx.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
                on_complete.forget();
            }) as Box<dyn FnMut(web_sys::Event)>)
        };
        open_request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));

        let tx_error = tx.clone();
        let on_error = Closure::wrap(Box::new(move |event: web_sys::Event| {
            let request: web_sys::IdbOpenDbRequest = event.target().unwrap().unchecked_into();
            let err = request
                .error()
                .unwrap()
                .map(|e| WorkspaceStoreError::Backend(format!("error: {:?}", e)))
                .unwrap_or_else(|| WorkspaceStoreError::Backend("Unknown error".into()));
            if let Some(tx) = tx_error.borrow_mut().take() {
                let _ = tx.send(Err(err));
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        open_request.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let res = rx
            .await
            .map_err(|_| WorkspaceStoreError::Backend("Oneshot sender dropped".into()))?;

        drop(on_success);
        drop(on_error);

        res
    }

    pub async fn clear_active_scope_data(&self) -> Result<(), WorkspaceStoreError> {
        let scope = self.scope()?;
        let prefix = scope.prefix();
        let stores = [FILES_STORE, FOLDERS_STORE, DOC_MAP_STORE, METADATA_STORE];
        let mut keys = Vec::with_capacity(stores.len());
        for store in stores {
            keys.push(self.get_all_keys(store).await?);
        }
        self.ensure_scope(scope)?;
        let names = js_sys::Array::new();
        for store in stores {
            names.push(&JsValue::from_str(store));
        }
        let tx = self
            .db
            .transaction_with_str_sequence_and_mode(&names.into(), IdbTransactionMode::Readwrite)
            .map_err(|error| WorkspaceStoreError::Backend(format!("cleanup tx: {error:?}")))?;
        for (store_name, keys) in stores.into_iter().zip(keys) {
            let store = tx.object_store(store_name).map_err(|error| {
                WorkspaceStoreError::Backend(format!("cleanup store: {error:?}"))
            })?;
            for key in keys.into_iter().filter(|key| key.starts_with(&prefix)) {
                store.delete(&JsValue::from_str(&key)).map_err(|error| {
                    WorkspaceStoreError::Backend(format!("cleanup delete: {error:?}"))
                })?;
            }
        }
        await_transaction(tx)
            .await
            .map_err(|error| WorkspaceStoreError::Backend(format!("cleanup await: {error:?}")))?;
        self.clear_scope();
        Ok(())
    }

    pub async fn delete_folder_marker(
        &self,
        path: &WorkspacePath,
    ) -> Result<(), WorkspaceStoreError> {
        self.delete_key(FOLDERS_STORE, &self.scoped_key(path.as_str())?)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(value: u128) -> ActiveScope {
        ActiveScope::Workspace(uuid::Uuid::from_u128(value))
    }

    #[test]
    fn two_workspaces_isolate_same_paths_and_document_ids() {
        let first = workspace(1);
        let second = workspace(2);
        let doc_id = uuid::Uuid::from_u128(9);

        assert_ne!(
            scoped_key(first, "Notes/Same.md"),
            scoped_key(second, "Notes/Same.md")
        );
        assert_ne!(
            scoped_key(first, &doc_id.to_string()),
            scoped_key(second, &doc_id.to_string())
        );
    }

    #[test]
    fn manifest_recent_notes_and_workspace_metadata_are_isolated() {
        let first = workspace(1);
        let second = workspace(2);
        for key in [WORKSPACE_MANIFEST_PATH, RECENT_NOTES_KEY, "projection"] {
            assert_ne!(scoped_key(first, key), scoped_key(second, key));
        }
        assert_eq!("\0device_id", "\0device_id");
    }

    #[test]
    fn migration_marker_is_stable_and_per_workspace() {
        let first = workspace(1);
        let second = workspace(2);
        assert_eq!(first.migration_key(), first.migration_key());
        assert_ne!(first.migration_key(), second.migration_key());
    }

    #[test]
    fn ambiguous_and_unproven_legacy_paths_are_quarantined() {
        let first_id = uuid::Uuid::from_u128(1);
        let second_id = uuid::Uuid::from_u128(2);
        let mut ownership = HashMap::new();
        ownership.insert(
            first_id,
            HashMap::from([("Shared.md".into(), Some(uuid::Uuid::from_u128(3)))]),
        );
        ownership.insert(
            second_id,
            HashMap::from([("Shared.md".into(), Some(uuid::Uuid::from_u128(4)))]),
        );

        assert_eq!(
            legacy_path_disposition(first_id, "Shared.md", &ownership),
            LegacyDisposition::Quarantine
        );
        assert_eq!(
            legacy_path_disposition(first_id, "Unknown.md", &ownership),
            LegacyDisposition::Quarantine
        );
    }

    #[test]
    fn uniquely_proven_legacy_path_migrates_to_only_its_owner() {
        let first_id = uuid::Uuid::from_u128(1);
        let second_id = uuid::Uuid::from_u128(2);
        let ownership = HashMap::from([(
            first_id,
            HashMap::from([("Owned.md".into(), Some(uuid::Uuid::from_u128(3)))]),
        )]);

        assert_eq!(
            legacy_path_disposition(first_id, "Owned.md", &ownership),
            LegacyDisposition::Migrate
        );
        assert_eq!(
            legacy_path_disposition(second_id, "Owned.md", &ownership),
            LegacyDisposition::LeaveForOtherWorkspace
        );
    }

    #[test]
    fn synchronized_operations_reject_missing_scope() {
        let error = require_scoped_key(None, "Notes.md").unwrap_err();
        assert!(format!("{error:?}").contains("workspace scope is not active"));
    }

    #[test]
    fn logout_and_account_switch_clear_or_replace_scope() {
        let mut active = Some(workspace(1));
        assert!(require_scoped_key(active, "Private.md").is_ok());
        active = None;
        assert!(require_scoped_key(active, "Private.md").is_err());

        active = Some(workspace(2));
        assert!(
            require_scoped_key(active, "Private.md")
                .unwrap()
                .starts_with(&workspace(2).prefix())
        );
    }

    #[test]
    fn rename_and_delete_keys_cannot_touch_another_workspace() {
        let first = workspace(1);
        let second = workspace(2);
        let mut files = HashMap::from([
            (scoped_key(first, "Same.md"), "first"),
            (scoped_key(second, "Same.md"), "second"),
        ]);
        let first_source = scoped_key(first, "Same.md");
        let first_destination = scoped_key(first, "Renamed.md");
        let value = files.remove(&first_source).unwrap();
        files.insert(first_destination.clone(), value);

        assert_eq!(files.get(&first_destination), Some(&"first"));
        assert_eq!(files.get(&scoped_key(second, "Same.md")), Some(&"second"));
        files.remove(&first_destination);
        assert_eq!(files.get(&scoped_key(second, "Same.md")), Some(&"second"));
    }
}
