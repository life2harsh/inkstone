use inkstone_core::document::InkstoneDocument;
use inkstone_core::{Comment, CommentId, DocId, DocumentOp, InkAnchor};
use leptos::prelude::*;
use uuid::Uuid;
use wasm_bindgen::JsCast;

use crate::{
    DriverInner,
    icons::svg,
    page_surface::PageSurface,
    ui::{Ui, driver, put, state},
};

const CARD_WIDTH: f64 = 320.0;

#[derive(Clone, Default, PartialEq)]
pub(crate) struct Pins {
    doc: Option<DocId>,
    offset: (f64, f64),
    width: f64,
    threads: Vec<Thread>,
    draft: Option<(f32, f32)>,
    me: Me,
}

#[derive(Clone, PartialEq)]
struct Thread {
    id: CommentId,
    at: (f32, f32),
    root: Comment,
    replies: Vec<(CommentId, Comment)>,
}

#[derive(Clone, Default, PartialEq)]
struct Me {
    id: Option<Uuid>,
    name: String,
    can_comment: bool,
    owner: bool,
}

impl Me {
    fn wrote(&self, comment: &Comment) -> bool {
        match (self.id, comment.author_id) {
            (Some(me), Some(author)) => me == author,
            (None, None) => comment.author == self.name,
            _ => false,
        }
    }
}

pub(crate) fn publish(page: &mut PageSurface, doc: &InkstoneDocument, inner: &DriverInner) {
    let ui = state();
    let previous = ui.comments.with_untracked(|pins| {
        (
            pins.doc,
            pins.threads.iter().map(|t| t.id).collect::<Vec<_>>(),
        )
    });
    if previous.0 != Some(doc.id) {
        put(ui.comment_open, None);
        put(ui.comment_draft, None);
    }
    let draft = ui.comment_draft.get_untracked();
    let mut pins = doc
        .comments
        .iter()
        .filter(|(_, comment)| comment.parent.is_none())
        .filter_map(|(id, comment)| Some((*id, comment.anchor.clone()?)))
        .collect::<Vec<_>>();
    if let Some(anchor) = &draft {
        pins.push((Uuid::nil(), anchor.clone()));
    }
    let Some((offset, width, mut points)) = page.comment_points(
        doc,
        inner.text_client_state.as_ref(),
        inner.editing_block,
        &pins,
    ) else {
        put(ui.comments, Pins::default());
        return;
    };
    let draft = draft.and_then(|_| {
        pins.pop();
        points.pop()
    });
    let mut threads = pins
        .into_iter()
        .zip(points)
        .map(|((id, _), at)| {
            let mut replies = doc
                .comments
                .iter()
                .filter(|(_, reply)| reply.parent == Some(id))
                .map(|(reply_id, reply)| (*reply_id, reply.clone()))
                .collect::<Vec<_>>();
            replies.sort_by_key(|(reply_id, reply)| (reply.created_at_ms, *reply_id));
            Thread {
                id,
                at,
                root: doc.comments[&id].clone(),
                replies,
            }
        })
        .collect::<Vec<_>>();
    threads.sort_by(|a, b| a.at.1.total_cmp(&b.at.1).then(a.at.0.total_cmp(&b.at.0)));
    if let Some(open) = ui.comment_open.get_untracked() {
        if previous.1.contains(&open) && !threads.iter().any(|thread| thread.id == open) {
            ui.comment_open.set(None);
        }
    }
    let guest = inner.guest_name.clone();
    put(
        ui.comments,
        Pins {
            doc: Some(doc.id),
            offset,
            width,
            threads,
            draft,
            me: Me {
                id: inner
                    .auth_user
                    .as_ref()
                    .map(|user| user.id)
                    .filter(|_| guest.is_none()),
                name: guest
                    .or_else(|| inner.auth_user.as_ref().map(|user| user.email.clone()))
                    .unwrap_or_else(|| "You".into()),
                can_comment: inner.selected_path.is_some()
                    && crate::can_edit_workspace(inner)
                    && crate::document_ready(inner),
                owner: crate::current_user_is_owner(inner),
            },
        },
    );
}

