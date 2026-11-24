use base64::Engine;
use inkstone_core::protocol::{
    AuthUserResponse, EncryptedGuestName, GuestJoinRequest, GuestJoinResponse,
    PublicLinkAccessRequest, PublicLinkRequest, PublicLinkResponse, UpdateWorkspaceRequest,
    WorkspaceKeyEnvelopeResponse, WorkspaceMemberResponse, WorkspaceResponse,
};
use leptos::prelude::{GetUntracked, Set, Update};
use wasm_bindgen::{JsCast, JsValue};

use crate::{
    AuthPhase, Driver, DriverInner, WorkspaceMetadata, api, crypto, decrypt_workspace_metadata,
    workspace_key_metadata,
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

    pub(crate) fn key(self) -> &'static str {
        match self {
            VaultAccess::Private => "private",
            VaultAccess::Shared => "shared",
            VaultAccess::Public => "public",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|access| access.key() == key)
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            VaultAccess::Private => "lock",
            VaultAccess::Shared => "users",
            VaultAccess::Public => "globe",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            VaultAccess::Private => "Private",
            VaultAccess::Shared => "Shared",
            VaultAccess::Public => "Public link",
        }
    }

    pub(crate) fn choice_text(self) -> &'static str {
        match self {
            VaultAccess::Private => "Only you can open it.",
            VaultAccess::Shared => "People you invite. You'll pick them next.",
            VaultAccess::Public => {
                "Anyone with the link can read, or edit as a guest if you allow it. No account needed."
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
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
pub(crate) const MAX_GUEST_NAME_CHARS: usize = 60;

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

pub(crate) fn new_vault_view(ui: crate::ui::Ui) -> impl leptos::IntoView {
    use leptos::prelude::*;
    let open = Memo::new(move |_| ui.new_vault.with(Option::is_some));
    let draft = Memo::new(move |_| ui.new_vault.get());
    let access = move || draft.with(|draft| draft.as_ref().map(|draft| draft.access));
    let busy = move || draft.with(|draft| draft.as_ref().is_some_and(|draft| draft.busy));
    move || {
        open.get().then(|| {
            let name = draft.with_untracked(|draft| draft.as_ref().map(|draft| draft.name.clone()));
            let choices = VaultAccess::ALL.map(|choice| {
                view! {
                    <button
                        type="button"
                        class="vault-access-choice"
                        role="radio"
                        aria-checked=move || crate::ui::bool_str(access() == Some(choice))
                        data-vault-action="access"
                        data-access=choice.key()
                        data-focus-key=format!("vault-access-{}", choice.key())
                    >
                        {crate::icons::svg(choice.icon(), "icon")}
                        <span class="vault-access-text">
                            <strong>{choice.title()}</strong>
                            <span>{choice.choice_text()}</span>
                        </span>
                    </button>
                }
            });
            view! {
                <div class="settings-backdrop new-vault-backdrop">
                    <section class="new-vault-dialog" role="dialog" aria-modal="true" aria-labelledby="new-vault-title">
                        <h2 id="new-vault-title">"New vault"</h2>
                        <p class="new-vault-intro">
                            "A vault is a separate space with its own notes, folders and people. Everything in it is end-to-end encrypted."
                        </p>
                        <label class="new-vault-label" for="new-vault-name">"Name"</label>
                        <input
                            id="new-vault-name"
                            class="new-vault-name"
                            maxlength="200"
                            autocomplete="off"
                            placeholder="Research, Journal, Team wiki…"
                            data-focus-key="new-vault-name"
                            prop:value=name.unwrap_or_default()
                        />
                        <p id="new-vault-access-label" class="new-vault-label">"Who can open it"</p>
                        <div class="vault-access-choices" role="radiogroup" aria-labelledby="new-vault-access-label">
                            {choices.into_iter().collect_view()}
                        </div>
                        {move || draft.with(|draft| draft.as_ref().and_then(|draft| draft.error.clone())).map(|error| view! {
                            <p class="settings-error" role="alert">{error}</p>
                        })}
                        <div class="new-vault-actions">
                            <button type="button" class="text-btn" data-vault-action="close-new">"Cancel"</button>
                            <button type="button" class="primary-btn new-vault-create" data-vault-action="create" disabled=busy>
                                {move || if busy() { "Creating…" } else { "Create vault" }}
                            </button>
                        </div>
                    </section>
                </div>
            }
        })
    }
}

impl Driver {
    pub(crate) fn open_new_vault(&self) {
        let mut inner = self.0.borrow_mut();
        if inner.local_mode || inner.public_view.is_some() {
            return;
        }
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
        let menu = crate::ui::state().menu_open;
        let control = target.closest("[data-vault-action]").ok().flatten();
        if control.is_none() && target.closest(".vault-menu").ok().flatten().is_none() {
            menu.set(false);
        }
        let Some(control) = control else {
            return false;
        };
        let action = control
            .get_attribute("data-vault-action")
            .unwrap_or_default();
        menu.update(|open| *open = action == "menu" && !*open);
        match action.as_str() {
            "menu" => {}
            "switch" => {
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
        let menu = crate::ui::state().menu_open;
        if !menu.get_untracked() {
            return false;
        }
        menu.set(false);
        crate::ui::focus_soon("vault-switcher");
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
