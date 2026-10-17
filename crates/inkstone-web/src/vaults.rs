use base64::Engine;
use inkstone_core::protocol::{
    AuthUserResponse, EncryptedGuestName, GuestJoinRequest, GuestJoinResponse,
    PublicLinkAccessRequest, PublicLinkRequest, PublicLinkResponse, UpdateWorkspaceRequest,
    WorkspaceKeyEnvelopeResponse, WorkspaceMemberResponse, WorkspaceResponse,
};
use wasm_bindgen::{JsCast, JsValue};

use crate::{
    AuthPhase, Driver, DriverInner, WorkspaceMetadata, api, can_manage_members, crypto,
    decrypt_workspace_metadata, icons, workspace_key_metadata,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultAccess {
    Private,
    Shared,
    Public,
}

impl VaultAccess {
    const ALL: [VaultAccess; 3] = [
        VaultAccess::Private,
        VaultAccess::Shared,
        VaultAccess::Public,
    ];

    fn key(self) -> &'static str {
        match self {
            VaultAccess::Private => "private",
            VaultAccess::Shared => "shared",
            VaultAccess::Public => "public",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|access| access.key() == key)
    }

    fn icon(self) -> &'static str {
        match self {
            VaultAccess::Private => "lock",
            VaultAccess::Shared => "users",
            VaultAccess::Public => "globe",
        }
    }

    fn title(self) -> &'static str {
        match self {
            VaultAccess::Private => "Private",
            VaultAccess::Shared => "Shared",
            VaultAccess::Public => "Public link",
        }
    }

    fn choice_text(self) -> &'static str {
        match self {
            VaultAccess::Private => "Only you can open it.",
            VaultAccess::Shared => "People you invite. You'll pick them next.",
            VaultAccess::Public => {
                "Anyone with the link can read, or edit as a guest if you allow it. No account needed."
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewVaultDraft {
    pub name: String,
    pub access: VaultAccess,
    pub busy: bool,
    pub error: Option<String>,
}

pub fn access_of(workspace: &WorkspaceResponse) -> VaultAccess {
    if workspace.public_link {
        VaultAccess::Public
    } else if workspace.member_count > 1 {
        VaultAccess::Shared
    } else {
        VaultAccess::Private
    }
}

#[derive(Debug, Clone)]
pub struct GuestJoin {
    pub workspace: WorkspaceResponse,
    pub key: [u8; 32],
    pub link: String,
    pub name: String,
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GuestSession {
    token: String,
    link: String,
}

const GUEST_NAME_KEY: &str = "inkstone.guest-name";
const MAX_GUEST_NAME_CHARS: usize = 60;

fn guest_session_key(workspace_id: uuid::Uuid) -> String {
    format!("guest-session:{workspace_id}")
}

fn remembered_guest_name() -> String {
    web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(GUEST_NAME_KEY).ok().flatten())
        .unwrap_or_default()
}

fn remember_guest_name(name: &str) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(GUEST_NAME_KEY, name);
    }
}

pub fn clean_guest_name(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!name.is_empty() && name.chars().count() <= MAX_GUEST_NAME_CHARS).then_some(name)
}

pub fn guest_name(
    member: &WorkspaceMemberResponse,
    workspace_id: uuid::Uuid,
    key: &[u8; 32],
) -> Option<String> {
    let name = member.guest_name.as_ref()?;
    decrypt_guest_name(name, workspace_id, key)
}

fn decrypt_guest_name(
    name: &EncryptedGuestName,
    workspace_id: uuid::Uuid,
    key: &[u8; 32],
) -> Option<String> {
    crypto::decrypt(
        key,
        &name.ciphertext_b64,
        &name.nonce_b64,
        &crypto::guest_name_aad(workspace_id),
    )
    .ok()
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .and_then(|name| clean_guest_name(&name))
}

pub fn render_guest_join(document: &web_sys::Document, card: &web_sys::Element, join: &GuestJoin) {
    card.append_child(&text(document, "h2", "", "Join as a guest"))
        .unwrap();
    card.append_child(&text(
        document,
        "p",
        "auth-help",
        "You can edit this vault without an account. Pick a name so others know who wrote what.",
    ))
    .unwrap();
    let label = text(document, "label", "", "Your name");
    label.set_attribute("for", "guest-name").unwrap();
    card.append_child(&label).unwrap();
    let input = element(document, "input", "guest-name");
    input.set_id("guest-name");
    input.set_attribute("type", "text").unwrap();
    input.set_attribute("autocomplete", "nickname").unwrap();
    input
        .set_attribute("maxlength", &MAX_GUEST_NAME_CHARS.to_string())
        .unwrap();
    input.set_attribute("data-focus-key", "guest-name").unwrap();
    input
        .dyn_ref::<web_sys::HtmlInputElement>()
        .unwrap()
        .set_value(&join.name);
    card.append_child(&input).unwrap();
    if let Some(error) = &join.error {
        let line = text(document, "p", "auth-error", error);
        line.set_attribute("role", "alert").unwrap();
        card.append_child(&line).unwrap();
    }
    let actions = element(document, "div", "guest-join-actions");
    let join_button = text(
        document,
        "button",
        "primary-btn guest-join-btn",
        if join.busy { "Joining…" } else { "Join" },
    );
    join_button.set_attribute("type", "button").unwrap();
    join_button
        .set_attribute("data-vault-action", "guest-join")
        .unwrap();
    let view_button = text(document, "button", "text-btn guest-view-btn", "Just read");
    view_button.set_attribute("type", "button").unwrap();
    view_button
        .set_attribute("data-vault-action", "guest-view")
        .unwrap();
    if join.busy {
        join_button.set_attribute("disabled", "").unwrap();
        view_button.set_attribute("disabled", "").unwrap();
    }
    actions.append_child(&join_button).unwrap();
    actions.append_child(&view_button).unwrap();
    card.append_child(&actions).unwrap();
    card.append_child(&text(
        document,
        "p",
        "settings-note",
        "Your name is encrypted with the vault, so the server can't read it either.",
    ))
    .unwrap();
}