fn send(op: DocumentOp) {
    driver().dispatch_page_op(op);
}

fn new_comment(
    me: &Me,
    anchor: Option<InkAnchor>,
    parent: Option<CommentId>,
    body: &str,
) -> Comment {
    Comment {
        anchor,
        parent,
        author_id: me.id,
        author: me.name.clone(),
        created_at_ms: js_sys::Date::now() as i64,
        edited_at_ms: None,
        body: body.trim().to_string(),
        resolved: false,
    }
}

fn display_name(me: &Me, comment: &Comment) -> String {
    if me.wrote(comment) {
        return "You".into();
    }
    comment
        .author
        .split_once('@')
        .map_or(comment.author.as_str(), |(name, _)| name)
        .to_string()
}

fn initial(comment: &Comment) -> String {
    comment
        .author
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or("?".into(), |c| c.to_uppercase().to_string())
}

fn hue(comment: &Comment) -> String {
    let hash = comment.author.bytes().fold(7u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    format!("--hue: {}", hash % 360)
}

fn ago(at_ms: i64) -> String {
    let seconds = ((js_sys::Date::now() as i64 - at_ms) / 1000).max(0);
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86_400 => format!("{}h ago", seconds / 3600),
        86_400..604_800 => format!("{}d ago", seconds / 86_400),
        _ => {
            let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at_ms as f64));
            String::from(date.to_locale_date_string("default", &wasm_bindgen::JsValue::UNDEFINED))
        }
    }
}

fn grow(event: &web_sys::Event) {
    if let Some(area) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let style = area.style();
        let _ = style.set_property("height", "auto");
        let _ = style.set_property("height", &format!("{}px", area.scroll_height()));
    }
}

fn submits(event: &web_sys::KeyboardEvent) -> bool {
    event.key() == "Enter" && (event.ctrl_key() || event.meta_key())
}

fn focus_later(node: NodeRef<leptos::html::Textarea>) {
    request_animation_frame(move || {
        if let Some(area) = node.get_untracked() {
            crate::page_driver::focus_in_place(&area);
        }
    });
}

fn close(ui: Ui) {
    ui.comment_open.set(None);
    ui.comment_draft.set(None);
}

fn card_style(ui: Ui, at: Option<(f32, f32)>) -> String {
    ui.comments.with(|pins| {
        let Some((x, y)) = at else {
            return "display: none".into();
        };
        let width = CARD_WIDTH.min(pins.width - 16.0);
        let left = (pins.offset.0 + f64::from(x) - 6.0)
            .min(pins.width - width - 8.0)
            .max(8.0);
        format!(
            "left: {}px; top: {}px; width: {width}px",
            left - pins.offset.0,
            f64::from(y) + 32.0
        )
    })
}

