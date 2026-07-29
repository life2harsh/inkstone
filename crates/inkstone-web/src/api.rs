use serde::Serialize;
use serde::de::DeserializeOwned;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestCredentials, RequestInit, Response};

thread_local! {
    static UNAUTHORIZED_HANDLER: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
}

pub fn set_unauthorized_handler(handler: Rc<dyn Fn()>) {
    UNAUTHORIZED_HANDLER.with(|current| *current.borrow_mut() = Some(handler));
}

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

pub async fn json<T: DeserializeOwned, B: Serialize>(
    method: &str,
    path: &str,
    body: Option<&B>,
) -> Result<T, ApiError> {
    let response = request(method, path, body).await?;
    let text = response_text(&response).await?;
    serde_json::from_str(&text).map_err(|error| ApiError {
        status: response.status(),
        message: format!("Invalid server response: {error}"),
    })
}

pub async fn empty<B: Serialize>(
    method: &str,
    path: &str,
    body: Option<&B>,
) -> Result<(), ApiError> {
    request(method, path, body).await.map(|_| ())
}

async fn request<B: Serialize>(
    method: &str,
    path: &str,
    body: Option<&B>,
) -> Result<Response, ApiError> {
    let options = RequestInit::new();
    options.set_method(method);
    options.set_credentials(RequestCredentials::Include);
    if let Some(body) = body {
        let json = serde_json::to_string(body).map_err(|error| ApiError {
            status: 0,
            message: format!("Failed to encode request: {error}"),
        })?;
        options.set_body(&JsValue::from_str(&json));
    }
    let request = Request::new_with_str_and_init(path, &options).map_err(js_error)?;
    request
        .headers()
        .set("Accept", "application/json")
        .map_err(js_error)?;
    if body.is_some() {
        request
            .headers()
            .set("Content-Type", "application/json")
            .map_err(js_error)?;
    }
    let window = web_sys::window().ok_or_else(|| ApiError {
        status: 0,
        message: "Browser window is unavailable".into(),
    })?;
    let response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(js_error)?
        .dyn_into::<Response>()
        .map_err(js_error)?;
    if !response.ok() {
        let status = response.status();
        if status == 401 {
            UNAUTHORIZED_HANDLER.with(|handler| {
                if let Some(handler) = handler.borrow().as_ref() {
                    handler();
                }
            });
        }
        let text = response_text(&response).await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .or_else(|| value.get("message"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| format!("Request failed with HTTP {status}"));
        return Err(ApiError { status, message });
    }
    Ok(response)
}

async fn response_text(response: &Response) -> Result<String, ApiError> {
    let promise = response.text().map_err(js_error)?;
    JsFuture::from(promise)
        .await
        .map_err(js_error)?
        .as_string()
        .ok_or_else(|| ApiError {
            status: response.status(),
            message: "Server response was not text".into(),
        })
}

fn js_error(error: JsValue) -> ApiError {
    ApiError {
        status: 0,
        message: error
            .as_string()
            .unwrap_or_else(|| format!("Browser request failed: {error:?}")),
    }
}