pub fn access_summary(workspace: &WorkspaceResponse) -> String {
    let people = match workspace.member_count {
        n if n > 1 => Some(format!("{n} people")),
        _ => None,
    };
    let link = if workspace.public_link_editable {
        "Edit link"
    } else {
        "Public link"
    };
    match (workspace.public_link, people) {
        (true, Some(people)) => format!("{link} · {people}"),
        (true, None) => link.into(),
        (false, Some(people)) => format!("Shared · {people}"),
        (false, None) => "Private".into(),
    }
}

pub fn link_url(token: &str, key: &[u8; 32]) -> String {
    let location = web_sys::window().map(|window| window.location());
    let origin = location
        .as_ref()
        .and_then(|location| location.origin().ok())
        .unwrap_or_default();
    let path = location
        .as_ref()
        .and_then(|location| location.pathname().ok())
        .unwrap_or_else(|| "/".into());
    format!(
        "{origin}{path}#view={token}&key={}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
    )
}

pub fn parse_view_fragment(hash: &str) -> Option<(String, [u8; 32])> {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut token = None;
    let mut key = None;
    for part in hash.trim_start_matches('#').split('&') {
        if let Some(value) = part.strip_prefix("view=") {
            token = engine
                .decode(value)
                .ok()
                .filter(|bytes| bytes.len() == 32)
                .map(|_| value.to_string());
        } else if let Some(value) = part.strip_prefix("key=") {
            key = engine
                .decode(value)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
        }
    }
    Some((token?, key?))
}

pub fn view_fragment_from_window() -> Option<(String, [u8; 32])> {
    parse_view_fragment(&web_sys::window()?.location().hash().ok()?)
}

fn current_workspace(inner: &DriverInner) -> Option<&WorkspaceResponse> {
    inner
        .workspaces
        .iter()
        .find(|workspace| Some(workspace.id) == inner.workspace_id)
}

fn element(document: &web_sys::Document, tag: &str, class: &str) -> web_sys::Element {
    let element = document.create_element(tag).unwrap();
    if !class.is_empty() {
        element.set_class_name(class);
    }
    element
}

fn text(document: &web_sys::Document, tag: &str, class: &str, value: &str) -> web_sys::Element {
    let element = element(document, tag, class);
    element.set_text_content(Some(value));
    element
}

fn action_button(
    document: &web_sys::Document,
    class: &str,
    action: &str,
    icon: Option<&str>,
    label: &str,
) -> web_sys::Element {
    let button = element(document, "button", class);
    button.set_attribute("type", "button").unwrap();
    button.set_attribute("data-vault-action", action).unwrap();
    if let Some(icon) = icon {
        button.append_child(&icons::icon(document, icon)).unwrap();
    }
    button
        .append_child(&text(document, "span", "", label))
        .unwrap();
    button
}

/// The vault name at the top of the sidebar, which opens the vault menu.
pub fn render_switcher(document: &web_sys::Document, inner: &DriverInner) -> web_sys::Element {
    let interactive = !inner.local_mode && inner.public_view.is_none();
    let switcher = element(
        document,
        if interactive { "button" } else { "div" },
        "brand vault-switcher",
    );
    if interactive {
        switcher.set_attribute("type", "button").unwrap();
        switcher.set_attribute("data-vault-action", "menu").unwrap();
        switcher.set_attribute("aria-haspopup", "menu").unwrap();
        switcher
            .set_attribute(
                "aria-expanded",
                if inner.vault_menu_open {
                    "true"
                } else {
                    "false"
                },
            )
            .unwrap();
        switcher
            .set_attribute("data-focus-key", "vault-switcher")
            .unwrap();
    }
    let mark = element(document, "span", "brand-mark");
    mark.append_child(&icons::icon(document, "mark")).unwrap();
    switcher.append_child(&mark).unwrap();
    let label = element(document, "span", "brand-text");
    label
        .append_child(&text(
            document,
            "span",
            "brand-name vault-name",
            &inner.workspace_name,
        ))
        .unwrap();
    let detail = if let Some(name) = &inner.guest_name {
        format!("Editing as {name}")
    } else if inner.public_view.is_some() {
        "View only".to_string()
    } else if inner.local_mode {
        "On this device".to_string()
    } else {
        current_workspace(inner)
            .map(access_summary)
            .unwrap_or_default()
    };
    label
        .append_child(&text(document, "span", "workspace-name", &detail))
        .unwrap();
    switcher.append_child(&label).unwrap();
    if interactive {
        let chevron = icons::icon(document, "chevrons-up-down");
        chevron
            .set_attribute("class", "icon vault-chevron")
            .unwrap();
        switcher.append_child(&chevron).unwrap();
    } else if inner.public_view.is_some() {
        let eye = icons::icon(
            document,
            if inner.guest_name.is_some() {
                "pencil"
            } else {
                "eye"
            },
        );
        eye.set_attribute("class", "icon vault-chevron").unwrap();
        switcher.append_child(&eye).unwrap();
    }
    switcher
}

