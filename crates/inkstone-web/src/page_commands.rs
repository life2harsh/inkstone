//! The `/` command menu: typing `/` at the start of a line or after a space
//! opens a filtered list of blocks to turn the line into or insert.
//!
//! What each command does to the text lives in
//! `inkstone_client::page_text`; this module shows the menu, follows the
//! keyboard and mouse, and issues the document ops.

use inkstone_client::page_text::{self, CommandEdit};
use inkstone_core::TextTarget;
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::document::BlockContent;
use inkstone_core::ids::BlockId;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::Driver;
use crate::icons;
use crate::page_editing::caret_metrics;

const MENU_ID: &str = "slash-menu";

/// The open menu: which `/` it belongs to and what it shows.
#[derive(Clone)]
pub(crate) struct SlashMenu {
    block_id: BlockId,
    /// UTF-16 offset of the `/` in the block's source.
    start: u32,
    query: String,
    commands: Vec<&'static str>,
    selected: usize,
}

fn source_block(textarea: &web_sys::HtmlTextAreaElement) -> Option<BlockId> {
    textarea
        .get_attribute("data-block-id")
        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
        .map(BlockId)
}

fn source_for(block_id: BlockId) -> Option<web_sys::HtmlTextAreaElement> {
    web_sys::window()?
        .document()?
        .query_selector(&format!(".page-source[data-block-id=\"{block_id}\"]"))
        .ok()
        .flatten()?
        .dyn_into()
        .ok()
}

fn menu_element(document: &web_sys::Document) -> Option<web_sys::HtmlElement> {
    document.get_element_by_id(MENU_ID)?.dyn_into().ok()
}

