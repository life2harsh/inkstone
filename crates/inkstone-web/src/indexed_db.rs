use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{IdbDatabase, IdbOpenDbRequest, IdbRequest, IdbTransaction, IdbTransactionMode};

#[derive(Clone)]
pub struct IndexedDb {
    db: IdbDatabase,
}

async fn wait_for_request(request: IdbRequest) -> Result<JsValue, JsValue> {
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

async fn wait_for_transaction(tx: IdbTransaction) -> Result<(), JsValue> {
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

impl IndexedDb {
    pub async fn open(name: &str, version: u32) -> Result<Self, JsValue> {
        let window = web_sys::window().ok_or("No window found")?;
        let idb_factory = window.indexed_db()?.ok_or("IndexedDB not supported")?;
        let request = idb_factory.open_with_u32(name, version)?;

        let (tx, rx) = futures_channel::oneshot::channel::<Result<IdbDatabase, JsValue>>();
        let tx = Rc::new(RefCell::new(Some(tx)));

        let on_upgrade = Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
            let request: IdbOpenDbRequest = event.target().unwrap().unchecked_into();
            let db: IdbDatabase = request.result().unwrap().unchecked_into();
            if !db.object_store_names().contains("documents") {
                if let Err(e) = db.create_object_store("documents") {
                    web_sys::console::error_1(&e);
                }
            }
        })
            as Box<dyn FnMut(web_sys::IdbVersionChangeEvent)>);
        request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));

        let tx_success = tx.clone();
        let on_success = Closure::wrap(Box::new(move |event: web_sys::Event| {
            let request: IdbOpenDbRequest = event.target().unwrap().unchecked_into();
            let db: IdbDatabase = request.result().unwrap().unchecked_into();
            if let Some(tx) = tx_success.borrow_mut().take() {
                let _ = tx.send(Ok(db));
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));

        let tx_error = tx.clone();
        let on_error = Closure::wrap(Box::new(move |event: web_sys::Event| {
            let request: IdbOpenDbRequest = event.target().unwrap().unchecked_into();
            let err = request
                .error()
                .unwrap()
                .map(|e| JsValue::from(e))
                .unwrap_or_else(|| JsValue::from_str("Unknown error opening DB"));
            if let Some(tx) = tx_error.borrow_mut().take() {
                let _ = tx.send(Err(err));
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        request.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let res = rx
            .await
            .map_err(|_| JsValue::from_str("Oneshot sender dropped"))?;

        drop(on_upgrade);
        drop(on_success);
        drop(on_error);

        res.map(|db| IndexedDb { db })
    }

    pub async fn get_document_record(&self, doc_id: &str) -> Result<Option<String>, JsValue> {
        let tx = self
            .db
            .transaction_with_str_and_mode("documents", IdbTransactionMode::Readonly)?;
        let store = tx.object_store("documents")?;
        let request = store.get(&JsValue::from_str(doc_id))?;

        let result = wait_for_request(request).await?;
        if result.is_undefined() || result.is_null() {
            Ok(None)
        } else {
            let s = result
                .as_string()
                .ok_or_else(|| JsValue::from_str("Expected string value in IndexedDB"))?;
            Ok(Some(s))
        }
    }

    pub fn close(&self) {
        self.db.close();
    }

    pub async fn put_document_record(
        &self,
        doc_id: &str,
        record_json: &str,
    ) -> Result<(), JsValue> {
        let tx = self
            .db
            .transaction_with_str_and_mode("documents", IdbTransactionMode::Readwrite)?;
        let store = tx.object_store("documents")?;

        let key = JsValue::from_str(doc_id);
        let val = JsValue::from_str(record_json);
        let request = store.put_with_key(&val, &key)?;

        wait_for_request(request).await?;
        wait_for_transaction(tx).await?;
        Ok(())
    }
}