pub fn render_menu(document: &web_sys::Document, inner: &DriverInner) -> web_sys::Element {
    let menu = element(document, "div", "vault-menu");
    menu.set_attribute("role", "menu").unwrap();
    menu.set_attribute("aria-label", "Vaults").unwrap();
    menu.append_child(&text(document, "div", "vault-menu-label", "Vaults"))
        .unwrap();
    let mut workspaces: Vec<&WorkspaceResponse> = inner.workspaces.iter().collect();
    let name_of = |workspace: &WorkspaceResponse| {
        inner
            .vault_names
            .get(&workspace.id)
            .cloned()
            .unwrap_or_else(|| "Locked vault".into())
    };
    workspaces.sort_by_key(|workspace| name_of(workspace).to_lowercase());
    for workspace in workspaces {
        let current = Some(workspace.id) == inner.workspace_id;
        let item = element(document, "button", "vault-menu-item");
        item.set_attribute("type", "button").unwrap();
        item.set_attribute("role", "menuitemradio").unwrap();
        item.set_attribute("aria-checked", if current { "true" } else { "false" })
            .unwrap();
        item.set_attribute("data-vault-action", "switch").unwrap();
        item.set_attribute("data-workspace-id", &workspace.id.to_string())
            .unwrap();
        item.append_child(&icons::icon(document, access_of(workspace).icon()))
            .unwrap();
        let label = element(document, "span", "vault-menu-text");
        label
            .append_child(&text(
                document,
                "span",
                "vault-menu-name",
                &name_of(workspace),
            ))
            .unwrap();
        label
            .append_child(&text(
                document,
                "span",
                "vault-menu-detail",
                &access_summary(workspace),
            ))
            .unwrap();
        item.append_child(&label).unwrap();
        if current {
            let check = icons::icon(document, "check");
            check
                .set_attribute("class", "icon vault-menu-check")
                .unwrap();
            item.append_child(&check).unwrap();
        }
        menu.append_child(&item).unwrap();
    }
    menu.append_child(&element(document, "div", "vault-menu-divider"))
        .unwrap();
    for (action, icon, label) in [
        ("new", "plus", "New vault"),
        ("share", "users", "Share this vault"),
        ("settings", "settings", "Vault settings"),
    ] {
        let item = action_button(
            document,
            "vault-menu-item vault-menu-command",
            action,
            Some(icon),
            label,
        );
        item.set_attribute("role", "menuitem").unwrap();
        menu.append_child(&item).unwrap();
    }
    menu
}

pub fn render_new_vault_dialog(
    document: &web_sys::Document,
    draft: &NewVaultDraft,
) -> web_sys::Element {
    let backdrop = element(document, "div", "settings-backdrop new-vault-backdrop");
    let dialog = element(document, "section", "new-vault-dialog");
    dialog.set_attribute("role", "dialog").unwrap();
    dialog.set_attribute("aria-modal", "true").unwrap();
    dialog
        .set_attribute("aria-labelledby", "new-vault-title")
        .unwrap();
    let heading = text(document, "h2", "", "New vault");
    heading.set_id("new-vault-title");
    dialog.append_child(&heading).unwrap();
    dialog
        .append_child(&text(
            document,
            "p",
            "new-vault-intro",
            "A vault is a separate space with its own notes, folders and people. Everything in it is end-to-end encrypted.",
        ))
        .unwrap();
    let label = text(document, "label", "new-vault-label", "Name");
    label.set_attribute("for", "new-vault-name").unwrap();
    dialog.append_child(&label).unwrap();
    let input = element(document, "input", "new-vault-name");
    input.set_id("new-vault-name");
    input.set_attribute("maxlength", "200").unwrap();
    input.set_attribute("autocomplete", "off").unwrap();
    input
        .set_attribute("placeholder", "Research, Journal, Team wiki…")
        .unwrap();
    input
        .set_attribute("data-focus-key", "new-vault-name")
        .unwrap();
    input
        .dyn_ref::<web_sys::HtmlInputElement>()
        .unwrap()
        .set_value(&draft.name);
    dialog.append_child(&input).unwrap();
    let group_label = text(document, "p", "new-vault-label", "Who can open it");
    group_label.set_id("new-vault-access-label");
    dialog.append_child(&group_label).unwrap();
    let choices = element(document, "div", "vault-access-choices");
    choices.set_attribute("role", "radiogroup").unwrap();
    choices
        .set_attribute("aria-labelledby", "new-vault-access-label")
        .unwrap();
    for access in VaultAccess::ALL {
        let selected = access == draft.access;
        let choice = element(document, "button", "vault-access-choice");
        choice.set_attribute("type", "button").unwrap();
        choice.set_attribute("role", "radio").unwrap();
        choice
            .set_attribute("aria-checked", if selected { "true" } else { "false" })
            .unwrap();
        choice.set_attribute("data-vault-action", "access").unwrap();
        choice.set_attribute("data-access", access.key()).unwrap();
        choice
            .set_attribute("data-focus-key", &format!("vault-access-{}", access.key()))
            .unwrap();
        choice
            .append_child(&icons::icon(document, access.icon()))
            .unwrap();
        let body = element(document, "span", "vault-access-text");
        body.append_child(&text(document, "strong", "", access.title()))
            .unwrap();
        body.append_child(&text(document, "span", "", access.choice_text()))
            .unwrap();
        choice.append_child(&body).unwrap();
        choices.append_child(&choice).unwrap();
    }
    dialog.append_child(&choices).unwrap();
    if let Some(error) = &draft.error {
        let error_line = text(document, "p", "settings-error", error);
        error_line.set_attribute("role", "alert").unwrap();
        dialog.append_child(&error_line).unwrap();
    }
    let actions = element(document, "div", "new-vault-actions");
    actions
        .append_child(&action_button(
            document,
            "text-btn",
            "close-new",
            None,
            "Cancel",
        ))
        .unwrap();
    let create = action_button(
        document,
        "primary-btn new-vault-create",
        "create",
        None,
        if draft.busy {
            "Creating…"
        } else {
            "Create vault"
        },
    );
    if draft.busy {
        create.set_attribute("disabled", "").unwrap();
    }
    actions.append_child(&create).unwrap();
    dialog.append_child(&actions).unwrap();
    backdrop.append_child(&dialog).unwrap();
    backdrop
}

