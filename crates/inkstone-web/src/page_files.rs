use inkstone_core::document::BlockContent;
use inkstone_core::ids::BlockId;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::Driver;
use crate::markdown_render::embed_markdown;

pub(crate) async fn read_file_bytes(file: &web_sys::File) -> Result<Vec<u8>, String> {
    let value = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
        .await
        .map_err(|error| format!("Could not read the file: {error:?}"))?;
    Ok(js_sys::Uint8Array::new(&value).to_vec())
}

pub(crate) fn file_media_type(file: &web_sys::File) -> String {
    let media_type = file.type_();
    if media_type.is_empty() {
        crate::asset_media_type(&file.name())
    } else {
        media_type
    }
}

fn extension_for(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        _ => "png",
    }
}

fn attachment_name(file: &web_sys::File, media_type: &str) -> String {
    let name: String = file
        .name()
        .chars()
        .filter(|character| !matches!(character, '/' | '\\' | ':' | '\0'..='\x1f'))
        .collect();
    let name = name.trim().trim_start_matches('.').to_string();
    let generic = name.is_empty()
        || name
            .rsplit_once('.')
            .is_some_and(|(stem, _)| stem.eq_ignore_ascii_case("image"));
    if !generic {
        return name;
    }
    let now = js_sys::Date::new_0();
    format!(
        "Pasted image {:04}-{:02}-{:02} {:02}-{:02}-{:02}.{}",
        now.get_full_year(),
        now.get_month() + 1,
        now.get_date(),
        now.get_hours(),
        now.get_minutes(),
        now.get_seconds(),
        extension_for(media_type)
    )
}

fn label_for(name: &str) -> String {
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_string()
}

fn file_list(list: Option<web_sys::FileList>) -> Vec<web_sys::File> {
    let Some(list) = list else {
        return Vec::new();
    };
    (0..list.length())
        .filter_map(|index| list.get(index))
        .collect()
}

fn carries_files(transfer: &web_sys::DataTransfer) -> bool {
    let types = transfer.types();
    (0..types.length()).any(|index| types.get(index).as_string().as_deref() == Some("Files"))
}

impl Driver {
    pub(crate) fn add_files_to_note(&self, files: Vec<web_sys::File>, after: Option<BlockId>) {
        if files.is_empty() || !self.page_editable() {
            return;
        }
        let Some(note) = self.0.borrow().selected_path.clone() else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut after = after;
            for file in files {
                let media_type = file_media_type(&file);
                let name = attachment_name(&file, &media_type);
                let image = media_type.starts_with("image/") || media_type == "application/pdf";
                let stored = match read_file_bytes(&file).await {
                    Ok(bytes) => this.store_asset(&name, media_type, bytes).await,
                    Err(error) => Err(error),
                };
                let path = match stored {
                    Ok(path) => path,
                    Err(error) => {
                        this.set_sync_error(error);
                        continue;
                    }
                };
                if this.0.borrow().selected_path.as_deref() != Some(note.as_str()) {
                    continue;
                }
                let text = embed_markdown(&note, path.as_str(), &label_for(&name), image);
                let index = {
                    let inner = this.0.borrow();
                    let doc = Driver::effective_document(&inner);
                    after
                        .and_then(|id| doc.block_index(id))
                        .map_or(doc.blocks.len(), |index| index + 1)
                };
                let id = BlockId::new_v4();
                this.insert_block_op(index, id, BlockContent::Markdown { text });
                after = Some(id);
            }
            this.refresh_entries();
            this.render();
        });
    }

    pub(crate) fn pick_files_for_note(&self) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let Some(input) = document
            .create_element("input")
            .ok()
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        input.set_type("file");
        input.set_accept("image/*,application/pdf,.pdf");
        input.set_multiple(true);
        input.set_class_name("note-file-input");
        let _ = input.style().set_property("display", "none");
        let Some(body) = document.body() else {
            return;
        };
        let _ = body.append_child(&input);
        let after = self.0.borrow().editing_block;
        let this = self.clone();
        let picked = input.clone();
        let on_change = Closure::once_into_js(move |_event: web_sys::Event| {
            this.add_files_to_note(file_list(picked.files()), after);
            picked.remove();
        });
        input.set_onchange(Some(on_change.unchecked_ref()));
        input.click();
    }

    fn paste_files(&self, event: &web_sys::ClipboardEvent) {
        let Some(transfer) = event.clipboard_data() else {
            return;
        };
        let files = file_list(transfer.files());
        if files.is_empty() || !self.page_editable() {
            return;
        }
        let active = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.active_element());
        if let Some(active) = active {
            let typing_elsewhere = matches!(active.tag_name().as_str(), "INPUT" | "TEXTAREA")
                && !active.class_list().contains("page-source")
                && !active.class_list().contains("document-title-input");
            if typing_elsewhere {
                return;
            }
        }
        event.prevent_default();
        let after = self.0.borrow().editing_block;
        self.add_files_to_note(files, after);
    }

    fn drop_target(&self, event: &web_sys::DragEvent) -> Option<Option<BlockId>> {
        let transfer = event.data_transfer()?;
        if !carries_files(&transfer) || !self.page_editable() {
            return None;
        }
        let target = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())?;
        target
            .closest(".page-surface, .ink-dock-wrap")
            .ok()
            .flatten()?;
        Some(
            target
                .closest("[data-page-block-id]")
                .ok()
                .flatten()
                .and_then(|block| block.get_attribute("data-page-block-id"))
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId),
        )
    }
}

pub(crate) fn install(driver: &Driver, window: &web_sys::Window) -> Result<(), JsValue> {
    let d = driver.clone();
    let on_paste = Closure::wrap(Box::new(move |event: web_sys::ClipboardEvent| {
        d.paste_files(&event);
    }) as Box<dyn FnMut(web_sys::ClipboardEvent)>);
    window.add_event_listener_with_callback("paste", on_paste.as_ref().unchecked_ref())?;
    on_paste.forget();

    let d = driver.clone();
    let on_dragover = Closure::wrap(Box::new(move |event: web_sys::DragEvent| {
        if d.drop_target(&event).is_some() {
            event.prevent_default();
            if let Some(transfer) = event.data_transfer() {
                transfer.set_drop_effect("copy");
            }
        }
    }) as Box<dyn FnMut(web_sys::DragEvent)>);
    window.add_event_listener_with_callback("dragover", on_dragover.as_ref().unchecked_ref())?;
    on_dragover.forget();

    let d = driver.clone();
    let on_drop = Closure::wrap(Box::new(move |event: web_sys::DragEvent| {
        let Some(after) = d.drop_target(&event) else {
            return;
        };
        event.prevent_default();
        let files = file_list(event.data_transfer().and_then(|transfer| transfer.files()));
        d.add_files_to_note(files, after);
    }) as Box<dyn FnMut(web_sys::DragEvent)>);
    window.add_event_listener_with_callback("drop", on_drop.as_ref().unchecked_ref())?;
    on_drop.forget();
    Ok(())
}