impl Driver {
    /// The menu element, created once and kept on `<body>`.
    fn slash_menu_element(&self, document: &web_sys::Document) -> Option<web_sys::HtmlElement> {
        if let Some(menu) = menu_element(document) {
            return Some(menu);
        }
        let menu: web_sys::HtmlElement = document.create_element("div").ok()?.dyn_into().ok()?;
        menu.set_id(MENU_ID);
        menu.set_class_name("slash-menu");
        menu.set_attribute("role", "listbox").ok()?;
        menu.set_attribute("aria-label", "Insert block").ok()?;
        menu.set_hidden(true);

        // Pressing on the menu must not take focus from the source.
        let driver = self.clone();
        let on_down = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
            event.prevent_default();
            let command = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                .and_then(|target| target.closest(".slash-item").ok().flatten())
                .and_then(|item| item.get_attribute("data-command"));
            if let Some(command) = command {
                driver.run_slash_command(&command);
            }
        }) as Box<dyn FnMut(web_sys::PointerEvent)>);
        menu.add_event_listener_with_callback("pointerdown", on_down.as_ref().unchecked_ref())
            .ok()?;
        on_down.forget();
        let on_mouse_down = Closure::wrap(Box::new(move |event: web_sys::MouseEvent| {
            event.prevent_default();
        }) as Box<dyn FnMut(web_sys::MouseEvent)>);
        menu.add_event_listener_with_callback("mousedown", on_mouse_down.as_ref().unchecked_ref())
            .ok()?;
        on_mouse_down.forget();

        document.body()?.append_child(&menu).ok()?;
        Some(menu)
    }

    pub(crate) fn close_slash_menu(&self) {
        self.0.borrow_mut().slash_menu = None;
        if let Some(menu) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| menu_element(&document))
        {
            menu.set_hidden(true);
        }
    }

    /// Opens, filters or closes the menu for the text before the caret.
    pub(crate) fn update_slash_menu(&self, textarea: &web_sys::HtmlTextAreaElement) {
        let Some(block_id) = source_block(textarea) else {
            return self.close_slash_menu();
        };
        let markdown = textarea.get_attribute("data-block-kind").as_deref() != Some("math");
        let caret = textarea.selection_start().ok().flatten().unwrap_or(0);
        let collapsed = textarea.selection_end().ok().flatten() == Some(caret);
        let query = (markdown && collapsed && self.page_editable())
            .then(|| page_text::slash_query(&textarea.value(), caret))
            .flatten();
        let Some((start, query)) = query else {
            self.0.borrow_mut().slash_dismissed = None;
            return self.close_slash_menu();
        };
        if self.0.borrow().slash_dismissed == Some((block_id, start)) {
            return self.close_slash_menu();
        }
        let commands = page_text::filter_commands(&query)
            .into_iter()
            .map(|command| command.id)
            .collect::<Vec<_>>();
        if commands.is_empty() {
            return self.close_slash_menu();
        }
        {
            let mut inner = self.0.borrow_mut();
            let selected = match &inner.slash_menu {
                Some(menu)
                    if menu.block_id == block_id && menu.start == start && menu.query == query =>
                {
                    menu.selected
                }
                _ => 0,
            };
            inner.slash_menu = Some(SlashMenu {
                block_id,
                start,
                query,
                commands,
                selected,
            });
        }
        self.show_slash_menu(textarea);
    }

    fn show_slash_menu(&self, textarea: &web_sys::HtmlTextAreaElement) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };
        let Some(menu) = self.slash_menu_element(&document) else {
            return;
        };
        let (start, commands, selected) = {
            let inner = self.0.borrow();
            let Some(state) = inner.slash_menu.as_ref() else {
                return;
            };
            (state.start, state.commands.clone(), state.selected)
        };

        menu.set_inner_html("");
        for (index, id) in commands.iter().enumerate() {
            let Some(command) = page_text::SLASH_COMMANDS
                .iter()
                .find(|command| command.id == *id)
            else {
                continue;
            };
            let Ok(item) = document.create_element("div") else {
                continue;
            };
            item.set_class_name(if index == selected {
                "slash-item selected"
            } else {
                "slash-item"
            });
            let _ = item.set_attribute("role", "option");
            let _ = item.set_attribute("data-command", command.id);
            let _ = item.set_attribute(
                "aria-selected",
                if index == selected { "true" } else { "false" },
            );
            let glyph = document.create_element("span").unwrap();
            glyph.set_class_name("slash-icon");
            let _ = glyph.append_child(&icons::icon(&document, command.icon));
            let _ = item.append_child(&glyph);
            let label = document.create_element("span").unwrap();
            label.set_class_name("slash-label");
            label.set_text_content(Some(command.label));
            let _ = item.append_child(&label);
            if !command.hint.is_empty() {
                let hint = document.create_element("span").unwrap();
                hint.set_class_name("slash-hint");
                hint.set_text_content(Some(command.hint));
                let _ = item.append_child(&hint);
            }
            let _ = menu.append_child(&item);
        }
        menu.set_hidden(false);

        // Below the line with the `/`, or above it when there is no room.
        let Some(metrics) = caret_metrics(textarea, start) else {
            return;
        };
        let viewport_width = window
            .inner_width()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let viewport_height = window
            .inner_height()
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let width = f64::from(menu.offset_width());
        let height = f64::from(menu.offset_height());
        let left = metrics.x.min(viewport_width - width - 8.0).max(8.0);
        let top = if metrics.line_bottom + 6.0 + height > viewport_height - 8.0
            && metrics.line_top - 6.0 - height > 8.0
        {
            metrics.line_top - 6.0 - height
        } else {
            metrics.line_bottom + 6.0
        };
        let style = menu.style();
        let _ = style.set_property("left", &format!("{left}px"));
        let _ = style.set_property("top", &format!("{top}px"));
        if let Ok(Some(item)) = menu.query_selector(".slash-item.selected") {
            let options = web_sys::ScrollIntoViewOptions::new();
            options.set_block(web_sys::ScrollLogicalPosition::Nearest);
            item.scroll_into_view_with_scroll_into_view_options(&options);
        }
    }

    /// Keys while the menu is open. Returns whether the key was handled.
    pub(crate) fn slash_keydown(
        &self,
        event: &web_sys::KeyboardEvent,
        textarea: &web_sys::HtmlTextAreaElement,
    ) -> bool {
        let Some(block_id) = source_block(textarea) else {
            return false;
        };
        let (count, selected, start) = {
            let inner = self.0.borrow();
            match inner.slash_menu.as_ref() {
                Some(menu) if menu.block_id == block_id => {
                    (menu.commands.len(), menu.selected, menu.start)
                }
                _ => return false,
            }
        };
        let step = |delta: isize| {
            let next = (selected as isize + delta).rem_euclid(count as isize) as usize;
            if let Some(menu) = self.0.borrow_mut().slash_menu.as_mut() {
                menu.selected = next;
            }
            self.show_slash_menu(textarea);
            true
        };
        match event.key().as_str() {
            "ArrowDown" => step(1),
            "ArrowUp" => step(-1),
            "Tab" if event.shift_key() => step(-1),
            "Enter" | "Tab" => {
                let command = self
                    .0
                    .borrow()
                    .slash_menu
                    .as_ref()
                    .and_then(|menu| menu.commands.get(menu.selected).copied());
                match command {
                    Some(command) => {
                        self.run_slash_command(command);
                        true
                    }
                    None => false,
                }
            }
            "Escape" => {
                self.0.borrow_mut().slash_dismissed = Some((block_id, start));
                self.close_slash_menu();
                true
            }
            _ => false,
        }
    }

    /// Runs `command` for the open menu's `/query`.
    pub(crate) fn run_slash_command(&self, command: &str) {
        let Some((block_id, start)) = self
            .0
            .borrow()
            .slash_menu
            .as_ref()
            .map(|menu| (menu.block_id, menu.start))
        else {
            return;
        };
        self.close_slash_menu();
        let Some(textarea) = source_for(block_id) else {
            return;
        };
        let target = TextTarget::Markdown(block_id);
        self.commit_source(&textarea, target);
        self.history_boundary();
        let value = textarea.value();
        let caret = textarea.selection_start().ok().flatten().unwrap_or(start);

        match page_text::apply_command(&value, start, caret, command) {
            CommandEdit::Text { text, caret } => {
                self.replace_source(&textarea, target, &text, caret);
                // The block's style follows its new kind.
                let kind = page_text::source_kind(&text).as_str();
                let classes = textarea.class_list();
                for index in (0..classes.length()).rev() {
                    if let Some(class) = classes
                        .item(index)
                        .filter(|class| class.starts_with("source-"))
                    {
                        let _ = classes.remove_1(&class);
                    }
                }
                let _ = classes.add_1(&format!("source-{kind}"));
                if let Some(section) = textarea.closest(".page-block").ok().flatten() {
                    let _ = section.set_attribute("data-md", kind);
                }
            }
            CommandEdit::Block {
                kind,
                remaining,
                caret,
            } => self.insert_command_block(&textarea, block_id, kind, &remaining, caret),
        }
    }

    fn insert_command_block(
        &self,
        textarea: &web_sys::HtmlTextAreaElement,
        block_id: BlockId,
        kind: &str,
        remaining: &str,
        caret: u32,
    ) {
        let target = TextTarget::Markdown(block_id);
        let Some(index) = ({
            let inner = self.0.borrow();
            Driver::effective_document(&inner)
                .blocks
                .iter()
                .position(|block| block.id == block_id)
        }) else {
            return;
        };
        self.replace_source(textarea, target, remaining, caret);
        let empty = remaining.trim().is_empty();
        let new_id = BlockId::new_v4();
        match kind {
            "math" => {
                let content = BlockContent::Math {
                    source: String::new(),
                };
                if empty {
                    // The equation takes the empty line's place.
                    self.insert_block_op(index, new_id, content);
                    self.dispatch_page_op(DocumentOp::DeleteBlock { block_id });
                } else {
                    self.insert_block_op(index + 1, new_id, content);
                }
                self.reopen_at(new_id, 0);
            }
            "space" | "divider" => {
                let content = if kind == "space" {
                    BlockContent::Space {
                        min_height: inkstone_core::DEFAULT_SPACE_HEIGHT,
                    }
                } else {
                    BlockContent::Divider
                };
                if empty {
                    // It goes above the line, which stays to keep writing.
                    self.insert_block_op(index, new_id, content);
                    self.reopen_at(block_id, 0);
                } else {
                    self.insert_block_op(index + 1, new_id, content);
                    let after = BlockId::new_v4();
                    self.insert_block_op(
                        index + 2,
                        after,
                        BlockContent::Markdown {
                            text: String::new(),
                        },
                    );
                    self.reopen_at(after, 0);
                }
            }
            _ => {}
        }
    }
}