/// Who can open the vault, and its public link, at the top of Sharing.
pub fn render_access_section(
    document: &web_sys::Document,
    content: &web_sys::Element,
    inner: &DriverInner,
) {
    let Some(workspace) = current_workspace(inner) else {
        return;
    };
    let access = access_of(workspace);
    let summary = element(document, "div", "sharing-access");
    summary.set_attribute("data-access", access.key()).unwrap();
    summary
        .append_child(&icons::icon(document, access.icon()))
        .unwrap();
    let body = element(document, "div", "sharing-access-text");
    body.append_child(&text(document, "strong", "", access.title()))
        .unwrap();
    let people = workspace.member_count;
    let detail = match access {
        VaultAccess::Private => {
            "Only you can open this vault. Invite people or turn on a public link to share it."
                .to_string()
        }
        VaultAccess::Shared => format!("{people} people can open this vault."),
        VaultAccess::Public => {
            let reach = if workspace.public_link_editable {
                "Anyone with the link can edit this vault as a guest."
            } else {
                "Anyone with the link can read this vault."
            };
            let mut detail = reach.to_string();
            if people > 1 {
                detail.push_str(&format!(" {people} people can also open it as members."));
            }
            match workspace.guest_count {
                0 => {}
                1 => detail.push_str(" 1 guest has joined."),
                guests => detail.push_str(&format!(" {guests} guests have joined.")),
            }
            detail
        }
    };
    body.append_child(&text(document, "span", "", &detail))
        .unwrap();
    summary.append_child(&body).unwrap();
    content.append_child(&summary).unwrap();

    content
        .append_child(&text(document, "h4", "", "Public link"))
        .unwrap();
    content
        .append_child(&text(
            document,
            "p",
            "settings-description",
            "Anyone with the link can open every note and file in this vault, without an account. Let them edit and they join as a guest under a name they pick. The key is part of the link, so the server still can't read anything.",
        ))
        .unwrap();
    let admin = can_manage_members(inner);
    let link = element(document, "div", "public-link-row");
    if workspace.public_link {
        match (&inner.workspace_public_link, inner.workspace_key) {
            (Some(token), Some(key)) => {
                let url = element(document, "input", "public-link-url");
                url.set_attribute("readonly", "").unwrap();
                url.set_attribute("aria-label", "Public link").unwrap();
                url.dyn_ref::<web_sys::HtmlInputElement>()
                    .unwrap()
                    .set_value(&link_url(token, &key));
                link.append_child(&url).unwrap();
                link.append_child(&action_button(
                    document,
                    "settings-row-action",
                    "link-copy",
                    Some("copy"),
                    "Copy link",
                ))
                .unwrap();
            }
            _ => {
                link.append_child(&text(
                    document,
                    "p",
                    "settings-note",
                    "The link is on, but it was made somewhere this device can't read. Reset it to get a new one.",
                ))
                .unwrap();
            }
        }
    }
    content.append_child(&link).unwrap();
    if workspace.public_link {
        let access = element(document, "div", "link-access");
        access.set_attribute("role", "radiogroup").unwrap();
        access
            .set_attribute("aria-label", "Anyone with the link can")
            .unwrap();
        access
            .append_child(&text(
                document,
                "span",
                "link-access-label",
                "Anyone with the link can",
            ))
            .unwrap();
        for (editable, icon, label) in [(false, "eye", "Read"), (true, "pencil", "Edit")] {
            let selected = workspace.public_link_editable == editable;
            let choice = action_button(
                document,
                if selected {
                    "link-access-choice selected"
                } else {
                    "link-access-choice"
                },
                "link-access",
                Some(icon),
                label,
            );
            choice.set_attribute("role", "radio").unwrap();
            choice
                .set_attribute("aria-checked", if selected { "true" } else { "false" })
                .unwrap();
            choice
                .set_attribute("data-editable", if editable { "true" } else { "false" })
                .unwrap();
            if !admin || inner.public_link_busy {
                choice.set_attribute("disabled", "").unwrap();
            }
            access.append_child(&choice).unwrap();
        }
        content.append_child(&access).unwrap();
    }
    if admin {
        let actions = element(document, "div", "public-link-actions");
        let busy = inner.public_link_busy;
        let mut buttons = Vec::new();
        if workspace.public_link {
            buttons.push(action_button(
                document,
                "settings-row-action",
                "link-enable",
                None,
                "Reset link",
            ));
            buttons.push(action_button(
                document,
                "settings-row-action danger-action",
                "link-disable",
                None,
                "Turn off link",
            ));
        } else {
            buttons.push(action_button(
                document,
                "settings-row-action",
                "link-enable",
                Some("globe"),
                "Create public link",
            ));
        }
        for button in buttons {
            if busy {
                button.set_attribute("disabled", "").unwrap();
            }
            actions.append_child(&button).unwrap();
        }
        content.append_child(&actions).unwrap();
    } else {
        content
            .append_child(&text(
                document,
                "p",
                "settings-note",
                "Only owners and admins can change the public link.",
            ))
            .unwrap();
    }
    if let Some(status) = &inner.public_link_status {
        let line = text(document, "p", "copy-status", status);
        line.set_attribute("role", "status").unwrap();
        content.append_child(&line).unwrap();
    }
    content
        .append_child(&text(document, "h4", "", "People with access"))
        .unwrap();
}