pub(crate) fn layer(ui: Ui) -> impl IntoView {
    let drafting = Memo::new(move |_| ui.comment_draft.with(Option::is_some));
    let draft_text = RwSignal::new(String::new());
    let visible = move || {
        let show = ui.show_resolved.get();
        let open = ui.comment_open.get();
        ui.comments.with(|pins| {
            pins.threads
                .iter()
                .filter(|thread| show || !thread.root.resolved || open == Some(thread.id))
                .map(|thread| thread.id)
                .collect::<Vec<_>>()
        })
    };
    let _ = window_event_listener(leptos::ev::pointerdown, move |event| {
        let inside = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| {
                element
                    .closest(".comment-layer, .comment-list")
                    .ok()
                    .flatten()
            })
            .is_some();
        if !inside {
            ui.comment_open.set(None);
            if draft_text.with_untracked(|text| text.trim().is_empty()) {
                ui.comment_draft.set(None);
            }
        }
    });
    let _ = window_event_listener(leptos::ev::keydown, move |event| {
        let open = ui.comment_open.with_untracked(Option::is_some)
            || ui.comment_draft.with_untracked(Option::is_some);
        let focus_free = document().active_element().is_none_or(|active| {
            active.tag_name() == "BODY" || active.closest(".comment-layer").ok().flatten().is_some()
        });
        if event.key() == "Escape" && open && focus_free {
            event.prevent_default();
            draft_text.set(String::new());
            close(ui);
        }
    });
    view! {
        <div
            class="comment-layer"
            style=move || ui.comments.with(|pins| {
                format!("left: {}px; top: {}px", pins.offset.0, pins.offset.1)
            })
        >
            <For each=visible key=|id| *id let:id>
                {pin(ui, id)}
            </For>
            {move || {
                let draft = ui.comments.with(|pins| pins.draft).filter(|_| drafting.get());
                draft.map(|(x, y)| {
                    view! {
                        <span class="comment-pin draft" style=format!("left: {x}px; top: {y}px")>
                            {svg("comment", "icon")}
                        </span>
                    }
                })
            }}
            {move || ui.comment_open.get().map(|id| thread_card(ui, id))}
            {move || drafting.get().then(|| draft_card(ui, draft_text))}
        </div>
    }
}

fn pin(ui: Ui, id: CommentId) -> impl IntoView {
    let thread = Memo::new(move |_| {
        ui.comments
            .with(|pins| pins.threads.iter().find(|thread| thread.id == id).cloned())
    });
    let open = move || ui.comment_open.get() == Some(id);
    view! {
        <button
            class=move || {
                let resolved = thread.with(|t| t.as_ref().is_some_and(|t| t.root.resolved));
                format!(
                    "comment-pin{}{}",
                    if open() { " open" } else { "" },
                    if resolved { " resolved" } else { "" },
                )
            }
            style=move || {
                thread.with(|t| {
                    t.as_ref().map_or("display: none".into(), |t| {
                        format!("left: {}px; top: {}px; {}", t.at.0, t.at.1, hue(&t.root))
                    })
                })
            }
            data-comment-id=id.to_string()
            aria-expanded=move || crate::ui::bool_str(open())
            aria-label=move || {
                thread.with(|t| {
                    t.as_ref().map_or(String::new(), |t| {
                        let replies = t.replies.len();
                        format!(
                            "Comment by {}{}",
                            t.root.author,
                            match replies {
                                0 => String::new(),
                                1 => ", 1 reply".into(),
                                n => format!(", {n} replies"),
                            }
                        )
                    })
                })
            }
            on:click=move |_| {
                ui.comment_draft.set(None);
                ui.comment_open.update(|current| {
                    *current = if *current == Some(id) { None } else { Some(id) };
                });
            }
        >
            <span class="comment-pin-initial">
                {move || thread.with(|t| t.as_ref().map(|t| initial(&t.root)))}
            </span>
            {move || {
                thread.with(|t| {
                    t.as_ref()
                        .map(|t| t.replies.len())
                        .filter(|count| *count > 0)
                        .map(|count| view! { <span class="comment-pin-count">{count + 1}</span> })
                })
            }}
        </button>
    }
}

