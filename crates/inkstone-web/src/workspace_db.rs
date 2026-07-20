use std::cell::{Cell, RefCell};
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
            .open_with_u32("inkstone-workspace", 1)
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

        res.map(|db| WorkspaceDb { db })
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

#[async_trait::async_trait(?Send)]
impl WorkspaceStore for WorkspaceDb {
    async fn load_manifest(&self) -> Result<Option<WorkspaceManifest>, WorkspaceStoreError> {
        let bytes = self.read_bytes(WORKSPACE_MANIFEST_PATH).await?;
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
        self.write_bytes(WORKSPACE_MANIFEST_PATH, toml.as_bytes())
            .await
    }

    async fn list_entries(&self) -> Result<Vec<WorkspaceEntry>, WorkspaceStoreError> {
        let keys = self.get_all_keys("files").await?;
        let mut entries: Vec<WorkspaceEntry> = Vec::new();
        for key in keys {
            if key == "__meta__" {
                continue;
            }
            let path = WorkspacePath::new(&key)?;
            let size = self
                .read_bytes(&key)
                .await?
                .map(|b| b.len() as u64)
                .unwrap_or(0);
            entries.push(WorkspaceEntry::classify(&path, size));
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    async fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Result<Option<Vec<u8>>, WorkspaceStoreError> {
        self.read_bytes(path.as_str()).await
    }

    async fn write_file(
        &self,
        path: &WorkspacePath,
        data: &[u8],
    ) -> Result<(), WorkspaceStoreError> {
        self.write_bytes(path.as_str(), data).await
    }

    async fn delete_file(&self, path: &WorkspacePath) -> Result<(), WorkspaceStoreError> {
        let path_str = path.as_str();
        if self.read_bytes(path_str).await?.is_none() {
            return Err(WorkspaceStoreError::NotFound(path_str.to_string()));
        }
        if INJECT_FAIL_DELETE_FILE.with(|f| f.replace(false)) {
            return Err(WorkspaceStoreError::Backend(
                "injected delete_file failure".into(),
            ));
        }
        self.delete_key("files", path_str).await?;
        self.delete_key("doc_map", path_str).await?;
        Ok(())
    }

    async fn rename_file(
        &self,
        src: &WorkspacePath,
        dst: &WorkspacePath,
    ) -> Result<(), WorkspaceStoreError> {
        let src_str = src.as_str();
        let dst_str = dst.as_str();

        let data = self
            .read_bytes(src_str)
            .await?
            .ok_or_else(|| WorkspaceStoreError::NotFound(src_str.to_string()))?;

        if self.read_bytes(dst_str).await?.is_some() {
            return Err(WorkspaceStoreError::AlreadyExists(dst_str.to_string()));
        }

        let doc_id = self.read_string("doc_map", src_str).await?;

        let store_names = js_sys::Array::new();
        store_names.push(&JsValue::from_str("files"));
        store_names.push(&JsValue::from_str("doc_map"));

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

        let js_array = js_sys::Uint8Array::from(&data[..]);
        files_store
            .put_with_key(&js_array, &JsValue::from_str(dst_str))
            .map_err(|e| WorkspaceStoreError::Backend(format!("put: {:?}", e)))?;

        if let Some(id) = &doc_id {
            doc_map_store
                .put_with_key(&JsValue::from_str(id), &JsValue::from_str(dst_str))
                .map_err(|e| WorkspaceStoreError::Backend(format!("put doc_map: {:?}", e)))?;
        }

        files_store
            .delete(&JsValue::from_str(src_str))
            .map_err(|e| WorkspaceStoreError::Backend(format!("del files: {:?}", e)))?;

        doc_map_store
            .delete(&JsValue::from_str(src_str))
            .map_err(|e| WorkspaceStoreError::Backend(format!("del doc_map: {:?}", e)))?;

        await_transaction(tx)
            .await
            .map_err(|e| WorkspaceStoreError::Backend(format!("tx await: {:?}", e)))?;

        Ok(())
    }
}

impl WorkspaceDb {
    /// Close the underlying IndexedDB connection.
    pub fn close(&self) {
        self.db.close();
    }

    pub async fn get_doc_id(&self, path: &str) -> Result<Option<uuid::Uuid>, WorkspaceStoreError> {
        let s = self.read_string("doc_map", path).await?;
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
        self.write_string("doc_map", path, &doc_id.to_string())
            .await
    }

    pub async fn delete_doc_id(&self, path: &str) -> Result<(), WorkspaceStoreError> {
        self.delete_key("doc_map", path).await
    }

    pub async fn get_metadata(&self, key: &str) -> Result<Option<String>, WorkspaceStoreError> {
        self.read_string("doc_map", key).await
    }

    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<(), WorkspaceStoreError> {
        self.write_string("doc_map", key, value).await
    }

    /// Load the recent notes list (up to 8 paths) from the reserved metadata key.
    /// Returns an empty list if no entry exists.
    /// Returns a Serialization error if the stored value is malformed JSON.
    pub async fn get_recent_notes(&self) -> Result<Vec<String>, WorkspaceStoreError> {
        const KEY: &str = "\x00recent_notes";
        match self.read_string("doc_map", KEY).await? {
            None => Ok(Vec::new()),
            Some(json) => {
                let list: Vec<String> = serde_json::from_str(&json).map_err(|e| {
                    WorkspaceStoreError::Serialization(format!(
                        "recent_notes JSON malformed: {}",
                        e
                    ))
                })?;
                Ok(list)
            }
        }
    }

    /// Persist the recent notes list. Truncates to 8 entries before saving.
    pub async fn save_recent_notes(&self, paths: &[String]) -> Result<(), WorkspaceStoreError> {
        const KEY: &str = "\x00recent_notes";
        let truncated: Vec<&String> = paths.iter().take(8).collect();
        let json = serde_json::to_string(&truncated).map_err(|e| {
            WorkspaceStoreError::Serialization(format!("recent_notes serialize: {}", e))
        })?;
        self.write_string("doc_map", KEY, &json).await
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

        let doc_id_str = doc_id.to_string();

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
}