pub fn render_link_unavailable(
    document: &web_sys::Document,
    card: &web_sys::Element,
    message: &str,
) {
    card.append_child(&text(document, "h2", "", "This link isn't available"))
        .unwrap();
    card.append_child(&text(document, "p", "auth-help", message))
        .unwrap();
    let open = text(document, "a", "primary-btn link-home-btn", "Open Inkstone");
    let home = web_sys::window()
        .and_then(|window| window.location().pathname().ok())
        .unwrap_or_else(|| "/".into());
    open.set_attribute("href", &home).unwrap();
    card.append_child(&open).unwrap();
}

impl Driver {
    pub(crate) fn open_new_vault(&self) {
        let mut inner = self.0.borrow_mut();
        if inner.local_mode || inner.public_view.is_some() {
            return;
        }
        inner.vault_menu_open = false;
        inner.settings_open = false;
        inner.new_vault = Some(NewVaultDraft {
            name: String::new(),
            access: VaultAccess::Private,
            busy: false,
            error: None,
        });
        inner.pending_focus_key = Some("new-vault-name".into());
        drop(inner);
        self.render();
    }

    pub(crate) fn set_new_vault_name(&self, name: String) {
        if let Some(draft) = self.0.borrow_mut().new_vault.as_mut() {
            draft.name = name;
        }
    }

    pub(crate) fn submit_new_vault(&self) {
        let (name, access) = {
            let mut inner = self.0.borrow_mut();
            let Some(draft) = inner.new_vault.as_mut() else {
                return;
            };
            if draft.busy {
                return;
            }
            let name = draft.name.trim().to_string();
            if name.is_empty() || name.len() > 200 {
                draft.error = Some("Give the vault a name (up to 200 characters).".into());
                drop(inner);
                self.render();
                return;
            }
            draft.busy = true;
            draft.error = None;
            (name, draft.access)
        };
        self.render();
        self.create_workspace(name, access);
    }

    pub(crate) fn vault_creation_failed(&self, message: String) {
        let mut inner = self.0.borrow_mut();
        match inner.new_vault.as_mut() {
            Some(draft) => {
                draft.busy = false;
                draft.error = Some(message);
                drop(inner);
                self.render();
            }
            None => {
                drop(inner);
                self.set_sync_error(message);
            }
        }
    }

    pub(crate) async fn vault_created(&self, access: VaultAccess) {
        self.0.borrow_mut().new_vault = None;
        if access == VaultAccess::Public {
            self.set_public_link(true).await;
        }
        if access != VaultAccess::Private {
            self.open_sharing();
        } else {
            self.render();
        }
    }

    fn open_sharing(&self) {
        let mut inner = self.0.borrow_mut();
        inner.vault_menu_open = false;
        inner.settings_open = true;
        inner.settings_category = "Sharing".into();
        inner.pending_focus_key = Some("settings-content".into());
        drop(inner);
        self.render();
        self.refresh_settings_data();
    }