fn thread_card(ui: Ui, id: CommentId) -> impl IntoView {
    let thread = Memo::new(move |_| {
        ui.comments
            .with(|pins| pins.threads.iter().find(|thread| thread.id == id).cloned())
    });
    let me = Memo::new(move |_| ui.comments.with(|pins| pins.me.clone()));
    let resolved = move || thread.with(|t| t.as_ref().is_some_and(|t| t.root.resolved));
    let entries = move || {
        thread.with(|t| {
            t.as_ref()
                .map(|t| {
                    std::iter::once((t.id, t.root.clone()))
                        .chain(t.replies.iter().cloned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    let reply = RwSignal::new(String::new());
    let input = NodeRef::<leptos::html::Textarea>::new();
    let post = move || {
        let body = reply.get_untracked();
        if body.trim().is_empty() {
            return;
        }
        let comment = new_comment(&me.get_untracked(), None, Some(id), &body);
        reply.set(String::new());
        if let Some(area) = input.get_untracked() {
            let _ = web_sys::HtmlElement::style(&area).set_property("height", "auto");
        }
        send(DocumentOp::AddComment {
            comment_id: Uuid::new_v4(),
            comment,
        });
    };
    view! {
        <div
            class="comment-card"
            role="dialog"
            aria-label="Comment thread"
            data-comment-thread=id.to_string()
            style=move || card_style(ui, thread.with(|t| t.as_ref().map(|t| t.at)))
        >
            <div class="comment-card-head">
                <span class="comment-card-title">
                    {move || if resolved() { "Resolved" } else { "Comment" }}
                </span>
                <Show when=move || me.with(|me| me.can_comment)>
                    <button
                        class=move || if resolved() { "comment-icon-btn on" } else { "comment-icon-btn" }
                        aria-label=move || if resolved() { "Reopen thread" } else { "Resolve thread" }
                        title=move || if resolved() { "Reopen" } else { "Resolve" }
                        on:click=move |_| {
                            let reopen = resolved();
                            send(DocumentOp::ResolveComment { comment_id: id, resolved: !reopen });
                            if !reopen && !ui.show_resolved.get_untracked() {
                                ui.comment_open.set(None);
                            }
                        }
                    >
                        {svg("check", "icon")}
                    </button>
                </Show>
                <button
                    class="comment-icon-btn"
                    aria-label="Close comment"
                    title="Close"
                    on:click=move |_| close(ui)
                >
                    {svg("x", "icon")}
                </button>
            </div>
            <div class="comment-entries">
                <For
                    each=entries
                    key=|(entry_id, comment)| (*entry_id, comment.edited_at_ms, comment.body.len())
                    let:entry
                >
                    {entry_view(me, id, entry.0, entry.1)}
                </For>
            </div>
            <Show when=move || me.with(|me| me.can_comment) && !resolved()>
                <form
                    class="comment-compose"
                    on:submit=move |event| {
                        event.prevent_default();
                        post();
                    }
                >
                    <textarea
                        node_ref=input
                        class="comment-input"
                        rows="1"
                        placeholder="Reply"
                        aria-label="Reply"
                        prop:value=move || reply.get()
                        on:input=move |event| {
                            reply.set(event_target_value(&event));
                            grow(&event);
                        }
                        on:keydown=move |event| {
                            if submits(&event) {
                                event.prevent_default();
                                post();
                            }
                        }
                    ></textarea>
                    <button
                        type="submit"
                        class="comment-send"
                        aria-label="Send reply"
                        disabled=move || reply.with(|text| text.trim().is_empty())
                    >
                        {svg("send", "icon")}
                    </button>
                </form>
            </Show>
        </div>
    }
}

fn entry_view(me: Memo<Me>, thread: CommentId, id: CommentId, comment: Comment) -> impl IntoView {
    let editing = RwSignal::new(false);
    let armed = RwSignal::new(false);
    let text = RwSignal::new(comment.body.clone());
    let edit = NodeRef::<leptos::html::Textarea>::new();
    let is_root = id == thread;
    let (own, can_delete, can_comment, name) = me.with_untracked(|me| {
        (
            me.wrote(&comment),
            me.wrote(&comment) || me.owner,
            me.can_comment,
            display_name(me, &comment),
        )
    });
    let save = move || {
        let body = text.get_untracked();
        if body.trim().is_empty() {
            return;
        }
        editing.set(false);
        send(DocumentOp::EditComment {
            comment_id: id,
            body: body.trim().to_string(),
        });
    };
    let full_time = String::from(
        js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(
            comment.created_at_ms as f64,
        ))
        .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED),
    );
    let body = comment.body.clone();
    view! {
        <div class="comment-entry" data-comment-entry=id.to_string()>
            <div class="comment-meta">
                <span class="comment-avatar" style=hue(&comment)>{initial(&comment)}</span>
                <span class="comment-author" title=comment.author.clone()>{name}</span>
                <span class="comment-time" title=full_time>
                    {ago(comment.created_at_ms)}
                    {comment.edited_at_ms.map(|_| " · edited")}
                </span>
                <span class="comment-actions">
                    <Show when=move || own && can_comment && !editing.get()>
                        <button
                            class="comment-icon-btn"
                            aria-label="Edit comment"
                            title="Edit"
                            on:click=move |_| {
                                editing.set(true);
                                focus_later(edit);
                            }
                        >
                            {svg("pencil", "icon")}
                        </button>
                    </Show>
                    <Show when=move || can_delete && can_comment>
                        <button
                            class=move || if armed.get() { "comment-icon-btn danger armed" } else { "comment-icon-btn danger" }
                            aria-label=move || match (armed.get(), is_root) {
                                (true, _) => "Confirm delete",
                                (false, true) => "Delete thread",
                                (false, false) => "Delete reply",
                            }
                            title=move || if armed.get() { "Click again to delete" } else { "Delete" }
                            on:click=move |_| {
                                if armed.get_untracked() {
                                    send(DocumentOp::DeleteComment { comment_id: id });
                                } else {
                                    armed.set(true);
                                    set_timeout(
                                        move || {
                                            let _ = armed.try_set(false);
                                        },
                                        std::time::Duration::from_secs(3),
                                    );
                                }
                            }
                        >
                            {move || if armed.get() { view! { <span class="comment-confirm">"Delete?"</span> }.into_any() } else { svg("trash", "icon").into_any() }}
                        </button>
                    </Show>
                </span>
            </div>
            {move || {
                if editing.get() {
                    view! {
                        <div class="comment-edit">
                            <textarea
                                node_ref=edit
                                class="comment-input"
                                aria-label="Edit comment"
                                prop:value=move || text.get()
                                on:input=move |event| {
                                    text.set(event_target_value(&event));
                                    grow(&event);
                                }
                                on:keydown=move |event| {
                                    if submits(&event) {
                                        event.prevent_default();
                                        save();
                                    } else if event.key() == "Escape" {
                                        event.prevent_default();
                                        event.stop_propagation();
                                        editing.set(false);
                                    }
                                }
                            ></textarea>
                            <div class="comment-buttons">
                                <button class="comment-btn" on:click=move |_| editing.set(false)>
                                    "Cancel"
                                </button>
                                <button
                                    class="comment-btn primary"
                                    disabled=move || text.with(|text| text.trim().is_empty())
                                    on:click=move |_| save()
                                >
                                    "Save"
                                </button>
                            </div>
                        </div>
                    }
                    .into_any()
                } else {
                    view! { <p class="comment-body">{body.clone()}</p> }.into_any()
                }
            }}
        </div>
    }
}

fn draft_card(ui: Ui, text: RwSignal<String>) -> impl IntoView {
    let input = NodeRef::<leptos::html::Textarea>::new();
    focus_later(input);
    let post = move || {
        let body = text.get_untracked();
        let Some(anchor) = ui.comment_draft.get_untracked() else {
            return;
        };
        if body.trim().is_empty() {
            return;
        }
        let me = ui.comments.with_untracked(|pins| pins.me.clone());
        let id = Uuid::new_v4();
        send(DocumentOp::AddComment {
            comment_id: id,
            comment: new_comment(&me, Some(anchor), None, &body),
        });
        text.set(String::new());
        ui.comment_draft.set(None);
        ui.comment_open.set(Some(id));
    };
    let cancel = move || {
        text.set(String::new());
        ui.comment_draft.set(None);
    };
    view! {
        <div
            class="comment-card"
            role="dialog"
            aria-label="New comment"
            style=move || card_style(ui, ui.comments.with(|pins| pins.draft))
        >
            <textarea
                node_ref=input
                class="comment-input"
                rows="2"
                placeholder="Add a comment"
                aria-label="New comment"
                prop:value=move || text.get()
                on:input=move |event| {
                    text.set(event_target_value(&event));
                    grow(&event);
                }
                on:keydown=move |event| {
                    if submits(&event) {
                        event.prevent_default();
                        post();
                    }
                }
            ></textarea>
            <div class="comment-buttons">
                <button class="comment-btn" on:click=move |_| cancel()>"Cancel"</button>
                <button
                    class="comment-btn primary"
                    disabled=move || text.with(|text| text.trim().is_empty())
                    on:click=move |_| post()
                >
                    "Comment"
                </button>
            </div>
        </div>
    }
}

pub(crate) fn start_draft(anchor: Option<InkAnchor>) {
    let ui = state();
    ui.comment_open.set(None);
    ui.comment_draft.set(anchor);
}

pub(crate) fn reveal(id: CommentId) {
    let ui = state();
    ui.comment_draft.set(None);
    ui.comment_open.set(Some(id));
    let Some((offset, at)) = ui.comments.with_untracked(|pins| {
        pins.threads
            .iter()
            .find(|thread| thread.id == id)
            .map(|thread| (pins.offset, thread.at))
    }) else {
        return;
    };
    let window = web_sys::window().expect("window");
    if window
        .inner_width()
        .ok()
        .and_then(|width| width.as_f64())
        .is_some_and(|width| width <= 1180.0)
    {
        crate::ui::close_panels();
    }
    if let Some(scroller) = window
        .document()
        .and_then(|document| document.query_selector(".main-content").ok().flatten())
    {
        let options = web_sys::ScrollToOptions::new();
        options.set_top(offset.1 + f64::from(at.1) - f64::from(scroller.client_height()) / 3.0);
        options.set_behavior(web_sys::ScrollBehavior::Smooth);
        scroller.scroll_to_with_scroll_to_options(&options);
    }
}

pub(crate) fn list(ui: Ui) -> impl IntoView {
    let rows = move || {
        let show = ui.show_resolved.get();
        ui.comments.with(|pins| {
            pins.threads
                .iter()
                .filter(|thread| show || !thread.root.resolved)
                .map(|thread| (thread.clone(), display_name(&pins.me, &thread.root)))
                .collect::<Vec<_>>()
        })
    };
    view! {
        <div class="comment-list">
            <label class="comment-filter">
                <input
                    type="checkbox"
                    prop:checked=move || ui.show_resolved.get()
                    on:change=move |event| {
                        ui.show_resolved.set(event_target_checked(&event));
                    }
                />
                "Show resolved"
            </label>
            {move || {
                let rows = rows();
                if rows.is_empty() {
                    view! {
                        <p class="comment-empty">
                            "No comments yet. Pick the comment tool in the dock, then tap the page."
                        </p>
                    }
                    .into_any()
                } else {
                    rows.into_iter()
                        .map(|(thread, name)| {
                            let id = thread.id;
                            let replies = thread.replies.len();
                            view! {
                                <button
                                    class=if thread.root.resolved { "comment-list-item resolved" } else { "comment-list-item" }
                                    on:click=move |_| reveal(id)
                                >
                                    <span class="comment-meta">
                                        <span class="comment-avatar" style=hue(&thread.root)>
                                            {initial(&thread.root)}
                                        </span>
                                        <span class="comment-author">{name}</span>
                                        <span class="comment-time">{ago(thread.root.created_at_ms)}</span>
                                    </span>
                                    <span class="comment-snippet">{thread.root.body.clone()}</span>
                                    {(replies > 0 || thread.root.resolved).then(|| view! {
                                        <span class="comment-list-foot">
                                            {match replies {
                                                0 => String::new(),
                                                1 => "1 reply".into(),
                                                n => format!("{n} replies"),
                                            }}
                                            {thread.root.resolved.then_some(" Resolved")}
                                        </span>
                                    })}
                                </button>
                            }
                        })
                        .collect_view()
                        .into_any()
                }
            }}
        </div>
    }
}