    /// Handles a click on anything carrying `data-vault-action`. Returns
    /// whether the click was consumed.
    pub(crate) fn handle_vault_click(&self, target: &web_sys::Element) -> bool {
        if target.class_name() == "settings-backdrop new-vault-backdrop" {
            self.close_new_vault();
            return true;
        }
        let menu_open = self.0.borrow().vault_menu_open;
        let control = target.closest("[data-vault-action]").ok().flatten();
        if menu_open && control.is_none() && target.closest(".vault-menu").ok().flatten().is_none()
        {
            self.0.borrow_mut().vault_menu_open = false;
            self.render();
        }
        let Some(control) = control else {
            return false;
        };
        let action = control
            .get_attribute("data-vault-action")
            .unwrap_or_default();
        match action.as_str() {
            "menu" => {
                let mut inner = self.0.borrow_mut();
                inner.vault_menu_open = !inner.vault_menu_open;
                drop(inner);
                self.render();
            }
            "switch" => {
                self.0.borrow_mut().vault_menu_open = false;
                let id = control
                    .get_attribute("data-workspace-id")
                    .and_then(|id| uuid::Uuid::parse_str(&id).ok());
                match id {
                    Some(id) if Some(id) != self.0.borrow().workspace_id => {
                        self.switch_workspace(id)
                    }
                    _ => self.render(),
                }
            }
            "new" => self.open_new_vault(),
            "share" => self.open_sharing(),
            "settings" => {
                let mut inner = self.0.borrow_mut();
                inner.vault_menu_open = false;
                inner.settings_open = true;
                inner.settings_category = "General".into();
                inner.pending_focus_key = Some("settings-content".into());
                drop(inner);
                self.render();
                self.refresh_settings_data();
            }
            "close-new" => {
                self.close_new_vault();
            }
            "access" => {
                let access = control
                    .get_attribute("data-access")
                    .and_then(|key| VaultAccess::from_key(&key));
                if let (Some(access), Some(draft)) =
                    (access, self.0.borrow_mut().new_vault.as_mut())
                {
                    draft.access = access;
                }
                self.render();
            }
            "create" => self.submit_new_vault(),
            "link-enable" | "link-disable" => {
                let enable = action == "link-enable";
                if !enable
                    && !web_sys::window()
                        .and_then(|window| {
                            window
                                .confirm_with_message(
                                    "Turn off the public link? Anyone using it loses access right away.",
                                )
                                .ok()
                        })
                        .unwrap_or(false)
                {
                    return true;
                }
                let this = self.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    this.set_public_link(enable).await;
                });
            }
            "link-copy" => self.copy_public_link(),
            "link-access" => {
                let editable = control.get_attribute("data-editable").as_deref() == Some("true");
                let this = self.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    this.set_link_access(editable).await;
                });
            }
            "guest-join" => self.submit_guest_join(),
            "guest-view" => self.view_without_joining(),
            _ => return false,
        }
        true
    }

    pub(crate) fn close_new_vault(&self) -> bool {
        let mut inner = self.0.borrow_mut();
        if inner.new_vault.as_ref().is_some_and(|draft| !draft.busy) {
            inner.new_vault = None;
            drop(inner);
            self.render();
            return true;
        }
        false
    }

    pub(crate) fn close_vault_menu(&self) -> bool {
        let mut inner = self.0.borrow_mut();
        if !inner.vault_menu_open {
            return false;
        }
        inner.vault_menu_open = false;
        inner.pending_focus_key = Some("vault-switcher".into());
        drop(inner);
        self.render();
        true
    }

    async fn set_public_link(&self, enable: bool) {
        let (workspace, key, description, name) = {
            let inner = self.0.borrow();
            (
                current_workspace(&inner).cloned(),
                inner.workspace_key,
                inner.workspace_description.clone(),
                inner.workspace_name.clone(),
            )
        };
        let (Some(workspace), Some(key)) = (workspace, key) else {
            return;
        };
        {
            let mut inner = self.0.borrow_mut();
            inner.public_link_busy = true;
            inner.public_link_status = None;
        }
        self.render();
        let result = async {
            let endpoint = format!("/api/workspaces/{}/public-link", workspace.id);
            let token = if enable {
                let token =
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(crypto::random_key()?);
                api::json::<WorkspaceResponse, _>(
                    "PUT",
                    &endpoint,
                    Some(&PublicLinkRequest {
                        token: token.clone(),
                        editable: workspace.public_link && workspace.public_link_editable,
                    }),
                )
                .await
                .map_err(|error| error.message)?;
                Some(token)
            } else {
                api::json::<WorkspaceResponse, serde_json::Value>("DELETE", &endpoint, None)
                    .await
                    .map_err(|error| error.message)?;
                None
            };
            self.0.borrow_mut().workspace_public_link = token.clone();
            let plaintext = serde_json::to_vec(&WorkspaceMetadata {
                name,
                description,
                public_link: token,
            })
            .map_err(|error| error.to_string())?;
            let encrypted = crypto::encrypt(
                &key,
                &plaintext,
                &crypto::workspace_metadata_aad(workspace.id),
            )?;
            api::json::<WorkspaceResponse, _>(
                "PATCH",
                &format!("/api/workspaces/{}", workspace.id),
                Some(&UpdateWorkspaceRequest {
                    encrypted_metadata_b64: encrypted.ciphertext_b64,
                    metadata_nonce_b64: encrypted.nonce_b64,
                    metadata_version: workspace.metadata_version + 1,
                }),
            )
            .await
            .map_err(|error| error.message)
        }
        .await;
        let mut inner = self.0.borrow_mut();
        inner.public_link_busy = false;
        match result {
            Ok(updated) => {
                if let Some(stored) = inner
                    .workspaces
                    .iter_mut()
                    .find(|stored| stored.id == updated.id)
                {
                    *stored = updated;
                }
                inner.public_link_status = Some(
                    if enable {
                        "Public link is on. Anyone with it can read this vault."
                    } else {
                        "Public link turned off. The old link no longer works."
                    }
                    .into(),
                );
            }
            Err(error) => {
                inner.public_link_status = Some(format!("Couldn't change the public link: {error}"))
            }
        }
        drop(inner);
        self.render();
        self.refresh_settings_data();
    }

    async fn set_link_access(&self, editable: bool) {
        let Some(workspace) = current_workspace(&self.0.borrow()).cloned() else {
            return;
        };
        if workspace.public_link_editable == editable {
            return;
        }
        {
            let mut inner = self.0.borrow_mut();
            inner.public_link_busy = true;
            inner.public_link_status = None;
        }
        self.render();
        let result = api::json::<WorkspaceResponse, _>(
            "PATCH",
            &format!("/api/workspaces/{}/public-link", workspace.id),
            Some(&PublicLinkAccessRequest { editable }),
        )
        .await;
        let mut inner = self.0.borrow_mut();
        inner.public_link_busy = false;
        match result {
            Ok(updated) => {
                if let Some(stored) = inner
                    .workspaces
                    .iter_mut()
                    .find(|stored| stored.id == updated.id)
                {
                    *stored = updated;
                }
                inner.public_link_status = Some(
                    if editable {
                        "People with the link can now edit. They'll pick a name when they join."
                    } else {
                        "The link is read-only again. Guests who joined can still read but not edit."
                    }
                    .into(),
                );
            }
            Err(error) => {
                inner.public_link_status =
                    Some(format!("Couldn't change the link: {}", error.message))
            }
        }
        drop(inner);
        self.render();
        self.refresh_settings_data();
    }

    fn copy_public_link(&self) {
        let url = {
            let inner = self.0.borrow();
            match (&inner.workspace_public_link, inner.workspace_key) {
                (Some(token), Some(key)) => link_url(token, &key),
                _ => return,
            }
        };
        let promise = web_sys::window().and_then(|window| {
            let navigator =
                js_sys::Reflect::get(window.as_ref(), &JsValue::from_str("navigator")).ok()?;
            let clipboard =
                js_sys::Reflect::get(&navigator, &JsValue::from_str("clipboard")).ok()?;
            js_sys::Reflect::get(&clipboard, &JsValue::from_str("writeText"))
                .ok()?
                .dyn_into::<js_sys::Function>()
                .ok()?
                .call1(&clipboard, &JsValue::from_str(&url))
                .ok()?
                .dyn_into::<js_sys::Promise>()
                .ok()
        });
        let Some(promise) = promise else {
            self.0.borrow_mut().public_link_status = Some(
                "Clipboard access is unavailable. Select the link and copy it yourself.".into(),
            );
            self.render();
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let copied = wasm_bindgen_futures::JsFuture::from(promise).await.is_ok();
            this.0.borrow_mut().public_link_status = Some(
                if copied {
                    "Link copied."
                } else {
                    "Copy was blocked. Select the link and copy it yourself."
                }
                .into(),
            );
            this.render();
        });
    }

    /// Names every vault the account can unlock, for the vault menu.
    pub(crate) fn load_vault_names(&self) {
        let (workspaces, user, vault_key, workspace_db) = {
            let inner = self.0.borrow();
            (
                inner
                    .workspaces
                    .iter()
                    .filter(|workspace| !inner.vault_names.contains_key(&workspace.id))
                    .cloned()
                    .collect::<Vec<_>>(),
                inner.auth_user.clone(),
                inner.account_vault_key,
                inner.workspace_db.clone(),
            )
        };
        let (Some(user), Some(vault_key), Some(workspace_db)) = (user, vault_key, workspace_db)
        else {
            return;
        };
        if workspaces.is_empty() {
            return;
        }
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut changed = false;
            for workspace in workspaces {
                let aad = crypto::workspace_key_aad(workspace.id, user.id);
                let local = workspace_db
                    .get_global_metadata(&workspace_key_metadata(workspace.id))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|json| serde_json::from_str::<crypto::Ciphertext>(&json).ok())
                    .and_then(|envelope| {
                        crypto::decrypt(
                            &vault_key,
                            &envelope.ciphertext_b64,
                            &envelope.nonce_b64,
                            &aad,
                        )
                        .ok()
                    });
                let plaintext = match local {
                    Some(plaintext) => Some(plaintext),
                    None => api::json::<WorkspaceKeyEnvelopeResponse, serde_json::Value>(
                        "GET",
                        &format!("/api/workspaces/{}/key-envelope", workspace.id),
                        None,
                    )
                    .await
                    .ok()
                    .and_then(|envelope| {
                        crypto::decrypt(
                            &vault_key,
                            &envelope.encrypted_key_b64,
                            &envelope.nonce_b64,
                            &aad,
                        )
                        .ok()
                    }),
                };
                let Some(key) = plaintext.and_then(|bytes| <[u8; 32]>::try_from(bytes).ok()) else {
                    continue;
                };
                if let Ok(metadata) = decrypt_workspace_metadata(&workspace, &key) {
                    this.0
                        .borrow_mut()
                        .vault_names
                        .insert(workspace.id, metadata.name);
                    changed = true;
                }
            }
            if changed {
                this.render();
            }
        });
    }

    /// Opens a vault through a shared link: read-only, or as a guest editor
    /// when the owner allows editing.
    pub(crate) async fn start_public_view(&self, token: String, key: [u8; 32]) {
        api::set_link_token(Some(token.clone()));
        self.0.borrow_mut().public_view = Some(token.clone());
        let result = async {
            let details =
                api::json::<PublicLinkResponse, serde_json::Value>("GET", "/api/public-link", None)
                    .await
                    .map_err(|error| match error.status {
                        401 | 403 => LINK_OFF.to_string(),
                        _ => error.message,
                    })?;
            decrypt_workspace_metadata(&details.workspace, &key).map_err(|_| {
                "This link is incomplete. Ask whoever shared it to send it again.".to_string()
            })?;
            if details.workspace.public_link_editable {
                if self.resume_guest(&details.workspace, &token, key).await? {
                    return Ok(());
                }
                let mut inner = self.0.borrow_mut();
                inner.guest_join = Some(GuestJoin {
                    workspace: details.workspace,
                    key,
                    link: token,
                    name: remembered_guest_name(),
                    busy: false,
                    error: None,
                });
                inner.auth_phase = AuthPhase::SignedOut;
                inner.pending_focus_key = Some("guest-name".into());
                drop(inner);
                self.render();
                return Ok(());
            }
            self.enter_shared(details, key, None).await
        }
        .await;
        if let Err(message) = result {
            self.shared_link_failed(message);
        }
    }

    fn shared_link_failed(&self, message: String) {
        let mut inner = self.0.borrow_mut();
        inner.guest_join = None;
        inner.public_view_error = Some(message);
        inner.auth_phase = AuthPhase::SignedOut;
        drop(inner);
        self.render();
    }

    async fn enter_shared(
        &self,
        details: PublicLinkResponse,
        key: [u8; 32],
        guest_name: Option<String>,
    ) -> Result<(), String> {
        {
            let mut inner = self.0.borrow_mut();
            inner.guest_join = None;
            inner.auth_user = Some(AuthUserResponse {
                id: details.user_id,
                email: String::new(),
                device_id: details.device_id,
            });
            if guest_name.is_some() {
                inner.device_id = Some(details.device_id);
            }
            inner.guest_name = guest_name;
            inner.workspaces = vec![details.workspace.clone()];
        }
        self.enter_workspace(details.workspace, key).await
    }

    async fn resume_guest(
        &self,
        workspace: &WorkspaceResponse,
        link: &str,
        key: [u8; 32],
    ) -> Result<bool, String> {
        let Some(workspace_db) = self.0.borrow().workspace_db.clone() else {
            return Ok(false);
        };
        let storage_key = guest_session_key(workspace.id);
        let Some(session) = workspace_db
            .get_global_metadata(&storage_key)
            .await
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<GuestSession>(&json).ok())
            .filter(|session| session.link == link)
        else {
            return Ok(false);
        };
        api::set_link_token(Some(session.token));
        let details =
            api::json::<PublicLinkResponse, serde_json::Value>("GET", "/api/public-link", None)
                .await;
        let name = details
            .as_ref()
            .ok()
            .and_then(|details| decrypt_guest_name(details.guest.as_ref()?, workspace.id, &key));
        match (details, name) {
            (Ok(details), Some(name)) => {
                self.enter_shared(details, key, Some(name)).await?;
                Ok(true)
            }
            (Err(error), _) if !matches!(error.status, 401 | 403) => Err(error.message),
            _ => {
                api::set_link_token(Some(link.to_string()));
                let _ = workspace_db.delete_global_metadata(&storage_key).await;
                Ok(false)
            }
        }
    }

    fn guest_name_from_page(&self) -> String {
        web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("guest-name"))
            .and_then(|input| input.dyn_into::<web_sys::HtmlInputElement>().ok())
            .map(|input| input.value())
            .unwrap_or_default()
    }

    pub(crate) fn submit_guest_join(&self) {
        let typed = self.guest_name_from_page();
        let join = {
            let mut inner = self.0.borrow_mut();
            let Some(join) = inner.guest_join.as_mut() else {
                return;
            };
            if join.busy {
                return;
            }
            join.name = typed.clone();
            match clean_guest_name(&typed) {
                Some(_) => {
                    join.busy = true;
                    join.error = None;
                    join.clone()
                }
                None => {
                    join.error = Some(format!(
                        "Type a name, up to {MAX_GUEST_NAME_CHARS} characters."
                    ));
                    drop(inner);
                    self.render();
                    return;
                }
            }
        };
        self.render();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(message) = this.join_as_guest(join).await {
                let mut inner = this.0.borrow_mut();
                match inner.guest_join.as_mut() {
                    Some(join) => {
                        join.busy = false;
                        join.error = Some(message);
                        drop(inner);
                        this.render();
                    }
                    None => {
                        drop(inner);
                        this.shared_link_failed(message);
                    }
                }
            }
        });
    }

    async fn join_as_guest(&self, join: GuestJoin) -> Result<(), String> {
        let name = clean_guest_name(&join.name).ok_or("Type a name first.")?;
        let encrypted = crypto::encrypt(
            &join.key,
            name.as_bytes(),
            &crypto::guest_name_aad(join.workspace.id),
        )?;
        api::set_link_token(Some(join.link.clone()));
        let joined = api::json::<GuestJoinResponse, _>(
            "POST",
            "/api/public-link/guests",
            Some(&GuestJoinRequest {
                name: EncryptedGuestName {
                    ciphertext_b64: encrypted.ciphertext_b64,
                    nonce_b64: encrypted.nonce_b64,
                },
            }),
        )
        .await
        .map_err(|error| match error.status {
            401 => LINK_OFF.to_string(),
            403 => "The owner made this link read-only. You can still read it.".to_string(),
            _ => error.message,
        })?;
        remember_guest_name(&name);
        if let Some(workspace_db) = self.0.borrow().workspace_db.clone() {
            let session = serde_json::to_string(&GuestSession {
                token: joined.token.clone(),
                link: join.link.clone(),
            })
            .map_err(|error| error.to_string())?;
            let _ = workspace_db
                .set_global_metadata(&guest_session_key(join.workspace.id), &session)
                .await;
        }
        api::set_link_token(Some(joined.token));
        let details =
            api::json::<PublicLinkResponse, serde_json::Value>("GET", "/api/public-link", None)
                .await
                .map_err(|error| error.message)?;
        self.enter_shared(details, join.key, Some(name)).await
    }

    pub(crate) fn view_without_joining(&self) {
        let Some(join) = self.0.borrow_mut().guest_join.take() else {
            return;
        };
        self.render();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            api::set_link_token(Some(join.link.clone()));
            let result = async {
                let details = api::json::<PublicLinkResponse, serde_json::Value>(
                    "GET",
                    "/api/public-link",
                    None,
                )
                .await
                .map_err(|error| match error.status {
                    401 | 403 => LINK_OFF.to_string(),
                    _ => error.message,
                })?;
                this.enter_shared(details, join.key, None).await
            }
            .await;
            if let Err(message) = result {
                this.shared_link_failed(message);
            }
        });
    }

    pub(crate) fn public_link_revoked(&self) {
        self.stop_discovery_timer();
        self.disconnect_socket();
        let mut inner = self.0.borrow_mut();
        inner.public_view_error = Some(LINK_OFF.into());
        inner.auth_phase = AuthPhase::SignedOut;
        inner.workspace_key = None;
        drop(inner);
        self.render();
    }
}

const LINK_OFF: &str = "The owner turned this link off or replaced it. Ask them for a new one.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_fragments_need_both_a_token_and_a_key() {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let token = engine.encode([1_u8; 32]);
        let key = engine.encode([2_u8; 32]);
        let parsed = parse_view_fragment(&format!("#view={token}&key={key}")).unwrap();
        assert_eq!(parsed.0, token);
        assert_eq!(parsed.1, [2_u8; 32]);
        assert!(parse_view_fragment(&format!("#view={token}")).is_none());
        assert!(parse_view_fragment(&format!("#key={key}")).is_none());
        assert!(parse_view_fragment(&format!("#view=short&key={key}")).is_none());
        assert!(parse_view_fragment("#owner-setup=abc").is_none());
    }

    #[test]
    fn access_follows_link_and_members() {
        let mut workspace: WorkspaceResponse = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::nil(),
            "encrypted_metadata_b64": "",
            "metadata_nonce_b64": "",
            "metadata_version": 1,
            "owner_id": uuid::Uuid::nil(),
            "created_at": "2026-10-14T00:00:00Z",
            "updated_at": "2026-10-14T00:00:00Z",
            "member_count": 1
        }))
        .unwrap();
        assert_eq!(access_of(&workspace), VaultAccess::Private);
        assert_eq!(access_summary(&workspace), "Private");
        workspace.member_count = 3;
        assert_eq!(access_of(&workspace), VaultAccess::Shared);
        assert_eq!(access_summary(&workspace), "Shared · 3 people");
        workspace.public_link = true;
        assert_eq!(access_of(&workspace), VaultAccess::Public);
        assert_eq!(access_summary(&workspace), "Public link · 3 people");
    }
}
