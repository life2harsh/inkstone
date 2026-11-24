#[derive(Clone)]
pub struct PendingSelection {
    pub path: String,
    pub doc_id: DocId,
    pub revision: u64,
}

#[derive(Clone)]
pub struct MissingNoteRequest {
    pub raw_target: String,
    pub proposed_path: String,
    pub source_path: String,
    pub resolution: knowledge::WikilinkResolution,
}

mod api;
mod auth;
mod crypto;
pub mod explorer;
mod home;
mod icons;
pub mod indexed_db;
pub mod ink_runtime;
pub mod knowledge;
mod live;
pub mod local_graph;
pub mod markdown_render;
pub mod math_render;
mod page_commands;
mod page_drag;
mod page_driver;
mod page_editing;
mod page_export;
mod page_files;
mod page_history;
pub mod page_surface;
mod page_touch;
mod pdf_pages;
mod prefs;
pub mod search;
mod settings;
pub mod storage;
mod theme;
mod ui;
use leptos::prelude::{GetUntracked, Set, Update};
mod vaults;
pub mod workspace_db;

use base64::Engine;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use inkstone_client::assets::{
    AssetMetadata, EncryptedAsset, EncryptedAssetChunk, decrypt_asset, encrypt_asset,
};
use inkstone_client::text_sync::{TextClientEffect, TextClientEvent, TextClientState};
use inkstone_client::{ClientEvent, ClientState, Effect, SessionPhase};
use inkstone_core::doc_ops::DocumentOp;
use inkstone_core::document::{BlockContent, InkstoneDocument};
use inkstone_core::ids::{BlockId, DocId, UpdateId};
use inkstone_core::protocol::{
    ApproveInvitationKeyRequest, AssetChunkResponse, AssetResponse, AuthUserResponse,
    ChangePasswordRequest, ClientWsMessage, ConsumeEmailVerificationRequest,
    ConsumePasswordResetRequest, CreateAssetRequest, CreateDocRequest, CreateInvitationRequest,
    CreateInvitationResponse, CreateWorkspaceRequest, DeviceResponse, DocResponse,
    EncryptedUpdateResponse, GenericMessageResponse, InvitationKeyRequestResponse,
    InvitationRegisterRequest, InvitationResponse, LoginRequest, OutgoingInvitationResponse,
    OwnerSetupRequest, PaginatedResponse, PathProjectionResponse, PostSnapshotRequest,
    PublicLinkResponse, PutAssetChunkRequest, PutPathProjectionRequest,
    PutWorkspaceKeyEnvelopeRequest, RecoveryEnvelopeRequest, RecoveryEnvelopeResponse,
    RegisterRequest, ServerWsMessage, SubmitInvitationKeyRequest, TokenRequest,
    TransferOwnershipRequest, TreeAssetMetadataUpdate, TreeDocPathUpdate, TreePathProjectionUpdate,
    UpdateAssetMetadataRequest, UpdateDocPathProjectionRequest, UpdateDocTitleProjectionRequest,
    UpdateMemberRoleRequest, UpdateWorkspaceRequest, WorkspaceKeyEnvelopeResponse,
    WorkspaceMemberResponse, WorkspaceResponse, WorkspaceTreeMutationRequest,
};
use inkstone_core::{
    DocumentUpdate, INK_PRESENCE_VERSION, InkPresencePayload, RelativeSelection, TextEdit,
    TextReplicaId, TextTarget,
};
use inkstone_workspace::{ExportAsset, ExportDocument, VaultExport};
use inkstone_workspace::{WorkspaceEntry, WorkspaceEntryKind, WorkspacePath, WorkspaceStore};

use indexed_db::IndexedDb;
use workspace_db::WorkspaceDb;

/// Scroll containers whose position survives a full render.
const RETAINED_SCROLLERS: [&str; 2] = [".main-content", ".inspector"];

thread_local! {
    // Replacing the DOM blurs active editors. Ignore that synthetic focusout while
    // render holds the driver borrow; user-initiated focus changes still render.
    static RENDERING: Cell<bool> = const { Cell::new(false) };
    static ASSET_URLS: RefCell<HashMap<String, AssetUrl>> = RefCell::new(HashMap::new());
    static PAGE_HEIGHT_HOLD: Cell<u32> = const { Cell::new(0) };
}

const PAGE_HEIGHT_HOLD_MS: i32 = 2500;

fn hold_page_height(document: &web_sys::Document, key: Option<String>, height: f64) {
    let same_note = document
        .query_selector(".main-content")
        .ok()
        .flatten()
        .is_some_and(|scroller| scroller.get_attribute("data-scroll-key") == key);
    let Some(surface) = document
        .query_selector(".page-surface")
        .ok()
        .flatten()
        .and_then(|surface| surface.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    if !same_note || height <= 0.0 {
        return;
    }
    let _ = surface
        .style()
        .set_property("min-height", &format!("{}px", height.ceil()));
    let generation = PAGE_HEIGHT_HOLD.with(|hold| {
        hold.set(hold.get().wrapping_add(1));
        hold.get()
    });
    let release = Closure::once_into_js(move || {
        if PAGE_HEIGHT_HOLD.with(Cell::get) != generation {
            return;
        }
        if let Some(surface) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(".page-surface").ok().flatten())
            .and_then(|surface| surface.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let _ = surface.style().remove_property("min-height");
        }
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            release.unchecked_ref(),
            PAGE_HEIGHT_HOLD_MS,
        );
    }
}

struct AssetUrl {
    url: String,
    fingerprint: u64,
    checked_at_ms: f64,
    checking: bool,
}

const ASSET_RECHECK_MS: f64 = 5_000.0;

fn asset_fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64 ^ bytes.len() as u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn set_asset_src(document: &web_sys::Document, path: &str, url: &str) {
    let Ok(nodes) = document.query_selector_all("img[data-asset-path]") else {
        return;
    };
    for index in 0..nodes.length() {
        let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        if element.get_attribute("data-asset-path").as_deref() == Some(path)
            && element.get_attribute("src").as_deref() != Some(url)
        {
            let _ = element.set_attribute("src", url);
        }
    }
}

const DEVICE_ID_KEY: &str = "\0device_id";
const ACCOUNT_VAULT_KEY: &str = "\0account_vault_key";
const DEVICE_PRIVATE_KEY: &str = "\0device_private_key";
const REMEMBERED_VAULT_KEY: &str = "\0remembered_vault_key";
const AWAITING_INVITATION_KEY: &str = "\0awaiting_invitation_workspace";
const SELECTED_WORKSPACE_KEY: &str = "\0selected_workspace";

fn asset_id_metadata(path: &str) -> String {
    format!("\0asset_id:{path}")
}

fn asset_version_metadata(path: &str) -> String {
    format!("\0asset_metadata_version:{path}")
}

fn asset_fetched_metadata(asset_id: uuid::Uuid) -> String {
    format!("\0asset_fetched:{asset_id}")
}

fn folder_id_metadata(path: &str) -> String {
    format!("\0folder_id:{path}")
}

fn folder_version_metadata(path: &str) -> String {
    format!("\0folder_projection_version:{path}")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspaceMetadata {
    name: String,
    description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_link: Option<String>,
}

fn decrypt_workspace_metadata(
    workspace: &WorkspaceResponse,
    key: &[u8; 32],
) -> Result<WorkspaceMetadata, String> {
    serde_json::from_slice(&crypto::decrypt(
        key,
        &workspace.encrypted_metadata_b64,
        &workspace.metadata_nonce_b64,
        &crypto::workspace_metadata_aad(workspace.id),
    )?)
    .map_err(|_| "Invalid encrypted workspace metadata".into())
}

fn revoke_asset_preview(url: &mut Option<String>) {
    if let Some(url) = url.take() {
        let _ = web_sys::Url::revoke_object_url(&url);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthPhase {
    Checking,
    SignedOut,
    UnlockRequired,
    AwaitingKey,
    Ready,
}

fn invitation_context_matches(
    auth_phase: AuthPhase,
    active_workspace_id: Option<uuid::Uuid>,
    active_workspace_key: Option<[u8; 32]>,
    active_generation: u64,
    active_user_id: Option<uuid::Uuid>,
    expected_workspace_id: uuid::Uuid,
    expected_workspace_key: [u8; 32],
    expected_generation: u64,
    expected_user_id: uuid::Uuid,
) -> bool {
    auth_phase == AuthPhase::Ready
        && active_workspace_id == Some(expected_workspace_id)
        && active_workspace_key == Some(expected_workspace_key)
        && active_generation == expected_generation
        && active_user_id == Some(expected_user_id)
}

#[derive(Debug, Clone)]
struct InviteFragment {
    token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AccountTokenFlow {
    VerifyEmail(String),
    PasswordReset(String),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LocalPathProjection {
    path: String,
    encrypted: crypto::Ciphertext,
    pending: bool,
    #[serde(default)]
    projection_version: i64,
}

struct SocketBinding {
    socket: web_sys::WebSocket,
    _on_open: Closure<dyn FnMut(web_sys::Event)>,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
    _on_close: Closure<dyn FnMut(web_sys::CloseEvent)>,
}

impl SocketBinding {
    fn detach(self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close();
    }
}

#[derive(Default)]
struct RuntimeHandles {
    socket: Option<SocketBinding>,
    reconnect_timeout: Option<(i32, Closure<dyn FnMut()>)>,
    discovery_interval: Option<(i32, Closure<dyn FnMut()>)>,
    presence_interval: Option<(i32, Closure<dyn FnMut()>)>,
    presence_timeout: Option<(i32, Closure<dyn FnMut()>)>,
    live: live::LiveMesh,
}

const PRESENCE_VERSION: u32 = 1;
const PRESENCE_TTL_MS: f64 = 25_000.0;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PresenceKind {
    Text {
        selection: RelativeSelection,
    },
    Ink {
        payload: InkPresencePayload,
    },
    Idle,
    /// Live mode handshake; never shown, only routed.
    Signal {
        to: Option<uuid::Uuid>,
        signal: live::LiveSignal,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PresencePayload {
    version: u32,
    sent_at_ms: i64,
    state: PresenceKind,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EncryptedPresenceWire {
    doc_id: uuid::Uuid,
    sender_device_id: uuid::Uuid,
    encrypted_presence_b64: String,
    nonce_b64: String,
    aad_version: i32,
    counter: u64,
}

#[derive(Debug, Clone)]
struct RemotePresence {
    payload: PresencePayload,
    counter: u64,
    received_at_ms: f64,
}

fn accept_presence_counter(previous: Option<u64>, next: u64) -> bool {
    next > previous.unwrap_or(0)
}

fn presence_is_live(received_at_ms: f64, now_ms: f64) -> bool {
    now_ms >= received_at_ms && now_ms - received_at_ms <= PRESENCE_TTL_MS
}

fn presence_sender_allowed(sender: uuid::Uuid, own: uuid::Uuid) -> bool {
    sender != own
}

fn target_text(document: &InkstoneDocument, target: TextTarget) -> Option<String> {
    match target {
        TextTarget::Title => Some(document.title.clone()),
        TextTarget::Markdown(block_id) => {
            match document.get_block(block_id).map(|block| &block.content) {
                Some(BlockContent::Markdown { text }) => Some(text.clone()),
                _ => None,
            }
        }
        TextTarget::Math(block_id) => {
            match document.get_block(block_id).map(|block| &block.content) {
                Some(BlockContent::Math { source }) => Some(source.clone()),
                _ => None,
            }
        }
    }
}

fn note_name_has_extension(name: &str) -> bool {
    name.rsplit('/')
        .next()
        .and_then(|file| file.rsplit_once('.'))
        .is_some_and(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
}

fn graph_node_radius(kind: local_graph::GraphNodeKind) -> f32 {
    if kind == local_graph::GraphNodeKind::Active {
        22.0
    } else {
        11.0
    }
}

fn graph_label(label: &str) -> String {
    if label.chars().count() <= 20 {
        label.to_string()
    } else {
        let mut short: String = label.chars().take(19).collect();
        short.push('…');
        short
    }
}

fn workspace_capabilities(local_mode: bool, is_owner: bool, role: Option<&str>) -> (bool, bool) {
    let can_read = local_mode
        || is_owner
        || role.is_some_and(|role| matches!(role, "owner" | "admin" | "editor" | "viewer"));
    let can_edit = local_mode
        || is_owner
        || role.is_some_and(|role| matches!(role, "owner" | "admin" | "editor"));
    (can_read, can_edit)
}

fn can_read_workspace(inner: &DriverInner) -> bool {
    let user_id = inner.auth_user.as_ref().map(|user| user.id);
    let is_owner = user_id.is_some_and(|user_id| {
        inner.workspaces.iter().any(|workspace| {
            Some(workspace.id) == inner.workspace_id && workspace.owner_id == user_id
        })
    });
    let role = inner
        .members
        .iter()
        .find(|member| Some(member.user_id) == user_id)
        .map(|member| member.role.as_str());
    workspace_capabilities(inner.local_mode, is_owner, role).0
}

fn viewport_width() -> f64 {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .unwrap_or(0.0)
}

fn can_edit_workspace(inner: &DriverInner) -> bool {
    let user_id = inner.auth_user.as_ref().map(|user| user.id);
    let is_owner = user_id.is_some_and(|user_id| {
        inner.workspaces.iter().any(|workspace| {
            Some(workspace.id) == inner.workspace_id && workspace.owner_id == user_id
        })
    });
    let role = inner
        .members
        .iter()
        .find(|member| Some(member.user_id) == user_id)
        .map(|member| member.role.as_str());
    workspace_capabilities(inner.local_mode, is_owner, role).1
}

/// Whether the open note has loaded, so edits reach the right document.
fn document_ready(inner: &DriverInner) -> bool {
    !matches!(
        inner.client_state.phase(),
        SessionPhase::Closed | SessionPhase::HydratingLocal
    )
}

fn client_event_allowed(inner: &DriverInner, event: &ClientEvent) -> bool {
    !matches!(event, ClientEvent::LocalIntent { .. }) || can_edit_workspace(inner)
}

fn document_storage_key(workspace_id: Option<uuid::Uuid>, doc_id: DocId) -> String {
    workspace_id
        .map(|workspace_id| format!("{workspace_id}/{doc_id}"))
        .unwrap_or_else(|| format!("local/{doc_id}"))
}

fn local_delete_is_safe(local_mode: bool, remote_delete_succeeded: bool) -> bool {
    local_mode || remote_delete_succeeded
}

fn parse_awaiting_workspace(value: Option<String>) -> Option<uuid::Uuid> {
    value.and_then(|value| uuid::Uuid::parse_str(&value).ok())
}

fn apply_device_revocation(devices: &mut Vec<DeviceResponse>, revoked_id: uuid::Uuid) {
    devices.retain(|device| device.id != revoked_id);
}

fn clear_expired_secrets(
    auth_user: &mut Option<AuthUserResponse>,
    auth_phase: &mut AuthPhase,
    account_vault_key: &mut Option<[u8; 32]>,
    workspace_key: &mut Option<[u8; 32]>,
) {
    *auth_user = None;
    *auth_phase = AuthPhase::SignedOut;
    *account_vault_key = None;
    *workspace_key = None;
}

fn expire_auth_context(inner: &mut DriverInner) {
    clear_expired_secrets(
        &mut inner.auth_user,
        &mut inner.auth_phase,
        &mut inner.account_vault_key,
        &mut inner.workspace_key,
    );
    inner.members.clear();
    inner.outgoing_invitations.clear();
    inner.key_requests.clear();
    inner.devices.clear();
    inner.auth_error = Some("You've been signed out. Sign in again; your changes are safe.".into());
    inner.invitation_generation += 1;
    inner.advance_workspace_revision();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentMode {
    Edit,
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InkSavePhase {
    Changed,
    Saving,
    Saved,
    Failed,
}

impl InkSavePhase {
    fn label(self) -> &'static str {
        match self {
            Self::Changed => "Drawing changed",
            Self::Saving => "Saving...",
            Self::Saved => "Saved locally",
            Self::Failed => "Save failed",
        }
    }

    fn state(self) -> &'static str {
        match self {
            Self::Changed => "changed",
            Self::Saving => "saving",
            Self::Saved => "saved",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct InkSaveState {
    phase: InkSavePhase,
    generation: u64,
    update_id: Option<UpdateId>,
}

#[derive(Debug, Clone, Default)]
struct InkSaveTracker {
    states: HashMap<(DocId, BlockId), InkSaveState>,
}

impl InkSaveTracker {
    fn changed(&mut self, doc_id: DocId, block_id: BlockId) -> InkSavePhase {
        let state = self
            .states
            .entry((doc_id, block_id))
            .or_insert(InkSaveState {
                phase: InkSavePhase::Saved,
                generation: 0,
                update_id: None,
            });
        state.generation += 1;
        state.phase = InkSavePhase::Changed;
        state.update_id = None;
        state.phase
    }

    fn queued(&mut self, doc_id: DocId, block_id: BlockId, update_id: UpdateId) {
        let state = self
            .states
            .entry((doc_id, block_id))
            .or_insert(InkSaveState {
                phase: InkSavePhase::Changed,
                generation: 1,
                update_id: None,
            });
        if state.phase != InkSavePhase::Changed {
            state.generation += 1;
            state.phase = InkSavePhase::Changed;
        }
        state.update_id = Some(update_id);
    }

    fn saving(
        &mut self,
        doc_id: DocId,
        block_id: BlockId,
        update_id: UpdateId,
    ) -> Option<InkSavePhase> {
        let state = self
            .states
            .entry((doc_id, block_id))
            .or_insert(InkSaveState {
                phase: InkSavePhase::Changed,
                generation: 1,
                update_id: Some(update_id),
            });
        if state.update_id != Some(update_id) {
            return None;
        }
        state.phase = InkSavePhase::Saving;
        Some(state.phase)
    }

    fn finish(
        &mut self,
        doc_id: DocId,
        block_id: BlockId,
        update_id: UpdateId,
        phase: InkSavePhase,
    ) -> Option<InkSavePhase> {
        let state = self.states.get_mut(&(doc_id, block_id))?;
        if state.update_id != Some(update_id) {
            return None;
        }
        state.phase = phase;
        if phase == InkSavePhase::Saved {
            state.update_id = None;
        }
        Some(state.phase)
    }

    #[cfg(test)]
    fn phase(&self, doc_id: DocId, block_id: BlockId) -> InkSavePhase {
        self.states
            .get(&(doc_id, block_id))
            .map(|state| state.phase)
            .unwrap_or(InkSavePhase::Saved)
    }

    fn failed(&mut self, doc_id: DocId, block_id: BlockId) -> InkSavePhase {
        let state = self
            .states
            .entry((doc_id, block_id))
            .or_insert(InkSaveState {
                phase: InkSavePhase::Failed,
                generation: 1,
                update_id: None,
            });
        state.phase = InkSavePhase::Failed;
        state.update_id = None;
        state.phase
    }
}

#[derive(Clone)]
pub struct DriverInner {
    pub client_state: ClientState,
    text_client_state: Option<TextClientState>,
    pub db: Option<IndexedDb>,
    pub workspace_db: Option<WorkspaceDb>,
    pub persistence_status: String,
    pub storage_error: Option<String>,
    pub document_error: Option<String>,
    pub local_persistence_error: Option<String>,
    pub selected_doc_id: DocId,
    pub entries: Vec<WorkspaceEntry>,
    pub selected_path: Option<String>,
    pub workspace_loaded: bool,
    pub document_mode: DocumentMode,
    pub link_message: Option<String>,
    pub search_index_revision: u64,
    pub index_error: Option<String>,
    pub search_documents: Vec<search::SearchDocument>,
    note_summaries: HashMap<String, home::NoteSummary>,
    pub recent_notes: Vec<String>,
    pub knowledge: knowledge::KnowledgeIndex,
    pub inspector_open: bool,
    pub selection_revision: u64,
    pub pending_selection: Option<PendingSelection>,
    pub recent_notes_revision: u64,
    pub missing_note: Option<MissingNoteRequest>,
    pub ink_tool: String,
    pub ink_brush_width: f32,
    pub ink_brush_color: (u8, u8, u8),
    pub ink_panel_open: bool,
    pub expanded_ink_block: Option<BlockId>,
    /// Markdown or Math block currently shown as source on the page surface.
    pub editing_block: Option<BlockId>,
    /// Mouse and touch ink when true; otherwise only a pen inks.
    pub ink_mode: bool,
    pending_block_caret: Option<(BlockId, u32)>,
    /// The `/` command menu, while open.
    slash_menu: Option<page_commands::SlashMenu>,
    /// A `/` the user closed the menu for, so it stays closed.
    slash_dismissed: Option<(BlockId, u32)>,
    /// A text save held back until structural saves ahead of it finish, so
    /// text is never stored against blocks that are not stored yet.
    deferred_text_persist: Option<DocumentUpdate>,
    quiet_dispatch: bool,
    /// A block being dragged by its grip.
    block_drag: Option<page_drag::BlockDrag>,
    page_history: page_history::PageHistory,
    migrated_docs: HashSet<DocId>,
    pub sidebar_open: bool,
    pub settings_open: bool,
    pub settings_category: String,
    pub deferred_navigation: Option<String>,
    pub pending_focus_key: Option<String>,
    local_mode: bool,
    auth_phase: AuthPhase,
    auth_user: Option<AuthUserResponse>,
    auth_error: Option<String>,
    auth_notice: Option<String>,
    account_token_flow: Option<AccountTokenFlow>,
    account_vault_key: Option<[u8; 32]>,
    recovery_key_display: Option<String>,
    new_account_setup_pending: bool,
    owner_setup_token: Option<String>,
    link_email: Option<String>,
    pending_password: Option<String>,
    owner_setup_in_flight: bool,
    sync_error: Option<String>,
    device_id: Option<uuid::Uuid>,
    workspaces: Vec<WorkspaceResponse>,
    workspace_id: Option<uuid::Uuid>,
    workspace_name: String,
    workspace_description: String,
    workspace_public_link: Option<String>,
    vault_names: HashMap<uuid::Uuid, String>,
    new_vault: Option<vaults::NewVaultDraft>,
    public_view: Option<String>,
    public_view_error: Option<String>,
    guest_join: Option<vaults::GuestJoin>,
    guest_name: Option<String>,
    public_link_busy: bool,
    public_link_status: Option<String>,
    asset_preview_url: Option<String>,
    asset_preview_type: String,
    reserved_asset_paths: HashSet<String>,
    workspace_key: Option<[u8; 32]>,
    members: Vec<WorkspaceMemberResponse>,
    outgoing_invitations: Vec<OutgoingInvitationResponse>,
    key_requests: Vec<InvitationKeyRequestResponse>,
    devices: Vec<DeviceResponse>,
    invite_email: String,
    invite_role: String,
    invite_expiry_hours: i64,
    invitation_in_flight: bool,
    invite_url: Option<String>,
    invite_copy_status: Option<String>,
    pending_invite: Option<InviteFragment>,
    needs_key_envelope: bool,
    remote_docs: HashSet<DocId>,
    active_workspace_ownership: HashMap<String, Option<DocId>>,
    workspace_revision: u64,
    invitation_generation: u64,
    discovery_in_flight: bool,
    locally_new_docs: HashSet<DocId>,
    ink_save_tracker: InkSaveTracker,
    composing_text_target: Option<TextTarget>,
    pending_relative_selection: Option<(RelativeSelection, String, i32, i32)>,
    remote_selection_queue: VecDeque<Option<(RelativeSelection, String, i32, i32)>>,
    presence_counter: u64,
    pending_presence: Option<PresencePayload>,
    last_presence: Option<PresencePayload>,
    remote_presence: HashMap<uuid::Uuid, RemotePresence>,
}

impl DriverInner {
    fn advance_workspace_revision(&mut self) -> u64 {
        advance_workspace_revision(&mut self.workspace_revision, &mut self.discovery_in_flight);
        self.workspace_revision
    }
}

fn advance_workspace_revision(revision: &mut u64, discovery_in_flight: &mut bool) {
    *revision += 1;
    *discovery_in_flight = false;
}

fn minimal_text_edit(before: &str, after: &str) -> Option<TextEdit> {
    if before == after {
        return None;
    }
    let mut prefix_bytes = 0;
    let mut prefix_utf16 = 0_u32;
    for (left, right) in before.chars().zip(after.chars()) {
        if left != right {
            break;
        }
        prefix_bytes += left.len_utf8();
        prefix_utf16 += left.len_utf16() as u32;
    }

    let before_tail = &before[prefix_bytes..];
    let after_tail = &after[prefix_bytes..];
    let mut suffix_after_bytes = 0;
    let mut suffix_utf16 = 0_u32;
    for (left, right) in before_tail.chars().rev().zip(after_tail.chars().rev()) {
        if left != right {
            break;
        }
        suffix_after_bytes += right.len_utf8();
        suffix_utf16 += left.len_utf16() as u32;
    }
    let before_utf16 = before.encode_utf16().count() as u32;
    Some(TextEdit {
        start_utf16: prefix_utf16,
        end_utf16: before_utf16 - suffix_utf16,
        replacement: after[prefix_bytes..after.len() - suffix_after_bytes].to_string(),
    })
}

fn text_replica_id(device_id: Option<uuid::Uuid>, doc_id: DocId) -> TextReplicaId {
    let device = device_id.unwrap_or(doc_id.0).as_u128();
    let mixed = device ^ doc_id.0.as_u128().rotate_left(53);
    let value = ((mixed as u64) & ((1_u64 << 53) - 1)).max(1);
    TextReplicaId::new(value).expect("masked browser replica id is valid")
}

fn ink_actor_id(device_id: Option<uuid::Uuid>, doc_id: DocId, block_id: BlockId) -> [u8; 16] {
    let device = device_id.unwrap_or(doc_id.0).as_u128();
    (device ^ doc_id.0.as_u128().rotate_left(37) ^ block_id.0.as_u128().rotate_left(79))
        .to_be_bytes()
}

#[derive(Clone)]
pub struct Driver(
    Rc<RefCell<DriverInner>>,
    Rc<RefCell<ink_runtime::InkRuntimeRegistry>>,
    Rc<RefCell<RuntimeHandles>>,
);

impl Driver {
    pub fn new(doc_id: DocId) -> Self {
        let saved_brush = page_driver::load_brush();
        Driver(
            Rc::new(RefCell::new(DriverInner {
                client_state: ClientState::new(doc_id),
                text_client_state: None,
                db: None,
                workspace_db: None,
                persistence_status: "Initializing...".to_string(),
                storage_error: None,
                document_error: None,
                local_persistence_error: None,
                selected_doc_id: doc_id,
                entries: Vec::new(),
                selected_path: None,
                workspace_loaded: false,
                document_mode: DocumentMode::Edit,
                link_message: None,
                search_index_revision: 0,
                index_error: None,
                search_documents: Vec::new(),
                note_summaries: HashMap::new(),
                recent_notes: Vec::new(),
                knowledge: knowledge::KnowledgeIndex::new(),
                inspector_open: false,
                selection_revision: 0,
                pending_selection: None,
                recent_notes_revision: 0,
                missing_note: None,
                ink_tool: saved_brush.0,
                ink_brush_width: saved_brush.1,
                ink_brush_color: saved_brush.2,
                ink_panel_open: false,
                expanded_ink_block: None,
                editing_block: None,
                ink_mode: false,
                pending_block_caret: None,
                slash_menu: None,
                slash_dismissed: None,
                deferred_text_persist: None,
                quiet_dispatch: false,
                block_drag: None,
                page_history: Default::default(),
                migrated_docs: HashSet::new(),
                sidebar_open: false,
                settings_open: false,
                settings_category: "General".to_string(),
                deferred_navigation: None,
                pending_focus_key: None,
                local_mode: false,
                auth_phase: AuthPhase::Checking,
                auth_user: None,
                auth_error: None,
                auth_notice: None,
                link_email: fragment_email_from_window(),
                pending_password: None,
                account_token_flow: parse_account_token_fragment_from_window(),
                account_vault_key: None,
                recovery_key_display: None,
                new_account_setup_pending: false,
                owner_setup_token: parse_owner_setup_fragment_from_window(),
                owner_setup_in_flight: false,
                sync_error: None,
                device_id: None,
                workspaces: Vec::new(),
                workspace_id: None,
                workspace_name: "Local knowledge workspace".to_string(),
                workspace_description: String::new(),
                workspace_public_link: None,
                vault_names: HashMap::new(),
                new_vault: None,
                public_view: None,
                public_view_error: None,
                guest_join: None,
                guest_name: None,
                public_link_busy: false,
                public_link_status: None,
                asset_preview_url: None,
                asset_preview_type: String::new(),
                reserved_asset_paths: HashSet::new(),
                workspace_key: None,
                members: Vec::new(),
                outgoing_invitations: Vec::new(),
                key_requests: Vec::new(),
                devices: Vec::new(),
                invite_email: String::new(),
                invite_role: "editor".to_string(),
                invite_expiry_hours: 168,
                invitation_in_flight: false,
                invite_url: None,
                invite_copy_status: None,
                pending_invite: parse_invite_fragment(),
                needs_key_envelope: false,
                remote_docs: HashSet::new(),
                active_workspace_ownership: HashMap::new(),
                workspace_revision: 0,
                invitation_generation: 0,
                discovery_in_flight: false,
                locally_new_docs: HashSet::new(),
                ink_save_tracker: InkSaveTracker::default(),
                composing_text_target: None,
                pending_relative_selection: None,
                remote_selection_queue: VecDeque::new(),
                presence_counter: 0,
                pending_presence: None,
                last_presence: None,
                remote_presence: HashMap::new(),
            })),
            Rc::new(RefCell::new(ink_runtime::InkRuntimeRegistry::default())),
            Rc::new(RefCell::new(RuntimeHandles::default())),
        )
    }

    fn set_ink_save_state(&self, block_id: BlockId, text: &str, state: &str) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let selector = format!(".ink-save-state[data-block-id=\"{}\"]", block_id);
        if let Ok(Some(element)) = document.query_selector(&selector) {
            element.set_text_content(Some(text));
            let _ = element.set_attribute("data-state", state);
        }
    }

    fn update_ink_save_phase(&self, block_id: BlockId, phase: InkSavePhase) {
        self.set_ink_save_state(block_id, phase.label(), phase.state());
    }

    fn mark_ink_changed(&self, block_id: BlockId) {
        let phase = {
            let mut inner = self.0.borrow_mut();
            if inner.document_mode != DocumentMode::Edit {
                return;
            }
            let doc_id = inner.selected_doc_id;
            inner.ink_save_tracker.changed(doc_id, block_id)
        };
        self.update_ink_save_phase(block_id, phase);
    }

    fn mark_ink_failed(&self, block_id: BlockId) {
        let phase = {
            let mut inner = self.0.borrow_mut();
            let doc_id = inner.selected_doc_id;
            inner.ink_save_tracker.failed(doc_id, block_id)
        };
        self.update_ink_save_phase(block_id, phase);
    }

    fn finish_ink_update(
        &self,
        doc_id: DocId,
        block_id: Option<BlockId>,
        update_id: UpdateId,
        phase: InkSavePhase,
    ) {
        let Some(block_id) = block_id else {
            return;
        };
        let visible_phase = {
            self.0
                .borrow_mut()
                .ink_save_tracker
                .finish(doc_id, block_id, update_id, phase)
        };
        if let Some(visible_phase) = visible_phase {
            self.update_ink_save_phase(block_id, visible_phase);
        }
    }

    fn handle_local_persistence_failure(
        &self,
        doc_id: DocId,
        block_id: Option<BlockId>,
        update_id: UpdateId,
        message: String,
    ) {
        {
            let mut inner = self.0.borrow_mut();
            inner.persistence_status = "Save failed".to_string();
            inner.local_persistence_error = Some(message);
        }
        self.finish_ink_update(doc_id, block_id, update_id, InkSavePhase::Failed);
        self.dispatch(ClientEvent::LocalPersistenceFailed { update_id });
        self.render();
    }

    fn retry_local_persistence(&self) {
        let has_failed_update = {
            let inner = self.0.borrow();
            inner.client_state.local_persistence_failed()
                || inner
                    .text_client_state
                    .as_ref()
                    .is_some_and(TextClientState::local_persistence_failed)
        };
        if !has_failed_update {
            return;
        }
        {
            let mut inner = self.0.borrow_mut();
            inner.persistence_status = "Retrying local save...".to_string();
            inner.local_persistence_error = None;
        }
        if self.0.borrow().db.is_some() {
            self.dispatch(ClientEvent::RetryLocalPersistence);
            let effects = self
                .0
                .borrow_mut()
                .text_client_state
                .as_mut()
                .map(|state| state.transition(TextClientEvent::RetryLocalPersistence))
                .unwrap_or_default();
            for effect in effects {
                self.execute_text_effect(effect);
            }
            self.render();
            return;
        }

        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match IndexedDb::open("inkstone_local_db", 1).await {
                Ok(db) => {
                    this.0.borrow_mut().db = Some(db);
                    this.dispatch(ClientEvent::RetryLocalPersistence);
                    let effects = this
                        .0
                        .borrow_mut()
                        .text_client_state
                        .as_mut()
                        .map(|state| state.transition(TextClientEvent::RetryLocalPersistence))
                        .unwrap_or_default();
                    for effect in effects {
                        this.execute_text_effect(effect);
                    }
                    this.render();
                }
                Err(error) => {
                    let mut inner = this.0.borrow_mut();
                    inner.persistence_status = "Save failed".to_string();
                    inner.local_persistence_error =
                        Some(format!("Failed to reopen IndexedDB: {:?}", error));
                    drop(inner);
                    this.render();
                }
            }
        });
    }

    fn resume_deferred_navigation(&self) {
        let path = {
            let mut inner = self.0.borrow_mut();
            if inner.client_state.has_pending_intent()
                || inner
                    .text_client_state
                    .as_ref()
                    .is_some_and(TextClientState::has_pending_persistence)
            {
                return;
            }
            inner.deferred_navigation.take()
        };
        if let Some(path) = path {
            self.select_entry(&path);
        }
    }

    fn report_ink_error(&self, context: &str, error: JsValue) {
        let message = format!("{}: {:?}", context, error);
        self.0.borrow_mut().persistence_status = message.clone();
        web_sys::console::error_1(&JsValue::from_str(&message));
    }

    fn dispatch_ink_update(&self, ink_update: ink_runtime::InkUpdate) {
        let is_current = {
            let inner = self.0.borrow();
            inner.selected_doc_id == ink_update.doc_id
                && matches!(
                    inner
                        .client_state
                        .effective_view_doc()
                        .get_block(ink_update.block_id)
                        .map(|block| &block.content),
                    Some(BlockContent::Ink { .. })
                )
        };
        if !is_current {
            return;
        }

        let update_id = UpdateId::new_v4();
        self.0.borrow_mut().ink_save_tracker.queued(
            ink_update.doc_id,
            ink_update.block_id,
            update_id,
        );
        self.dispatch(ClientEvent::LocalIntent {
            op: DocumentOp::ApplyInkUpdateV2 {
                payload: inkstone_core::CollaborativeInkUpdate {
                    version: inkstone_core::INK_UPDATE_PAYLOAD_VERSION,
                    block_id: ink_update.block_id,
                    stream_epoch: ink_update.stream_epoch,
                    update: ink_update.update,
                    bootstrap_snapshot: ink_update.bootstrap_snapshot,
                },
            },
            update_id,
            applied_at_ms: js_sys::Date::now() as i64,
        });
    }

    fn save_ink_block(&self, block_id: BlockId) {
        let result = { self.1.borrow_mut().take_local_update(block_id) };
        match result {
            Ok(Some(update)) => self.dispatch_ink_update(update),
            Ok(None) => {}
            Err(error) => {
                self.mark_ink_failed(block_id);
                self.report_ink_error("Failed to export Ink block", error);
            }
        }
    }

    fn save_all_ink_blocks(&self) {
        let result = { self.1.borrow_mut().take_all_local_updates() };
        match result {
            Ok(updates) => {
                for update in updates {
                    self.dispatch_ink_update(update);
                }
            }
            Err(error) => self.report_ink_error("Failed to export Ink blocks", error),
        }
    }

    fn queue_ink_previews(&self, block_id: Option<BlockId>, immediate: bool) {
        let result = if let Some(block_id) = block_id {
            self.1
                .borrow_mut()
                .take_local_preview(block_id)
                .map(|preview| preview.into_iter().collect())
        } else {
            self.1.borrow_mut().take_all_local_previews()
        };
        let Ok(previews) = result else {
            return;
        };
        for preview in previews {
            if preview.doc_id != self.0.borrow().selected_doc_id {
                continue;
            }
            let state = PresenceKind::Ink {
                payload: InkPresencePayload {
                    version: INK_PRESENCE_VERSION,
                    block_id: preview.block_id.0,
                    stream_epoch: preview.stream_epoch,
                    stonepen_preview: preview.preview,
                },
            };
            if immediate {
                self.live_broadcast(&state, true);
                self.0.borrow_mut().pending_presence = Some(PresencePayload {
                    version: PRESENCE_VERSION,
                    sent_at_ms: js_sys::Date::now() as i64,
                    state,
                });
                self.send_pending_presence();
            } else {
                self.queue_presence(state);
            }
        }
    }

    fn undo_ink_block(&self, block_id: BlockId) {
        let result = { self.1.borrow_mut().undo(block_id) };
        match result {
            Ok(Some(update)) => self.dispatch_ink_update(update),
            Ok(None) => {}
            Err(error) => {
                self.mark_ink_failed(block_id);
                self.report_ink_error("Failed to undo Ink block", error);
            }
        }
    }

    fn redo_ink_block(&self, block_id: BlockId) {
        let result = { self.1.borrow_mut().redo(block_id) };
        match result {
            Ok(Some(update)) => self.dispatch_ink_update(update),
            Ok(None) => {}
            Err(error) => {
                self.mark_ink_failed(block_id);
                self.report_ink_error("Failed to redo Ink block", error);
            }
        }
    }

    fn flush_ink_runtimes(&self) {
        self.flush_page_ink();
        let result = { self.1.borrow_mut().destroy_all() };
        match result {
            Ok(updates) => {
                for update in updates {
                    self.dispatch_ink_update(update);
                }
            }
            Err(error) => self.report_ink_error("Failed to flush Ink blocks", error),
        }
    }

    pub(crate) fn flush_ink_before_transition(&self) {
        let was_rendering = RENDERING.with(|rendering| {
            let was_rendering = rendering.get();
            rendering.set(true);
            was_rendering
        });
        self.flush_ink_runtimes();
        RENDERING.with(|rendering| rendering.set(was_rendering));
    }

    fn mount_ink_blocks(
        &self,
        document: &InkstoneDocument,
        tool: &str,
        brush_width: f32,
        brush_color: (u8, u8, u8),
        read_only: bool,
        device_id: Option<uuid::Uuid>,
    ) {
        let Some(dom_document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };

        for block in &document.blocks {
            let BlockContent::Ink {
                stonepen_json,
                collaboration,
            } = &block.content
            else {
                continue;
            };
            let canvas_id = format!("ink-canvas-{}", block.id);
            if dom_document.get_element_by_id(&canvas_id).is_none() {
                continue;
            }

            let result = self.1.borrow_mut().mount(
                document.id,
                block.id,
                &canvas_id,
                ink_actor_id(device_id, document.id, block.id),
                stonepen_json,
                collaboration.as_ref(),
                tool,
                brush_width,
                brush_color,
                read_only,
            );
            match result {
                Ok(Some(update)) => self.dispatch_ink_update(update),
                Ok(None) => {}
                Err(error) => self.report_ink_error("Failed to mount Ink block", error),
            }
        }
    }

    fn go_home(&self) {
        if self.0.borrow().selected_path.is_none() {
            self.0.borrow_mut().sidebar_open = false;
            self.render();
            return;
        }
        self.flush_ink_before_transition();
        if self.has_unsaved_edits() {
            let this = self.clone();
            let retry = Closure::once_into_js(move || this.go_home());
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    retry.unchecked_ref(),
                    120,
                );
            }
            return;
        }
        {
            let mut inner = self.0.borrow_mut();
            inner.selected_path = None;
            inner.sidebar_open = false;
        }
        self.dispatch(ClientEvent::Close);
        self.render();
    }

    fn select_entry(&self, path: &str) {
        {
            let inner = self.0.borrow();
            if !inner.local_mode && !inner.active_workspace_ownership.contains_key(path) {
                return;
            }
        }
        self.flush_ink_before_transition();
        let path_str = path.to_string();
        let pending_text_persistence = self
            .0
            .borrow()
            .text_client_state
            .as_ref()
            .is_some_and(TextClientState::has_pending_persistence);
        if self.0.borrow().client_state.has_pending_intent() || pending_text_persistence {
            let mut inner = self.0.borrow_mut();
            inner.deferred_navigation = Some(path_str);
            inner.sidebar_open = false;
            inner.inspector_open &= !inspector_is_drawer();
            inner.persistence_status = if inner.client_state.local_persistence_failed()
                || inner
                    .text_client_state
                    .as_ref()
                    .is_some_and(TextClientState::local_persistence_failed)
            {
                "Save failed; retry before leaving this note".to_string()
            } else {
                "Finishing local save before navigation...".to_string()
            };
            drop(inner);
            self.render();
            return;
        }
        let is_markdown = path_str.to_lowercase().ends_with(".md")
            && !path_str.starts_with('.')
            && !path_str.contains("/.");

        let selection_rev = {
            let mut inner = self.0.borrow_mut();
            inner.selection_revision += 1;
            inner.document_error = None;
            revoke_asset_preview(&mut inner.asset_preview_url);
            inner.asset_preview_type.clear();
            inner.sidebar_open = false;
            inner.inspector_open &= !inspector_is_drawer();
            inner.editing_block = None;
            inner.pending_block_caret = None;
            inner.selection_revision
        };
        ui::state().reset_explorer();
        ui::state().reveal(&path_str);

        let is_asset = {
            let inner = self.0.borrow();
            inner
                .entries
                .iter()
                .any(|e| e.path.as_str() == path_str && e.kind == WorkspaceEntryKind::Asset)
        };
        if is_asset {
            self.0.borrow_mut().selected_path = Some(path_str.clone());
            let this = self.clone();
            let workspace_db = self.0.borrow().workspace_db.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let Some(workspace_db) = workspace_db else {
                    return;
                };
                let Ok(path) = WorkspacePath::new(&path_str) else {
                    return;
                };
                let Ok(Some(bytes)) = workspace_db.read_file(&path).await else {
                    return;
                };
                let media_type = asset_media_type(&path_str);
                match create_blob_url(&bytes, &media_type) {
                    Ok(url) => {
                        let mut inner = this.0.borrow_mut();
                        if inner.selection_revision != selection_rev {
                            let _ = web_sys::Url::revoke_object_url(&url);
                            return;
                        }
                        revoke_asset_preview(&mut inner.asset_preview_url);
                        inner.asset_preview_url = Some(url);
                        inner.asset_preview_type = media_type;
                        drop(inner);
                        this.render();
                    }
                    Err(error) => this.set_sync_error(error),
                }
            });
            self.render();
            return;
        }
        let this = self.clone();
        let ws = self.0.borrow().workspace_db.clone();
        let ws = match ws {
            Some(w) => w,
            None => return,
        };

        wasm_bindgen_futures::spawn_local(async move {
            match ws.get_doc_id(&path_str).await {
                Ok(Some(existing_id)) => {
                    let is_current = { this.0.borrow().selection_revision == selection_rev };
                    if !is_current {
                        return;
                    }
                    if is_markdown {
                        let mut inner = this.0.borrow_mut();
                        inner.pending_selection = Some(PendingSelection {
                            path: path_str.clone(),
                            doc_id: DocId(existing_id),
                            revision: selection_rev,
                        });
                    }
                    let doc_id = DocId(existing_id);
                    {
                        let mut inner = this.0.borrow_mut();
                        inner.selected_doc_id = doc_id;
                        inner.selected_path = Some(path_str);
                    }
                    this.dispatch(ClientEvent::OpenDocument { doc_id });
                    this.render();
                }
                Ok(None) => {
                    if !can_edit_workspace(&this.0.borrow()) {
                        this.0.borrow_mut().document_error = Some(format!(
                            "Document mapping for {path_str} is unavailable; the read-only note was quarantined"
                        ));
                        this.render();
                        return;
                    }
                    let new_id = uuid::Uuid::new_v4();
                    let doc_id = DocId(new_id);
                    if let Err(e) = ws.set_doc_id(&path_str, new_id).await {
                        this.0.borrow_mut().storage_error =
                            Some(format!("Failed to save document mapping: {:?}", e));
                        this.render();
                        return;
                    }
                    let is_current = { this.0.borrow().selection_revision == selection_rev };
                    if !is_current {
                        return;
                    }
                    if is_markdown {
                        let mut inner = this.0.borrow_mut();
                        inner.pending_selection = Some(PendingSelection {
                            path: path_str.clone(),
                            doc_id,
                            revision: selection_rev,
                        });
                    }
                    {
                        let mut inner = this.0.borrow_mut();
                        inner.selected_doc_id = doc_id;
                        inner.selected_path = Some(path_str.clone());
                        inner.locally_new_docs.insert(doc_id);
                    }
                    this.dispatch(ClientEvent::OpenDocument { doc_id });
                    this.render();
                }
                Err(e) => {
                    let is_current = { this.0.borrow().selection_revision == selection_rev };
                    if !is_current {
                        return;
                    }
                    this.0.borrow_mut().storage_error =
                        Some(format!("Failed to read document mapping: {:?}", e));
                    this.render();
                }
            }
        });
    }

    fn create_file(&self, name: &str) {
        if !can_edit_workspace(&self.0.borrow()) {
            return;
        }
        let mut name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let create = ui::state().create.get_untracked().unwrap_or_default();
        if !create.folder && !note_name_has_extension(&name) {
            name.push_str(".md");
        }
        let full_path = match &create.parent {
            Some(folder) => format!("{folder}/{name}"),
            None => name.clone(),
        };
        let path = match WorkspacePath::new(&full_path) {
            Ok(p) => p,
            Err(_) => {
                self.0.borrow_mut().storage_error =
                    Some(format!("Invalid file name: {}", full_path));
                self.render();
                return;
            }
        };
        let path_str = path.as_str().to_string();
        if create.folder {
            self.create_folder_entry(path);
            return;
        }
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
                        this.0.borrow_mut().locally_new_docs.insert(doc_id);
                        if let Err(error) =
                            this.associate_active_path(&path_str, Some(doc_id)).await
                        {
                            this.0.borrow_mut().sync_error = Some(error);
                        }
                        this.refresh_entries();
                        this.index_workspace();
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

    fn create_folder_entry(&self, path: WorkspacePath) {
        let (workspace_db, local_mode, workspace_id, key) = {
            let inner = self.0.borrow();
            (
                inner.workspace_db.clone(),
                inner.local_mode,
                inner.workspace_id,
                inner.workspace_key,
            )
        };
        let Some(workspace_db) = workspace_db else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut folder_id = None;
            if !local_mode {
                let (Some(workspace_id), Some(key)) = (workspace_id, key) else {
                    return;
                };
                let id = uuid::Uuid::new_v4();
                let encrypted = match crypto::encrypt(
                    &key,
                    path.as_str().as_bytes(),
                    &crypto::folder_path_aad(workspace_id, id),
                ) {
                    Ok(encrypted) => encrypted,
                    Err(error) => {
                        this.set_sync_error(error);
                        return;
                    }
                };
                let endpoint = format!("/api/workspaces/{workspace_id}/path-projections/{id}");
                if let Err(error) = api::json::<PathProjectionResponse, _>(
                    "PUT",
                    &endpoint,
                    Some(&PutPathProjectionRequest {
                        entry_kind: "folder".into(),
                        encrypted_path_b64: encrypted.ciphertext_b64,
                        path_nonce_b64: encrypted.nonce_b64,
                        projection_version: 1,
                        aad_version: crypto::AAD_VERSION,
                    }),
                )
                .await
                {
                    this.set_sync_error(error.message);
                    return;
                }
                folder_id = Some((workspace_id, id));
            }
            if let Err(error) = workspace_db.create_folder(&path).await {
                if let Some((workspace_id, id)) = folder_id {
                    let endpoint = format!("/api/workspaces/{workspace_id}/path-projections/{id}");
                    let _ = api::empty::<serde_json::Value>("DELETE", &endpoint, None).await;
                }
                this.set_sync_error(format!("Folder creation rolled back: {error}"));
                return;
            }
            if let Some((_, id)) = folder_id {
                let _ = workspace_db
                    .set_metadata(&folder_id_metadata(path.as_str()), &id.to_string())
                    .await;
                let _ = workspace_db
                    .set_metadata(&folder_version_metadata(path.as_str()), "1")
                    .await;
            }
            let ui = ui::state();
            ui.create.set(None);
            ui.expanded.update(|expanded| {
                expanded.insert(path.as_str().into());
            });
            this.refresh_entries();
            this.render();
        });
    }

    fn rename_folder_tree(&self, old_path: &str, new_path: &str) {
        let (Ok(source), Ok(destination)) = (
            WorkspacePath::new(old_path),
            WorkspacePath::new(new_path.trim()),
        ) else {
            self.set_sync_error("Invalid folder destination".into());
            return;
        };
        let (workspace_db, entries, local_mode, workspace_id, key) = {
            let inner = self.0.borrow();
            (
                inner.workspace_db.clone(),
                inner.entries.clone(),
                inner.local_mode,
                inner.workspace_id,
                inner.workspace_key,
            )
        };
        let Some(workspace_db) = workspace_db else {
            return;
        };
        let prefix = format!("{}/", source.as_str());
        let moving = entries
            .iter()
            .filter(|entry| entry.path == source || entry.path.as_str().starts_with(&prefix))
            .cloned()
            .collect::<Vec<_>>();
        let targets = moving
            .iter()
            .filter_map(|entry| {
                let suffix = entry.path.as_str().strip_prefix(source.as_str())?;
                WorkspacePath::new(format!("{}{suffix}", destination.as_str()))
                    .ok()
                    .map(|target| (entry.clone(), target))
            })
            .collect::<Vec<_>>();
        let moving_paths = moving
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<HashSet<_>>();
        if targets.len() != moving.len()
            || targets.iter().any(|(_, target)| {
                entries.iter().any(|entry| {
                    entry.path == *target && !moving_paths.contains(entry.path.as_str())
                })
            })
        {
            self.set_sync_error("Folder move collides with an existing path".into());
            return;
        }
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if !local_mode {
                let (Some(workspace_id), Some(key)) = (workspace_id, key) else {
                    return;
                };
                let mut request = WorkspaceTreeMutationRequest {
                    doc_updates: Vec::new(),
                    asset_updates: Vec::new(),
                    projection_updates: Vec::new(),
                    delete_doc_ids: Vec::new(),
                    delete_asset_ids: Vec::new(),
                    delete_projection_ids: Vec::new(),
                };
                for (entry, target) in &targets {
                    match entry.kind {
                        WorkspaceEntryKind::Folder => {
                            let id = workspace_db
                                .get_metadata(&folder_id_metadata(entry.path.as_str()))
                                .await
                                .ok()
                                .flatten()
                                .and_then(|value| uuid::Uuid::parse_str(&value).ok());
                            let version = workspace_db
                                .get_metadata(&folder_version_metadata(entry.path.as_str()))
                                .await
                                .ok()
                                .flatten()
                                .and_then(|value| value.parse::<i64>().ok())
                                .unwrap_or(1)
                                + 1;
                            let Some(id) = id else {
                                this.set_sync_error(format!(
                                    "Folder {} has no synchronized identity",
                                    entry.path.as_str()
                                ));
                                return;
                            };
                            let encrypted = match crypto::encrypt(
                                &key,
                                target.as_str().as_bytes(),
                                &crypto::folder_path_aad(workspace_id, id),
                            ) {
                                Ok(value) => value,
                                Err(error) => {
                                    this.set_sync_error(error);
                                    return;
                                }
                            };
                            request.projection_updates.push(TreePathProjectionUpdate {
                                entry_id: id,
                                projection: PutPathProjectionRequest {
                                    entry_kind: "folder".into(),
                                    encrypted_path_b64: encrypted.ciphertext_b64,
                                    path_nonce_b64: encrypted.nonce_b64,
                                    projection_version: version,
                                    aad_version: 1,
                                },
                            });
                        }
                        WorkspaceEntryKind::Markdown => {
                            let Some(doc_id) = workspace_db
                                .get_doc_id(entry.path.as_str())
                                .await
                                .ok()
                                .flatten()
                            else {
                                this.set_sync_error("Document mapping is unavailable".into());
                                return;
                            };
                            let projection_key =
                                remote_path_envelope_metadata(workspace_id, DocId(doc_id));
                            let version = workspace_db
                                .get_metadata(&projection_key)
                                .await
                                .ok()
                                .flatten()
                                .and_then(|json| {
                                    serde_json::from_str::<LocalPathProjection>(&json).ok()
                                })
                                .map_or(1, |projection| projection.projection_version + 1);
                            let encrypted = match crypto::encrypt(
                                &key,
                                target.as_str().as_bytes(),
                                &crypto::path_aad(workspace_id, doc_id),
                            ) {
                                Ok(value) => value,
                                Err(error) => {
                                    this.set_sync_error(error);
                                    return;
                                }
                            };
                            request.doc_updates.push(TreeDocPathUpdate {
                                doc_id,
                                projection: UpdateDocPathProjectionRequest {
                                    encrypted_path_b64: encrypted.ciphertext_b64,
                                    path_nonce_b64: encrypted.nonce_b64,
                                    projection_version: version,
                                },
                            });
                        }
                        WorkspaceEntryKind::Asset => {
                            let Some(asset_id) = workspace_db
                                .get_metadata(&asset_id_metadata(entry.path.as_str()))
                                .await
                                .ok()
                                .flatten()
                                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                            else {
                                this.set_sync_error("Asset mapping is unavailable".into());
                                return;
                            };
                            let version = workspace_db
                                .get_metadata(&asset_version_metadata(entry.path.as_str()))
                                .await
                                .ok()
                                .flatten()
                                .and_then(|value| value.parse::<i64>().ok())
                                .unwrap_or(1)
                                + 1;
                            let bytes = match workspace_db.read_file(&entry.path).await {
                                Ok(Some(bytes)) => bytes,
                                _ => {
                                    this.set_sync_error("Asset cache is unavailable".into());
                                    return;
                                }
                            };
                            use sha2::Digest;
                            let metadata = AssetMetadata {
                                path: target.as_str().into(),
                                media_type: asset_media_type(target.as_str()),
                                plaintext_size: bytes.len() as u64,
                                content_sha256: base64::engine::general_purpose::STANDARD
                                    .encode(sha2::Sha256::digest(&bytes)),
                            };
                            let encrypted = match encrypt_asset(
                                &key,
                                workspace_id,
                                asset_id,
                                1,
                                &metadata,
                                &bytes,
                            ) {
                                Ok(value) => value,
                                Err(error) => {
                                    this.set_sync_error(error.to_string());
                                    return;
                                }
                            };
                            request.asset_updates.push(TreeAssetMetadataUpdate {
                                asset_id,
                                metadata: UpdateAssetMetadataRequest {
                                    encrypted_metadata_b64: encrypted.metadata_ciphertext_b64,
                                    metadata_nonce_b64: encrypted.metadata_nonce_b64,
                                    metadata_version: version,
                                    aad_version: 1,
                                },
                            });
                        }
                        WorkspaceEntryKind::Internal => {}
                    }
                }
                let endpoint = format!("/api/workspaces/{workspace_id}/tree-mutations");
                if let Err(error) = api::empty("POST", &endpoint, Some(&request)).await {
                    this.set_sync_error(error.message);
                    return;
                }
            }
            if let Err(error) = workspace_db.rename_tree(&source, &destination).await {
                this.set_sync_error(format!(
                    "Remote move committed; local retry required: {error}"
                ));
                return;
            }
            {
                let mut inner = this.0.borrow_mut();
                let ownership = inner.active_workspace_ownership.clone();
                for (path, id) in ownership {
                    if path == source.as_str() || path.starts_with(&prefix) {
                        inner.active_workspace_ownership.remove(&path);
                        let suffix = path.strip_prefix(source.as_str()).unwrap();
                        inner
                            .active_workspace_ownership
                            .insert(format!("{}{suffix}", destination.as_str()), id);
                    }
                }
                ui::state().expanded.update(|expanded| {
                    expanded.remove(source.as_str());
                    expanded.insert(destination.as_str().into());
                });
            }
            this.refresh_entries();
            this.index_workspace();
            this.render();
        });
    }

    fn delete_folder_tree(&self, path: &str) {
        let Ok(folder) = WorkspacePath::new(path) else {
            return;
        };
        let (workspace_db, entries, local_mode, workspace_id) = {
            let inner = self.0.borrow();
            (
                inner.workspace_db.clone(),
                inner.entries.clone(),
                inner.local_mode,
                inner.workspace_id,
            )
        };
        let Some(workspace_db) = workspace_db else {
            return;
        };
        let prefix = format!("{}/", folder.as_str());
        let deleting = entries
            .into_iter()
            .filter(|entry| entry.path == folder || entry.path.as_str().starts_with(&prefix))
            .collect::<Vec<_>>();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if !local_mode {
                let Some(workspace_id) = workspace_id else {
                    return;
                };
                let mut request = WorkspaceTreeMutationRequest {
                    doc_updates: Vec::new(),
                    asset_updates: Vec::new(),
                    projection_updates: Vec::new(),
                    delete_doc_ids: Vec::new(),
                    delete_asset_ids: Vec::new(),
                    delete_projection_ids: Vec::new(),
                };
                for entry in &deleting {
                    match entry.kind {
                        WorkspaceEntryKind::Folder => {
                            if let Some(id) = workspace_db
                                .get_metadata(&folder_id_metadata(entry.path.as_str()))
                                .await
                                .ok()
                                .flatten()
                                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                            {
                                request.delete_projection_ids.push(id);
                            }
                        }
                        WorkspaceEntryKind::Markdown => {
                            if let Some(id) = workspace_db
                                .get_doc_id(entry.path.as_str())
                                .await
                                .ok()
                                .flatten()
                            {
                                request.delete_doc_ids.push(id);
                            }
                        }
                        WorkspaceEntryKind::Asset => {
                            if let Some(id) = workspace_db
                                .get_metadata(&asset_id_metadata(entry.path.as_str()))
                                .await
                                .ok()
                                .flatten()
                                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                            {
                                request.delete_asset_ids.push(id);
                            }
                        }
                        WorkspaceEntryKind::Internal => {}
                    }
                }
                let endpoint = format!("/api/workspaces/{workspace_id}/tree-mutations");
                if let Err(error) = api::empty("POST", &endpoint, Some(&request)).await {
                    this.set_sync_error(error.message);
                    return;
                }
            }
            if let Err(error) = workspace_db.delete_tree(&folder).await {
                this.set_sync_error(format!(
                    "Remote delete committed; local retry required: {error}"
                ));
                return;
            }
            {
                let mut inner = this.0.borrow_mut();
                inner
                    .active_workspace_ownership
                    .retain(|path, _| path != folder.as_str() && !path.starts_with(&prefix));
                if inner
                    .selected_path
                    .as_ref()
                    .is_some_and(|path| path == folder.as_str() || path.starts_with(&prefix))
                {
                    inner.selected_path = None;
                }
            }
            this.refresh_entries();
            this.index_workspace();
            this.render();
        });
    }
    fn delete_entry(&self, path: &str) {
        if !can_edit_workspace(&self.0.borrow()) {
            return;
        }
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
        let is_asset = self.0.borrow().entries.iter().any(|entry| {
            entry.path.as_str() == path_str && entry.kind == WorkspaceEntryKind::Asset
        });
        if is_selected {
            self.flush_ink_before_transition();
        }
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
            let workspace_id = {
                let inner = this.0.borrow();
                (!inner.local_mode).then_some(inner.workspace_id).flatten()
            };
            let mut remote_delete_succeeded = workspace_id.is_none();
            if is_asset && workspace_id.is_some() {
                let asset_id = ws
                    .get_metadata(&asset_id_metadata(&path_str))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| uuid::Uuid::parse_str(&value).ok());
                let Some(asset_id) = asset_id else {
                    this.set_sync_error(
                        "Asset was not deleted because its synchronized identity is unavailable"
                            .into(),
                    );
                    return;
                };
                let endpoint = format!("/api/assets/{asset_id}");
                if let Err(error) = api::empty::<serde_json::Value>("DELETE", &endpoint, None).await
                {
                    this.set_sync_error(format!(
                        "Asset was not deleted locally; remote delete can be retried: {}",
                        error.message
                    ));
                    return;
                }
                remote_delete_succeeded = true;
            }
            if let (Some(_), Some(did)) = (workspace_id, doc_id) {
                let endpoint = format!("/api/docs/{did}");
                if let Err(error) = api::empty::<serde_json::Value>("DELETE", &endpoint, None).await
                {
                    this.set_sync_error(format!(
                        "Document was not deleted locally; remote delete failed and can be retried: {}",
                        error.message
                    ));
                    return;
                }
                remote_delete_succeeded = true;
            }
            if !local_delete_is_safe(workspace_id.is_none(), remote_delete_succeeded) {
                this.set_sync_error(
                    "Document was not deleted because its synchronized identity is unavailable"
                        .into(),
                );
                return;
            }
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
            let workspace_id = {
                let mut inner = this.0.borrow_mut();
                if inner.local_mode {
                    None
                } else {
                    inner.active_workspace_ownership.remove(&path_str);
                    inner.advance_workspace_revision();
                    inner.workspace_id
                }
            };
            if let Some(workspace_id) = workspace_id {
                if let Err(error) = this.persist_active_workspace_ownership(workspace_id).await {
                    this.0.borrow_mut().sync_error = Some(error);
                }
            }
            this.handle_delete_recent_note(&path_str);
            if is_selected {
                this.0.borrow_mut().selected_path = None;
                this.dispatch(ClientEvent::Close);
            }
            this.refresh_entries();
            this.index_workspace();
            this.render();
            if workspace_id.is_some()
                && let Some(did) = doc_id
            {
                this.0.borrow_mut().remote_docs.remove(&DocId(did));
            }
        });
    }

    fn rename_entry(&self, old_path: &str, new_name: &str) {
        if !can_edit_workspace(&self.0.borrow()) {
            return;
        }
        let old_path = old_path.to_string();
        let new_name = new_name.trim().to_string();
        if new_name.is_empty() {
            return;
        }
        let new_path = match WorkspacePath::new(&new_name) {
            Ok(p) => p,
            Err(_) => {
                self.0.borrow_mut().storage_error =
                    Some(format!("Invalid file name: {}", new_name));
                ui::state().renaming.set(None);
                self.render();
                return;
            }
        };
        let old_path_wp = match WorkspacePath::new(&old_path) {
            Ok(p) => p,
            Err(_) => {
                ui::state().renaming.set(None);
                return;
            }
        };
        let this = self.clone();
        let ws = self.0.borrow().workspace_db.clone();
        let ws = match ws {
            Some(w) => w,
            None => return,
        };
        let is_selected = self.0.borrow().selected_path.as_deref() == Some(&old_path);
        let (is_asset, local_mode, workspace_id, workspace_key) = {
            let inner = self.0.borrow();
            (
                inner.entries.iter().any(|entry| {
                    entry.path.as_str() == old_path && entry.kind == WorkspaceEntryKind::Asset
                }),
                inner.local_mode,
                inner.workspace_id,
                inner.workspace_key,
            )
        };
        wasm_bindgen_futures::spawn_local(async move {
            if is_asset && !local_mode {
                let (Some(workspace_id), Some(key)) = (workspace_id, workspace_key) else {
                    return;
                };
                let Some(asset_id) = ws
                    .get_metadata(&asset_id_metadata(&old_path))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                else {
                    this.set_sync_error("Asset mapping is unavailable".into());
                    return;
                };
                let version = ws
                    .get_metadata(&asset_version_metadata(&old_path))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(1)
                    + 1;
                let bytes = match ws.read_file(&old_path_wp).await {
                    Ok(Some(bytes)) => bytes,
                    _ => {
                        this.set_sync_error("Asset cache is unavailable".into());
                        return;
                    }
                };
                use sha2::Digest;
                let metadata = AssetMetadata {
                    path: new_name.clone(),
                    media_type: asset_media_type(&new_name),
                    plaintext_size: bytes.len() as u64,
                    content_sha256: base64::engine::general_purpose::STANDARD
                        .encode(sha2::Sha256::digest(&bytes)),
                };
                let encrypted =
                    match encrypt_asset(&key, workspace_id, asset_id, 1, &metadata, &bytes) {
                        Ok(value) => value,
                        Err(error) => {
                            this.set_sync_error(error.to_string());
                            return;
                        }
                    };
                let endpoint = format!("/api/assets/{asset_id}");
                if let Err(error) = api::json::<AssetResponse, _>(
                    "PATCH",
                    &endpoint,
                    Some(&UpdateAssetMetadataRequest {
                        encrypted_metadata_b64: encrypted.metadata_ciphertext_b64,
                        metadata_nonce_b64: encrypted.metadata_nonce_b64,
                        metadata_version: version,
                        aad_version: 1,
                    }),
                )
                .await
                {
                    this.set_sync_error(error.message);
                    return;
                }
            }
            match ws.rename_file(&old_path_wp, &new_path).await {
                Ok(()) => {
                    let doc_id = match ws.get_doc_id(&new_name).await {
                        Ok(doc_id) => doc_id.map(DocId),
                        Err(error) => {
                            this.0.borrow_mut().sync_error = Some(format!(
                                "Rename succeeded locally, but document lookup failed: {error:?}"
                            ));
                            None
                        }
                    };
                    if is_selected {
                        this.0.borrow_mut().selected_path = Some(new_name.clone());
                    }
                    ui::state().renaming.set(None);
                    let workspace_id = {
                        let mut inner = this.0.borrow_mut();
                        if inner.local_mode {
                            None
                        } else {
                            inner.active_workspace_ownership.remove(&old_path);
                            inner
                                .active_workspace_ownership
                                .insert(new_name.clone(), doc_id);
                            inner.advance_workspace_revision();
                            inner.workspace_id
                        }
                    };
                    if let Some(workspace_id) = workspace_id {
                        if let Err(error) =
                            this.persist_active_workspace_ownership(workspace_id).await
                        {
                            this.0.borrow_mut().sync_error = Some(error);
                        }
                    }
                    this.handle_rename_recent_note(&old_path, &new_name);
                    this.refresh_entries();
                    this.index_workspace();
                    this.render();
                    if workspace_id.is_some() {
                        if let Some(doc_id) = doc_id {
                            if let Err(error) =
                                this.update_remote_path_projection(doc_id, &new_name).await
                            {
                                this.set_sync_error(format!(
                                    "Renamed locally, but the encrypted remote path is pending: {error}"
                                ));
                            }
                        }
                    }
                }
                Err(e) => {
                    this.0.borrow_mut().storage_error = Some(format!("Rename failed: {:?}", e));
                    ui::state().renaming.set(None);
                    this.render();
                }
            }
        });
    }

    fn navigate_to_link(&self, target_path: &str) {
        let entry = {
            let inner = self.0.borrow();
            inner
                .entries
                .iter()
                .find(|e| e.path.as_str() == target_path)
                .cloned()
        };
        match entry {
            Some(e) if e.kind == WorkspaceEntryKind::Markdown => {
                self.select_entry(target_path);
            }
            Some(_) => {
                // Asset or internal entry — do not navigate
                self.0.borrow_mut().link_message = Some(format!(
                    "Cannot navigate: not a Markdown note ({})",
                    target_path
                ));
                self.render();
            }
            None => {
                self.0.borrow_mut().link_message =
                    Some(format!("Linked note not found: {}", target_path));
                self.render();
            }
        }
    }

    fn request_missing_note(&self, raw_target: &str, source_path: &str) {
        let (resolution, proposal) = {
            let inner = self.0.borrow();
            let resolution = inner.knowledge.resolve_wikilink(source_path, raw_target);
            (
                resolution,
                knowledge::propose_missing_note(source_path, raw_target),
            )
        };
        let proposed_path = match (&resolution, proposal) {
            (knowledge::WikilinkResolution::Unresolved, Some(path)) => path,
            (knowledge::WikilinkResolution::Ambiguous(_), _) => {
                self.0.borrow_mut().link_message =
                    Some(format!("Ambiguous note link: {}", raw_target));
                self.render();
                return;
            }
            _ => {
                self.0.borrow_mut().link_message =
                    Some(format!("Cannot create note: {}", raw_target));
                self.render();
                return;
            }
        };
        {
            let mut inner = self.0.borrow_mut();
            inner.missing_note = Some(MissingNoteRequest {
                raw_target: raw_target.to_string(),
                proposed_path,
                source_path: source_path.to_string(),
                resolution,
            });
            inner.pending_focus_key = Some("dismiss-missing-note".to_string());
        }
        self.render();
    }

    fn refresh_entries(&self) {
        let (ws, revision, local_mode, ownership) = {
            let inner = self.0.borrow();
            let Some(ws) = inner.workspace_db.clone() else {
                return;
            };
            (
                ws,
                inner.workspace_revision,
                inner.local_mode,
                inner.active_workspace_ownership.clone(),
            )
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match ws.list_entries().await {
                Ok(mut entries) => {
                    if this.0.borrow().workspace_revision != revision {
                        return;
                    }
                    if !local_mode {
                        entries.retain(|entry| {
                            entry.kind == WorkspaceEntryKind::Folder
                                || ownership.contains_key(entry.path.as_str())
                        });
                    }
                    entries.sort_by(|a, b| a.path.cmp(&b.path));
                    this.0.borrow_mut().entries = entries;
                    this.0.borrow_mut().workspace_loaded = true;

                    // Clean and load recent notes
                    let recents = {
                        let inner = this.0.borrow();
                        inner.recent_notes.clone()
                    };
                    this.clean_and_set_recent_notes(recents);

                    this.render();

                    // Re-index search database
                    this.index_workspace();
                }
                Err(e) => {
                    if this.0.borrow().workspace_revision != revision {
                        return;
                    }
                    this.0.borrow_mut().storage_error =
                        Some(format!("List entries failed: {:?}", e));
                    this.render();
                }
            }
        });
    }

    fn index_workspace(&self) {
        let (ws, db_opt, revision, workspace_id, device_id, index_revision) = {
            let mut inner = self.0.borrow_mut();
            let Some(ws) = inner.workspace_db.clone() else {
                return;
            };
            inner.search_index_revision += 1;
            (
                ws,
                inner.db.clone(),
                inner.workspace_revision,
                inner.workspace_id,
                inner.device_id,
                inner.search_index_revision,
            )
        };

        let this = self.clone();
        ui::state().indexing.set(true);
        this.0.borrow_mut().index_error = None;
        this.render();

        wasm_bindgen_futures::spawn_local(async move {
            let db = match db_opt {
                Some(d) => d,
                None => match IndexedDb::open("inkstone_local_db", 1).await {
                    Ok(d) => {
                        this.0.borrow_mut().db = Some(d.clone());
                        d
                    }
                    Err(e) => {
                        ui::state().indexing.set(false);
                        this.0.borrow_mut().storage_error =
                            Some(format!("Search index fail-closed (db open): {:?}", e));
                        this.render();
                        return;
                    }
                },
            };

            if this.0.borrow().workspace_revision != revision
                || this.0.borrow().search_index_revision != index_revision
            {
                return;
            }

            let markdown_files: Vec<String> = this
                .0
                .borrow()
                .entries
                .iter()
                .filter(|e| e.kind == WorkspaceEntryKind::Markdown)
                .map(|e| e.path.as_str().to_string())
                .collect();

            let mut search_docs = Vec::new();
            let mut summaries = HashMap::new();

            // Rebuild knowledge index completely from scratch
            let mut new_knowledge = knowledge::KnowledgeIndex::new();
            // First pass: register all paths for wikilink resolution
            for path in &markdown_files {
                new_knowledge.register_path(path);
            }

            for path in markdown_files {
                if this.0.borrow().workspace_revision != revision
                    || this.0.borrow().search_index_revision != index_revision
                {
                    return;
                }
                let file_name = explorer::file_name_only(&path).to_string();
                let doc_id_opt = match ws.get_doc_id(&path).await {
                    Ok(Some(uuid)) => Some(DocId(uuid)),
                    Ok(None) => None,
                    Err(e) => {
                        this.0.borrow_mut().index_error = Some(format!(
                            "Skipped {path} while indexing its document mapping: {e:?}"
                        ));
                        continue;
                    }
                };

                let mut title = String::new();
                let mut blocks = Vec::new();
                let mut md_blocks_for_knowledge = Vec::new();
                let mut summary = None;

                if let Some(doc_id) = doc_id_opt {
                    let storage_key = document_storage_key(workspace_id, doc_id);
                    let record_result = db.get_document_record(&storage_key).await;
                    match record_result {
                        Ok(Some(record_json)) => match storage::deserialize_record(&record_json) {
                            Ok(record) => {
                                let edited_at_ms = record
                                    .pending_updates
                                    .iter()
                                    .map(|update| update.applied_at_ms)
                                    .fold(record.edited_at_ms, i64::max);
                                let mut doc = record.confirmed_doc;
                                let text_pending = record
                                    .pending_updates
                                    .iter()
                                    .filter(|update| {
                                        matches!(update.op, DocumentOp::ApplyTextUpdateV2 { .. })
                                    })
                                    .cloned()
                                    .collect::<Vec<_>>();
                                let mut replay_failed = None;
                                for update in record.pending_updates {
                                    if matches!(update.op, DocumentOp::ApplyTextUpdateV2 { .. }) {
                                        continue;
                                    }
                                    if let Err(e) =
                                        inkstone_core::doc_ops::apply_update(&mut doc, update)
                                    {
                                        replay_failed = Some(e);
                                        break;
                                    }
                                }
                                if let Some(projection) = record.text_projection.as_ref() {
                                    storage::preserve_collaborative_text(&mut doc, projection);
                                }
                                if replay_failed.is_none()
                                    && record.text_projection.is_none()
                                    && (record.text_snapshot.is_some() || !text_pending.is_empty())
                                {
                                    let replica_id = text_replica_id(device_id, doc_id);
                                    match TextClientState::new(doc.clone(), replica_id) {
                                        Ok(mut state) => {
                                            let effects =
                                                state.transition(TextClientEvent::Hydrate {
                                                    document: doc.clone(),
                                                    snapshot: record.text_snapshot,
                                                    pending_updates: text_pending,
                                                });
                                            if let Some(error) = effects.into_iter().find_map(
                                                |effect| match effect {
                                                    TextClientEffect::NotifyError(error) => {
                                                        Some(error)
                                                    }
                                                    _ => None,
                                                },
                                            ) {
                                                this.0.borrow_mut().index_error = Some(format!(
                                                    "Skipped {path} while rebuilding text index: {error}"
                                                ));
                                                continue;
                                            }
                                            doc = state.document().clone();
                                        }
                                        Err(error) => {
                                            this.0.borrow_mut().index_error = Some(format!(
                                                "Skipped {path} while initializing text index: {error}"
                                            ));
                                            continue;
                                        }
                                    }
                                }
                                if let Some(error) = replay_failed {
                                    this.0.borrow_mut().index_error = Some(format!(
                                        "Skipped {path} while rebuilding derived indexes: {error:?}"
                                    ));
                                    continue;
                                }
                                title = doc.title.clone();
                                summary = Some(home::summarize(&doc, edited_at_ms));
                                md_blocks_for_knowledge =
                                    inkstone_core::markdown::index_document_blocks(&doc);
                                for block in doc.blocks {
                                    if let inkstone_core::document::BlockContent::Markdown {
                                        text,
                                    } = block.content
                                    {
                                        blocks.push(text);
                                    }
                                }
                            }
                            Err(e) => {
                                this.0.borrow_mut().index_error = Some(format!(
                                    "Skipped corrupt local note {path} while indexing: {e:?}"
                                ));
                                continue;
                            }
                        },
                        Ok(None) => {}
                        Err(e) => {
                            this.0.borrow_mut().index_error = Some(format!(
                                "Skipped unreadable local note {path} while indexing: {e:?}"
                            ));
                            continue;
                        }
                    }
                }

                if let Some(summary) = summary {
                    summaries.insert(path.clone(), summary);
                }
                search_docs.push(search::SearchDocument {
                    path: path.clone(),
                    file_name,
                    doc_id: doc_id_opt,
                    title,
                    blocks,
                });

                new_knowledge.index_document(&path, &md_blocks_for_knowledge);
            }

            {
                let mut inner = this.0.borrow_mut();
                if inner.workspace_revision != revision
                    || inner.search_index_revision != index_revision
                {
                    return;
                }
                inner.search_documents = search_docs;
                inner.note_summaries = summaries;
                inner.knowledge = new_knowledge;
                ui::state().indexing.set(false);
            }
            ui::state().rank(&this.0.borrow().search_documents);
            this.render();
        });
    }

    fn update_search_index_for_active_doc(&self, path: &str) {
        {
            let mut inner = self.0.borrow_mut();
            let summary = home::summarize(
                &Driver::effective_document(&inner),
                js_sys::Date::now() as i64,
            );
            inner.note_summaries.insert(path.to_string(), summary);
        }
        {
            let mut inner = self.0.borrow_mut();
            inner.search_index_revision += 1;
            ui::state().indexing.set(false);
            if let Some(pos) = inner
                .search_documents
                .iter()
                .position(|doc| doc.path == path)
            {
                let effective_doc = Driver::effective_document(&inner);
                let mut blocks = Vec::new();
                for block in &effective_doc.blocks {
                    if let inkstone_core::document::BlockContent::Markdown { text } = &block.content
                    {
                        blocks.push(text.clone());
                    }
                }
                inner.search_documents[pos].title = effective_doc.title.clone();
                inner.search_documents[pos].blocks = blocks;

                let md_blocks_for_knowledge =
                    inkstone_core::markdown::index_document_blocks(&effective_doc);
                inner
                    .knowledge
                    .index_document(path, &md_blocks_for_knowledge);
            } else {
                let effective_doc = Driver::effective_document(&inner);
                let blocks = effective_doc
                    .blocks
                    .iter()
                    .filter_map(|block| match &block.content {
                        inkstone_core::document::BlockContent::Markdown { text } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let md_blocks_for_knowledge =
                    inkstone_core::markdown::index_document_blocks(&effective_doc);
                inner.search_documents.push(search::SearchDocument {
                    path: path.to_string(),
                    file_name: explorer::file_name_only(path),
                    doc_id: Some(effective_doc.id),
                    title: effective_doc.title.clone(),
                    blocks,
                });
                inner.knowledge.register_path(path);
                inner
                    .knowledge
                    .index_document(path, &md_blocks_for_knowledge);
            }
        }
        if ui::state().searching() {
            ui::state().rank(&self.0.borrow().search_documents);
        }
    }

    fn clean_and_set_recent_notes(&self, mut recents: Vec<String>) {
        let valid_paths: std::collections::HashSet<String> = {
            let inner = self.0.borrow();
            inner
                .entries
                .iter()
                .filter(|e| e.kind == WorkspaceEntryKind::Markdown)
                .map(|e| e.path.as_str().to_string())
                .collect()
        };

        let old_recents = recents.clone();

        let mut i = 0;
        let mut seen = std::collections::HashSet::new();
        while i < recents.len() {
            let path = &recents[i];
            let is_valid = valid_paths.contains(path)
                && path.to_lowercase().ends_with(".md")
                && !path.starts_with('.')
                && !path.contains("/.");

            if !is_valid || !seen.insert(path.clone()) {
                recents.remove(i);
            } else {
                i += 1;
            }
        }

        if recents.len() > 8 {
            recents.truncate(8);
        }

        let changed = recents != old_recents;

        if changed {
            let ws = self.0.borrow().workspace_db.clone();
            let this = self.clone();
            let rev = {
                let mut inner = self.0.borrow_mut();
                inner.recent_notes_revision += 1;
                inner.recent_notes_revision
            };
            if let Some(ws) = ws {
                wasm_bindgen_futures::spawn_local(async move {
                    match ws.save_recent_notes(&recents).await {
                        Ok(()) => {
                            let mut inner = this.0.borrow_mut();
                            if inner.recent_notes_revision == rev {
                                inner.recent_notes = recents;
                                drop(inner);
                                this.render();
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().storage_error =
                                Some(format!("Failed to normalize recent notes: {:?}", e));
                            this.render();
                        }
                    }
                });
            }
        } else {
            self.0.borrow_mut().recent_notes = recents;
        }
    }

    fn complete_pending_recent(&self, doc_id: DocId) {
        let (path, rev) = {
            let mut inner = self.0.borrow_mut();
            if let Some(pending) = inner.pending_selection.take() {
                if pending.revision == inner.selection_revision && pending.doc_id == doc_id {
                    (pending.path, pending.revision)
                } else {
                    return;
                }
            } else {
                return;
            }
        };
        self.add_recent_note(&path, rev);
    }

    fn add_recent_note(&self, path: &str, revision: u64) {
        let mut inner = self.0.borrow_mut();
        if inner.selection_revision != revision {
            return;
        }

        let mut recents = inner.recent_notes.clone();
        recents.retain(|p| p != path);
        recents.insert(0, path.to_string());
        recents.truncate(8);

        inner.recent_notes = recents.clone();
        inner.recent_notes_revision += 1;
        let recent_revision = inner.recent_notes_revision;

        let ws = inner.workspace_db.clone();
        let this = self.clone();
        drop(inner);

        if let Some(ws) = ws {
            wasm_bindgen_futures::spawn_local(async move {
                match ws.save_recent_notes(&recents).await {
                    Ok(()) => {
                        if this.0.borrow().recent_notes_revision == recent_revision {
                            this.render();
                        }
                    }
                    Err(e) => {
                        this.0.borrow_mut().storage_error =
                            Some(format!("Failed to save recent notes: {:?}", e));
                        this.render();
                    }
                }
            });
        }
    }

    fn clear_recent_notes(&self) {
        let ws = self.0.borrow().workspace_db.clone();
        let this = self.clone();
        let rev = {
            let mut inner = self.0.borrow_mut();
            inner.recent_notes_revision += 1;
            inner.recent_notes.clear();
            inner.recent_notes_revision
        };
        if let Some(ws) = ws {
            wasm_bindgen_futures::spawn_local(async move {
                match ws.save_recent_notes(&[]).await {
                    Ok(()) => {
                        let inner = this.0.borrow_mut();
                        if inner.recent_notes_revision == rev {
                            drop(inner);
                            this.render();
                        }
                    }
                    Err(e) => {
                        this.0.borrow_mut().storage_error =
                            Some(format!("Failed to clear recent notes: {:?}", e));
                        this.render();
                    }
                }
            });
        }
    }

    fn handle_rename_recent_note(&self, old_path: &str, new_path: &str) {
        let mut recents = {
            let inner = self.0.borrow();
            inner.recent_notes.clone()
        };
        let mut changed = false;
        for path in recents.iter_mut() {
            if path == old_path {
                *path = new_path.to_string();
                changed = true;
            }
        }
        if changed {
            let ws = self.0.borrow().workspace_db.clone();
            let this = self.clone();
            let rev = {
                let mut inner = self.0.borrow_mut();
                inner.recent_notes_revision += 1;
                inner.recent_notes_revision
            };
            if let Some(ws) = ws {
                wasm_bindgen_futures::spawn_local(async move {
                    match ws.save_recent_notes(&recents).await {
                        Ok(()) => {
                            let mut inner = this.0.borrow_mut();
                            if inner.recent_notes_revision == rev {
                                inner.recent_notes = recents;
                                drop(inner);
                                this.render();
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().storage_error =
                                Some(format!("Failed to update recent notes on rename: {:?}", e));
                            this.render();
                        }
                    }
                });
            }
        }
    }

    fn handle_delete_recent_note(&self, path: &str) {
        let mut recents = {
            let inner = self.0.borrow();
            inner.recent_notes.clone()
        };
        if recents.contains(&path.to_string()) {
            recents.retain(|p| p != path);
            let ws = self.0.borrow().workspace_db.clone();
            let this = self.clone();
            let rev = {
                let mut inner = self.0.borrow_mut();
                inner.recent_notes = recents.clone();
                inner.recent_notes_revision += 1;
                inner.recent_notes_revision
            };
            if let Some(ws) = ws {
                wasm_bindgen_futures::spawn_local(async move {
                    match ws.save_recent_notes(&recents).await {
                        Ok(()) => {
                            if this.0.borrow().recent_notes_revision == rev {
                                this.render();
                            }
                        }
                        Err(e) => {
                            this.0.borrow_mut().storage_error =
                                Some(format!("Failed to update recent notes on delete: {:?}", e));
                            this.render();
                        }
                    }
                });
            }
        }
    }

    fn auth_action(&self, endpoint: &'static str) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let email = document
            .query_selector(".auth-email")
            .ok()
            .flatten()
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
            .map(|input| input.value())
            .unwrap_or_default();
        let password = document
            .query_selector(".auth-password")
            .ok()
            .flatten()
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
            .map(|input| input.value())
            .unwrap_or_default();
        if endpoint == "/api/auth/login" {
            let mut inner = self.0.borrow_mut();
            inner.new_account_setup_pending = false;
            inner.recovery_key_display = None;
            inner.account_vault_key = None;
        }
        if email.trim().is_empty() || password.is_empty() {
            self.0.borrow_mut().auth_error = Some("Email and password are required".into());
            self.render();
            return;
        }
        if endpoint == "/api/auth/register" {
            if let Err(message) = crypto::check_new_password(&password) {
                self.0.borrow_mut().auth_error = Some(message);
                self.render();
                return;
            }
        }
        if endpoint == "/api/auth/register" && self.0.borrow().recovery_key_display.is_none() {
            let Ok(vault_key) = crypto::random_key() else {
                self.0.borrow_mut().auth_error = Some("Secure randomness is unavailable".into());
                self.render();
                return;
            };
            let Ok(recovery_key) = crypto::random_key() else {
                self.0.borrow_mut().auth_error = Some("Secure randomness is unavailable".into());
                self.render();
                return;
            };
            let mut inner = self.0.borrow_mut();
            inner.account_vault_key = Some(vault_key);
            inner.recovery_key_display = Some(crypto::recovery_key_to_string(&recovery_key));
            inner.new_account_setup_pending = true;
            inner.auth_error =
                Some("Save your recovery key somewhere safe, then confirm below.".into());
            drop(inner);
            self.render();
            return;
        }
        if endpoint == "/api/auth/register" && !recovery_confirmation_checked(&document) {
            self.0.borrow_mut().auth_error =
                Some("Confirm you've saved your recovery key first".into());
            self.render();
            return;
        }
        let Some(device_id) = self.0.borrow().device_id else {
            self.0.borrow_mut().auth_error = Some("Device identity is unavailable".into());
            self.render();
            return;
        };
        self.0.borrow_mut().auth_phase = AuthPhase::Checking;
        self.0.borrow_mut().auth_error = None;
        self.render();
        let pending_invite = self.0.borrow().pending_invite.clone();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let secret = match crypto::login_secret(&password, &email) {
                Ok(secret) => secret,
                Err(message) => {
                    let mut inner = this.0.borrow_mut();
                    inner.auth_phase = AuthPhase::SignedOut;
                    inner.auth_error = Some(message);
                    drop(inner);
                    this.render();
                    return;
                }
            };
            let registration = RegisterRequest {
                email: email.clone(),
                password: secret.clone(),
                device_id,
                device_name: Some("Inkstone browser".into()),
            };
            let result = match endpoint {
                "/api/auth/login" => {
                    api::json::<AuthUserResponse, _>(
                        "POST",
                        endpoint,
                        Some(&LoginRequest {
                            email,
                            password: secret,
                            device_id,
                            device_name: Some("Inkstone browser".into()),
                        }),
                    )
                    .await
                }
                "/api/auth/register" => {
                    let Some(invite) = pending_invite else {
                        return;
                    };
                    api::json::<AuthUserResponse, _>(
                        "POST",
                        endpoint,
                        Some(&InvitationRegisterRequest {
                            registration,
                            invitation_token: invite.token,
                        }),
                    )
                    .await
                }
                _ => return,
            };
            match result {
                Ok(user) => this.bootstrap_authenticated(user, Some(password)).await,
                Err(error) => {
                    let mut inner = this.0.borrow_mut();
                    inner.auth_phase = AuthPhase::SignedOut;
                    inner.auth_error = Some(error.message);
                    drop(inner);
                    this.render();
                }
            }
        });
    }

    fn account_token_action(&self) {
        let Some(flow) = self.0.borrow().account_token_flow.clone() else {
            return;
        };
        let read = |selector: &str| {
            web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| document.query_selector(selector).ok().flatten())
                .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
                .map(|input| input.value())
                .unwrap_or_default()
        };
        let password = read(".reset-password");
        let email = read(".reset-email");
        let secret = if matches!(flow, AccountTokenFlow::PasswordReset(_)) {
            if email.trim().is_empty() {
                self.0.borrow_mut().auth_error = Some("Enter your account email".into());
                self.render();
                return;
            }
            match crypto::check_new_password(&password)
                .and_then(|()| crypto::login_secret(&password, &email))
            {
                Ok(secret) => secret,
                Err(message) => {
                    self.0.borrow_mut().auth_error = Some(message);
                    self.render();
                    return;
                }
            }
        } else {
            String::new()
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = match flow {
                AccountTokenFlow::VerifyEmail(token) => {
                    api::json::<GenericMessageResponse, _>(
                        "POST",
                        "/api/auth/email-verification/consume",
                        Some(&ConsumeEmailVerificationRequest { token }),
                    )
                    .await
                }
                AccountTokenFlow::PasswordReset(token) => {
                    api::json::<GenericMessageResponse, _>(
                        "POST",
                        "/api/auth/password-reset/consume",
                        Some(&ConsumePasswordResetRequest {
                            token,
                            email: Some(email),
                            password: secret,
                        }),
                    )
                    .await
                }
            };
            let mut inner = this.0.borrow_mut();
            inner.account_token_flow = None;
            match result {
                Ok(response) => inner.auth_notice = Some(response.message),
                Err(error) => inner.auth_error = Some(error.message),
            }
            drop(inner);
            this.render();
        });
    }

    fn request_password_reset(&self) {
        let email = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(".auth-email").ok().flatten())
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
            .map(|input| input.value())
            .unwrap_or_default();
        if email.trim().is_empty() {
            self.0.borrow_mut().auth_error = Some("Enter your email address first".into());
            self.render();
            return;
        }
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::json::<GenericMessageResponse, _>(
                "POST",
                "/api/auth/password-reset/request",
                Some(&TokenRequest { email }),
            )
            .await
            {
                Ok(response) => this.0.borrow_mut().auth_notice = Some(response.message),
                Err(error) => this.0.borrow_mut().auth_error = Some(error.message),
            }
            this.render();
        });
    }

    fn request_current_account_email(&self, verification: bool) {
        let Some(email) = self
            .0
            .borrow()
            .auth_user
            .as_ref()
            .map(|user| user.email.clone())
        else {
            return;
        };
        let endpoint = if verification {
            "/api/auth/email-verification/request"
        } else {
            "/api/auth/password-reset/request"
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match api::json::<GenericMessageResponse, _>(
                "POST",
                endpoint,
                Some(&TokenRequest { email }),
            )
            .await
            {
                Ok(response) => this.0.borrow_mut().auth_notice = Some(response.message),
                Err(error) => this.0.borrow_mut().auth_error = Some(error.message),
            }
            this.render();
        });
    }

    fn recover_account(&self) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let value = |selector: &str| {
            document
                .query_selector(selector)
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
                .map(|input| input.value())
                .unwrap_or_default()
        };
        let recovery = value(".recovery-key-input");
        let typed = value(".recovery-password");
        let Some(user) = self.0.borrow().auth_user.clone() else {
            return;
        };
        let Some(password) = self
            .0
            .borrow()
            .pending_password
            .clone()
            .or((!typed.is_empty()).then_some(typed))
        else {
            self.0.borrow_mut().auth_error = Some("Enter your password too".into());
            self.render();
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result: Result<(), String> = async {
                let recovery_key = crypto::recovery_key_from_string(&recovery)?;
                let envelopes = api::json::<Vec<RecoveryEnvelopeResponse>, serde_json::Value>(
                    "GET",
                    "/api/auth/recovery-envelopes",
                    None,
                )
                .await
                .map_err(|error| error.message)?;
                let recovery_envelope =
                    envelopes
                        .iter()
                        .find(|item| item.kind == "recovery_key")
                        .ok_or_else(|| "No recovery envelope is configured".to_string())?;
                let vault_key: [u8; 32] = crypto::decrypt(
                    &recovery_key,
                    &recovery_envelope.ciphertext_b64,
                    &recovery_envelope.nonce_b64,
                    &crypto::account_vault_aad(user.id, "recovery_key"),
                )
                .map_err(|_| "That recovery key doesn't match this account".to_string())?
                .try_into()
                .map_err(|_| "Recovery envelope has an invalid key length".to_string())?;
                this.rewrap_with_password(&user, &envelopes, &password, &password, &vault_key)
                    .await?;
                {
                    let mut inner = this.0.borrow_mut();
                    inner.account_vault_key = Some(vault_key);
                    inner.pending_password = None;
                }
                this.bootstrap_authenticated_inner(&user, None).await
            }
            .await;
            if let Err(error) = result {
                this.0.borrow_mut().auth_error = Some(error);
                this.render();
            }
        });
    }

    /// Wraps the vault key with `new_password` and makes it the sign-in
    /// password, in one step on the server.
    async fn rewrap_with_password(
        &self,
        user: &AuthUserResponse,
        envelopes: &[RecoveryEnvelopeResponse],
        current_password: &str,
        new_password: &str,
        vault_key: &[u8; 32],
    ) -> Result<(), String> {
        let expected_version = envelopes
            .iter()
            .find(|item| item.kind == "passphrase")
            .map(|item| item.version);
        let encrypted = crypto::encrypt(
            &crypto::derive_account_kek(new_password, user.id)?,
            vault_key,
            &crypto::account_vault_aad(user.id, "passphrase"),
        )?;
        api::json::<GenericMessageResponse, _>(
            "POST",
            "/api/auth/password",
            Some(&ChangePasswordRequest {
                current_password: crypto::login_secret(current_password, &user.email)?,
                new_password: crypto::login_secret(new_password, &user.email)?,
                passphrase_envelope: RecoveryEnvelopeRequest {
                    kind: "passphrase".into(),
                    expected_version,
                    ciphertext_b64: encrypted.ciphertext_b64,
                    nonce_b64: encrypted.nonce_b64,
                    aad_version: crypto::AAD_VERSION,
                    wrapping_version: crypto::WRAPPING_VERSION,
                },
            }),
        )
        .await
        .map_err(|error| error.message)?;
        Ok(())
    }

    fn copy_recovery_key(&self) {
        let Some(value) = self.0.borrow().recovery_key_display.clone() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(navigator) = js_sys::Reflect::get(window.as_ref(), &JsValue::from_str("navigator"))
        else {
            return;
        };
        let Ok(clipboard) = js_sys::Reflect::get(&navigator, &JsValue::from_str("clipboard"))
        else {
            return;
        };
        let Ok(write) = js_sys::Reflect::get(&clipboard, &JsValue::from_str("writeText")) else {
            return;
        };
        if let Ok(function) = write.dyn_into::<js_sys::Function>() {
            let _ = function.call1(&clipboard, &JsValue::from_str(&value));
            self.0.borrow_mut().auth_notice = Some("Recovery key copied".into());
            self.render();
        }
    }

    fn change_password(&self) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let read = |selector: &str| {
            document
                .query_selector(selector)
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
                .map(|input| input.value())
                .unwrap_or_default()
        };
        let current = read(".current-password");
        let new_password = read(".new-password");
        let problem = if current.is_empty() {
            Some("Enter your current password".to_string())
        } else if new_password != read(".new-password-confirm") {
            Some("New passwords don't match".to_string())
        } else {
            crypto::check_new_password(&new_password).err()
        };
        if let Some(problem) = problem {
            self.0.borrow_mut().auth_error = Some(problem);
            self.render();
            return;
        }
        let (Some(user), Some(vault_key)) = ({
            let inner = self.0.borrow();
            (inner.auth_user.clone(), inner.account_vault_key)
        }) else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result: Result<(), String> = async {
                let envelopes = api::json::<Vec<RecoveryEnvelopeResponse>, serde_json::Value>(
                    "GET",
                    "/api/auth/recovery-envelopes",
                    None,
                )
                .await
                .map_err(|error| error.message)?;
                this.rewrap_with_password(&user, &envelopes, &current, &new_password, &vault_key)
                    .await
            }
            .await;
            match result {
                Ok(()) => {
                    if let Some(inputs) = web_sys::window()
                        .and_then(|window| window.document())
                        .and_then(|document| {
                            document
                                .query_selector_all(".settings-dialog input[type=password]")
                                .ok()
                        })
                    {
                        for index in 0..inputs.length() {
                            if let Some(input) = inputs
                                .item(index)
                                .and_then(|node| node.dyn_into::<web_sys::HtmlInputElement>().ok())
                            {
                                input.set_value("");
                            }
                        }
                    }
                    let mut inner = this.0.borrow_mut();
                    inner.auth_error = None;
                    inner.auth_notice = Some("Password changed.".into());
                }
                Err(error) => this.0.borrow_mut().auth_error = Some(error),
            }
            this.render();
        });
    }

    fn complete_owner_setup(&self) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let input_value = |selector: &str| {
            document
                .query_selector(selector)
                .ok()
                .flatten()
                .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
                .map(|input| input.value())
                .unwrap_or_default()
        };
        let email = input_value(".owner-email");
        let password = input_value(".owner-password");
        let password_confirmation = input_value(".owner-password-confirmation");
        let problem = if email.trim().is_empty() {
            Some("Enter your email".to_string())
        } else if password != password_confirmation {
            Some("Passwords don't match".to_string())
        } else {
            crypto::check_new_password(&password).err()
        };
        if let Some(problem) = problem {
            self.0.borrow_mut().auth_error = Some(problem);
            self.render();
            return;
        }
        if self.0.borrow().recovery_key_display.is_none() {
            let (Ok(vault_key), Ok(recovery_key)) = (crypto::random_key(), crypto::random_key())
            else {
                self.0.borrow_mut().auth_error = Some("Secure randomness is unavailable".into());
                self.render();
                return;
            };
            let mut inner = self.0.borrow_mut();
            inner.account_vault_key = Some(vault_key);
            inner.recovery_key_display = Some(crypto::recovery_key_to_string(&recovery_key));
            inner.new_account_setup_pending = true;
            inner.auth_error =
                Some("Save your recovery key somewhere safe, then confirm below.".into());
            drop(inner);
            self.render();
            return;
        }
        if !recovery_confirmation_checked(&document) {
            self.0.borrow_mut().auth_error =
                Some("Confirm you've saved your recovery key first".into());
            self.render();
            return;
        }
        let (Some(token), Some(device_id)) = ({
            let inner = self.0.borrow();
            (inner.owner_setup_token.clone(), inner.device_id)
        }) else {
            self.0.borrow_mut().auth_error = Some("Owner setup link is unavailable".into());
            self.render();
            return;
        };
        {
            let mut inner = self.0.borrow_mut();
            inner.owner_setup_in_flight = true;
            inner.auth_error = None;
        }
        self.render();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let secret = match crypto::login_secret(&password, &email) {
                Ok(secret) => secret,
                Err(message) => {
                    let mut inner = this.0.borrow_mut();
                    inner.owner_setup_in_flight = false;
                    inner.auth_error = Some(message);
                    drop(inner);
                    this.render();
                    return;
                }
            };
            match api::json::<AuthUserResponse, _>(
                "POST",
                "/api/auth/owner-setup",
                Some(&OwnerSetupRequest {
                    token,
                    email: Some(email),
                    password: secret,
                    device_id,
                    device_name: Some("Inkstone browser".into()),
                }),
            )
            .await
            {
                Ok(user) => {
                    {
                        let mut inner = this.0.borrow_mut();
                        inner.owner_setup_token = None;
                        inner.owner_setup_in_flight = false;
                    }
                    this.bootstrap_authenticated(user, Some(password)).await;
                }
                Err(error) => {
                    let mut inner = this.0.borrow_mut();
                    inner.owner_setup_in_flight = false;
                    inner.auth_error = Some(error.message);
                    drop(inner);
                    this.render();
                }
            }
        });
    }

    fn unlock_workspace(&self) {
        let password = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(".unlock-password").ok().flatten())
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
            .map(|input| input.value())
            .unwrap_or_default();
        if password.is_empty() {
            self.0.borrow_mut().auth_error = Some("Enter your password".into());
            self.render();
            return;
        }
        let Some(user) = self.0.borrow().auth_user.clone() else {
            return;
        };
        self.0.borrow_mut().auth_phase = AuthPhase::Checking;
        self.0.borrow_mut().auth_error = None;
        self.render();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            this.bootstrap_authenticated(user, Some(password)).await;
        });
    }

    async fn bootstrap_authenticated(&self, user: AuthUserResponse, password: Option<String>) {
        self.stop_discovery_timer();
        if let Some(workspace_db) = self.0.borrow().workspace_db.as_ref() {
            workspace_db.clear_scope();
        }
        {
            let mut inner = self.0.borrow_mut();
            inner.auth_user = Some(user.clone());
            inner.device_id = Some(user.device_id);
        }
        let workspace_db = { self.0.borrow().workspace_db.clone() };
        if let Some(workspace_db) = workspace_db {
            if let Err(error) = workspace_db
                .set_global_metadata(DEVICE_ID_KEY, &user.device_id.to_string())
                .await
            {
                self.0.borrow_mut().auth_error =
                    Some(format!("Failed to persist session device ID: {error:?}"));
            }
        }
        let result = self
            .bootstrap_authenticated_inner(&user, password.as_deref())
            .await;
        if let Err(message) = result {
            let mut inner = self.0.borrow_mut();
            inner.pending_password = password;
            inner.auth_phase = if inner.auth_user.is_some() {
                AuthPhase::UnlockRequired
            } else {
                AuthPhase::SignedOut
            };
            inner.auth_error = Some(message);
            drop(inner);
            self.render();
        }
    }

    async fn bootstrap_authenticated_inner(
        &self,
        user: &AuthUserResponse,
        password: Option<&str>,
    ) -> Result<(), String> {
        let workspace_db = self
            .0
            .borrow()
            .workspace_db
            .clone()
            .ok_or_else(|| "Local workspace storage is unavailable".to_string())?;

        let new_account_setup = self.0.borrow().new_account_setup_pending;
        let vault_key = if new_account_setup {
            let vault_key = self
                .0
                .borrow()
                .account_vault_key
                .ok_or_else(|| "New account vault key is unavailable".to_string())?;
            let displayed = self.0.borrow().recovery_key_display.clone();
            if let (Some(passphrase), Some(displayed)) = (password, displayed) {
                let recovery_key = crypto::recovery_key_from_string(&displayed)?;
                self.ensure_new_account_envelopes(
                    user.id,
                    &crypto::derive_account_kek(passphrase, user.id)?,
                    &recovery_key,
                    &vault_key,
                )
                .await?;
                workspace_db
                    .set_global_metadata(ACCOUNT_VAULT_KEY, "configured-v1")
                    .await
                    .map_err(|error| format!("Failed to persist vault status: {error:?}"))?;
            }
            self.0.borrow_mut().new_account_setup_pending = false;
            vault_key
        } else if let Some(vault_key) = self.0.borrow().account_vault_key {
            vault_key
        } else if let Some(password) = password {
            self.unlock_account_vault(user.id, password).await?
        } else if let Some(vault_key) = remembered_vault_key(&workspace_db, user.id).await {
            vault_key
        } else {
            self.0.borrow_mut().auth_phase = AuthPhase::UnlockRequired;
            self.render();
            return Ok(());
        };
        self.0.borrow_mut().account_vault_key = Some(vault_key);
        self.0.borrow_mut().pending_password = None;
        let remembered = format!(
            "{}:{}",
            user.id,
            base64::engine::general_purpose::STANDARD.encode(vault_key)
        );
        if let Err(error) = workspace_db
            .set_global_metadata(REMEMBERED_VAULT_KEY, &remembered)
            .await
        {
            web_sys::console::warn_1(&format!("Could not remember the unlock: {error:?}").into());
        }
        self.ensure_device_key(user, &workspace_db, &vault_key)
            .await?;

        let pending_invite = self.0.borrow().pending_invite.clone();
        let mut awaiting_workspace = parse_awaiting_workspace(
            workspace_db
                .get_global_metadata(AWAITING_INVITATION_KEY)
                .await
                .map_err(|error| format!("Failed to read invitation status: {error:?}"))?,
        );
        if let Some(invite) = &pending_invite {
            awaiting_workspace = Some(
                self.submit_invitation_key_request(user, &workspace_db, &vault_key, invite)
                    .await?,
            );
            clear_invite_fragment();
            self.0.borrow_mut().pending_invite = None;
        }

        let mut workspaces = api::json::<PaginatedResponse<WorkspaceResponse>, serde_json::Value>(
            "GET",
            "/api/workspaces",
            None,
        )
        .await
        .map_err(|error| error.message)?
        .items;

        if let Some(workspace_id) = awaiting_workspace {
            if workspaces
                .iter()
                .any(|workspace| workspace.id == workspace_id)
            {
                let _ = workspace_db
                    .delete_global_metadata(AWAITING_INVITATION_KEY)
                    .await;
                awaiting_workspace = None;
            }
        }

        if workspaces.is_empty() {
            if awaiting_workspace.is_some() {
                let mut inner = self.0.borrow_mut();
                inner.workspaces.clear();
                inner.auth_phase = AuthPhase::AwaitingKey;
                inner.auth_error = None;
                inner.recovery_key_display = None;
                drop(inner);
                self.render();
                return Ok(());
            }
            let workspace_id = uuid::Uuid::new_v4();
            let mut key = [0_u8; 32];
            getrandom::getrandom(&mut key).map_err(|error| error.to_string())?;
            let encrypted_metadata = crypto::encrypt(
                &key,
                &serde_json::to_vec(&WorkspaceMetadata {
                    name: "My vault".into(),
                    description: String::new(),
                    public_link: None,
                })
                .map_err(|error| error.to_string())?,
                &crypto::workspace_metadata_aad(workspace_id),
            )?;
            let workspace = api::json::<WorkspaceResponse, _>(
                "POST",
                "/api/workspaces",
                Some(&CreateWorkspaceRequest {
                    workspace_id,
                    encrypted_metadata_b64: encrypted_metadata.ciphertext_b64,
                    metadata_nonce_b64: encrypted_metadata.nonce_b64,
                }),
            )
            .await
            .map_err(|error| error.message)?;
            let local_envelope = crypto::encrypt(
                &vault_key,
                &key,
                &crypto::workspace_key_aad(workspace.id, user.id),
            )?;
            workspace_db
                .set_global_metadata(
                    &workspace_key_metadata(workspace.id),
                    &serde_json::to_string(&local_envelope).map_err(|error| error.to_string())?,
                )
                .await
                .map_err(|error| format!("Failed to store workspace key: {error:?}"))?;
            self.put_key_envelope(&workspace, user, &vault_key, &key)
                .await?;
            workspaces.push(workspace);
        }

        let stored_preference = workspace_db
            .get_global_metadata(SELECTED_WORKSPACE_KEY)
            .await
            .map_err(|error| format!("Failed to read selected workspace: {error:?}"))?
            .and_then(|value| uuid::Uuid::parse_str(&value).ok());
        let remembered = self.0.borrow().workspace_id.or(stored_preference);
        let find = |id: uuid::Uuid| workspaces.iter().find(|workspace| workspace.id == id);
        let workspace = match awaiting_workspace {
            // An invitation names its workspace; anything else is wrong.
            Some(id) => find(id)
                .cloned()
                .ok_or_else(|| "The invited workspace is not available yet".to_string())?,
            // The last workspace used may since have been deleted or left:
            // open another rather than locking the user out.
            None => remembered
                .and_then(find)
                .or_else(|| workspaces.first())
                .cloned()
                .ok_or_else(|| "No workspace is available".to_string())?,
        };
        self.0.borrow_mut().workspaces = workspaces;
        self.activate_workspace(workspace, user, Some(&vault_key))
            .await
    }

    async fn put_account_envelope(
        &self,
        user_id: uuid::Uuid,
        kind: &str,
        wrapping_key: &[u8; 32],
        vault_key: &[u8; 32],
        expected_version: Option<i64>,
    ) -> Result<(), String> {
        let encrypted = crypto::encrypt(
            wrapping_key,
            vault_key,
            &crypto::account_vault_aad(user_id, kind),
        )?;
        api::json::<RecoveryEnvelopeResponse, _>(
            "PUT",
            "/api/auth/recovery-envelopes",
            Some(&RecoveryEnvelopeRequest {
                kind: kind.into(),
                expected_version,
                ciphertext_b64: encrypted.ciphertext_b64,
                nonce_b64: encrypted.nonce_b64,
                aad_version: crypto::AAD_VERSION,
                wrapping_version: crypto::WRAPPING_VERSION,
            }),
        )
        .await
        .map_err(|error| error.message)?;
        Ok(())
    }

    async fn ensure_new_account_envelopes(
        &self,
        user_id: uuid::Uuid,
        passphrase_key: &[u8; 32],
        recovery_key: &[u8; 32],
        vault_key: &[u8; 32],
    ) -> Result<(), String> {
        let existing = api::json::<Vec<RecoveryEnvelopeResponse>, serde_json::Value>(
            "GET",
            "/api/auth/recovery-envelopes",
            None,
        )
        .await
        .map_err(|error| error.message)?;
        for (kind, wrapping_key) in [
            ("passphrase", passphrase_key),
            ("recovery_key", recovery_key),
        ] {
            if let Some(envelope) = existing.iter().find(|item| item.kind == kind) {
                let unwrapped = crypto::decrypt(
                    wrapping_key,
                    &envelope.ciphertext_b64,
                    &envelope.nonce_b64,
                    &crypto::account_vault_aad(user_id, kind),
                )?;
                if unwrapped.as_slice() != vault_key {
                    return Err(format!(
                        "Existing {kind} envelope does not match this account setup"
                    ));
                }
            } else {
                self.put_account_envelope(user_id, kind, wrapping_key, vault_key, None)
                    .await?;
            }
        }
        Ok(())
    }

    async fn unlock_account_vault(
        &self,
        user_id: uuid::Uuid,
        passphrase: &str,
    ) -> Result<[u8; 32], String> {
        let envelopes = api::json::<Vec<RecoveryEnvelopeResponse>, serde_json::Value>(
            "GET",
            "/api/auth/recovery-envelopes",
            None,
        )
        .await
        .map_err(|error| error.message)?;
        let envelope = envelopes
            .into_iter()
            .find(|envelope| envelope.kind == "passphrase")
            .ok_or_else(|| "This account has no password-locked vault yet".to_string())?;
        if envelope.aad_version != crypto::AAD_VERSION
            || envelope.wrapping_version != crypto::WRAPPING_VERSION
        {
            return Err("Unsupported account vault envelope version".into());
        }
        crypto::decrypt(
            &crypto::derive_account_kek(passphrase, user_id)?,
            &envelope.ciphertext_b64,
            &envelope.nonce_b64,
            &crypto::account_vault_aad(user_id, "passphrase"),
        )
        .map_err(|_| {
            "That password didn't unlock your notes. If you reset it, use your recovery key."
                .to_string()
        })?
        .try_into()
        .map_err(|_| "Account vault envelope has an invalid key length".into())
    }

    async fn ensure_device_key(
        &self,
        user: &AuthUserResponse,
        workspace_db: &WorkspaceDb,
        vault_key: &[u8; 32],
    ) -> Result<[u8; 32], String> {
        if let Some(value) = workspace_db
            .get_global_metadata(DEVICE_PRIVATE_KEY)
            .await
            .map_err(|error| format!("Failed to read the device key: {error:?}"))?
        {
            // A key that this account's vault cannot open was left by an
            // account that is gone (signed in elsewhere, reset, recreated)
            // without signing out here. It only ever received invitation keys
            // for that account, so this one gets a fresh key instead.
            let private = serde_json::from_str::<crypto::Ciphertext>(&value)
                .ok()
                .and_then(|envelope| {
                    crypto::decrypt(
                        vault_key,
                        &envelope.ciphertext_b64,
                        &envelope.nonce_b64,
                        &crypto::device_private_key_aad(user.id, user.device_id),
                    )
                    .ok()
                })
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
            if let Some(private) = private {
                return Ok(private);
            }
        }
        let (private, _) = crypto::generate_device_keypair()?;
        let envelope = crypto::encrypt(
            vault_key,
            &private,
            &crypto::device_private_key_aad(user.id, user.device_id),
        )?;
        workspace_db
            .set_global_metadata(
                DEVICE_PRIVATE_KEY,
                &serde_json::to_string(&envelope).map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| format!("Failed to protect the device private key: {error:?}"))?;
        Ok(private)
    }

    async fn submit_invitation_key_request(
        &self,
        user: &AuthUserResponse,
        workspace_db: &WorkspaceDb,
        vault_key: &[u8; 32],
        invite: &InviteFragment,
    ) -> Result<uuid::Uuid, String> {
        let private = self
            .ensure_device_key(user, workspace_db, vault_key)
            .await?;
        let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private));
        let incoming = api::json::<PaginatedResponse<InvitationResponse>, serde_json::Value>(
            "GET",
            "/api/invitations",
            None,
        )
        .await
        .map_err(|error| error.message)?;
        let mut last_error = "No pending invitation matches this account".to_string();
        for invitation in incoming.items {
            match api::json::<InvitationKeyRequestResponse, _>(
                "POST",
                "/api/invitations/key-request",
                Some(&SubmitInvitationKeyRequest {
                    token: invite.token.clone(),
                    workspace_id: invitation.workspace_id,
                    public_key_b64: base64::engine::general_purpose::STANDARD
                        .encode(public.as_bytes()),
                    key_version: 1,
                }),
            )
            .await
            {
                Ok(request) => {
                    workspace_db
                        .set_global_metadata(
                            AWAITING_INVITATION_KEY,
                            &request.workspace_id.to_string(),
                        )
                        .await
                        .map_err(|error| {
                            format!("Failed to save awaiting-key status: {error:?}")
                        })?;
                    return Ok(request.workspace_id);
                }
                Err(error) if matches!(error.status, 403 | 404) => last_error = error.message,
                Err(error) => return Err(error.message),
            }
        }
        Err(last_error)
    }

    async fn put_key_envelope(
        &self,
        workspace: &WorkspaceResponse,
        user: &AuthUserResponse,
        vault_key: &[u8; 32],
        key: &[u8; 32],
    ) -> Result<(), String> {
        let encrypted = crypto::encrypt(
            vault_key,
            key,
            &crypto::workspace_key_aad(workspace.id, user.id),
        )?;
        let path = format!("/api/workspaces/{}/key-envelope", workspace.id);
        api::json::<WorkspaceKeyEnvelopeResponse, _>(
            "PUT",
            &path,
            Some(&PutWorkspaceKeyEnvelopeRequest {
                encrypted_key_b64: encrypted.ciphertext_b64,
                nonce_b64: encrypted.nonce_b64,
                aad_version: crypto::AAD_VERSION,
                replace_version: None,
            }),
        )
        .await
        .map_err(|error| error.message)?;
        self.0.borrow_mut().needs_key_envelope = false;
        Ok(())
    }

    async fn share_key_across_devices(
        &self,
        workspace: &WorkspaceResponse,
        user: &AuthUserResponse,
        vault_key: &[u8; 32],
        key: &[u8; 32],
        envelope: &WorkspaceKeyEnvelopeResponse,
    ) {
        let aad = crypto::workspace_key_aad(workspace.id, user.id);
        if crypto::decrypt(
            vault_key,
            &envelope.encrypted_key_b64,
            &envelope.nonce_b64,
            &aad,
        )
        .is_ok()
        {
            return;
        }
        let Ok(encrypted) = crypto::encrypt(vault_key, key, &aad) else {
            return;
        };
        let path = format!("/api/workspaces/{}/key-envelope", workspace.id);
        if let Err(error) = api::json::<WorkspaceKeyEnvelopeResponse, _>(
            "PUT",
            &path,
            Some(&PutWorkspaceKeyEnvelopeRequest {
                encrypted_key_b64: encrypted.ciphertext_b64,
                nonce_b64: encrypted.nonce_b64,
                aad_version: crypto::AAD_VERSION,
                replace_version: Some(envelope.version),
            }),
        )
        .await
        {
            web_sys::console::warn_1(
                &format!("Could not share the vault key: {}", error.message).into(),
            );
        }
    }

    async fn activate_workspace(
        &self,
        workspace: WorkspaceResponse,
        user: &AuthUserResponse,
        vault_key: Option<&[u8; 32]>,
    ) -> Result<(), String> {
        self.stop_discovery_timer();
        let workspace_db = self
            .0
            .borrow()
            .workspace_db
            .clone()
            .ok_or_else(|| "Local workspace storage is unavailable".to_string())?;
        workspace_db.clear_scope();
        let local_key = workspace_db
            .get_global_metadata(&workspace_key_metadata(workspace.id))
            .await
            .map_err(|error| format!("Failed to read workspace key: {error:?}"))?
            .map(|value| {
                let vault_key = vault_key.ok_or_else(|| "Account vault is locked".to_string())?;
                let envelope: crypto::Ciphertext = serde_json::from_str(&value)
                    .map_err(|_| "Invalid local workspace key envelope".to_string())?;
                crypto::decrypt(
                    vault_key,
                    &envelope.ciphertext_b64,
                    &envelope.nonce_b64,
                    &crypto::workspace_key_aad(workspace.id, user.id),
                )?
                .try_into()
                .map_err(|_| "Local workspace key envelope has an invalid key length".to_string())
            })
            .transpose()?;
        let envelope_path = format!("/api/workspaces/{}/key-envelope", workspace.id);
        let envelope = api::json::<WorkspaceKeyEnvelopeResponse, serde_json::Value>(
            "GET",
            &envelope_path,
            None,
        )
        .await;
        let key = match (local_key, envelope) {
            (Some(key), Ok(envelope)) => {
                if self.0.borrow().needs_key_envelope {
                    let Some(vault_key) = vault_key else {
                        let mut inner = self.0.borrow_mut();
                        inner.workspace_id = Some(workspace.id);
                        inner.workspace_name = "Locked vault".into();
                        inner.auth_phase = AuthPhase::UnlockRequired;
                        inner.auth_error = None;
                        drop(inner);
                        self.render();
                        return Ok(());
                    };
                    if envelope.aad_version != crypto::AAD_VERSION {
                        return Err("Unsupported workspace key envelope version".into());
                    }
                    let stored_key: [u8; 32] = crypto::decrypt(
                        vault_key,
                        &envelope.encrypted_key_b64,
                        &envelope.nonce_b64,
                        &crypto::workspace_key_aad(workspace.id, user.id),
                    )?
                    .try_into()
                    .map_err(|_| "Workspace key envelope has an invalid key length".to_string())?;
                    if stored_key != key {
                        let _ = workspace_db
                            .delete_global_metadata(&workspace_key_metadata(workspace.id))
                            .await;
                        clear_invite_fragment();
                        let mut inner = self.0.borrow_mut();
                        inner.pending_invite = None;
                        inner.needs_key_envelope = false;
                        return Err("Invitation key does not match this workspace".into());
                    }
                    self.0.borrow_mut().needs_key_envelope = false;
                }
                if let Some(vault_key) = vault_key {
                    self.share_key_across_devices(&workspace, user, vault_key, &key, &envelope)
                        .await;
                }
                key
            }
            (Some(key), Err(error)) if error.status == 404 => {
                let Some(vault_key) = vault_key else {
                    let mut inner = self.0.borrow_mut();
                    inner.workspace_id = Some(workspace.id);
                    inner.workspace_name = "Locked vault".into();
                    inner.workspace_key = Some(key);
                    inner.auth_phase = AuthPhase::UnlockRequired;
                    inner.auth_error = None;
                    drop(inner);
                    self.render();
                    return Ok(());
                };
                self.put_key_envelope(&workspace, user, vault_key, &key)
                    .await?;
                key
            }
            (Some(_), Err(error)) => return Err(error.message),
            (None, Ok(envelope)) => {
                let Some(vault_key) = vault_key else {
                    let mut inner = self.0.borrow_mut();
                    inner.workspace_id = Some(workspace.id);
                    inner.workspace_name = "Locked vault".into();
                    inner.auth_phase = AuthPhase::UnlockRequired;
                    inner.auth_error = None;
                    drop(inner);
                    self.render();
                    return Ok(());
                };
                if envelope.aad_version != crypto::AAD_VERSION {
                    return Err("Unsupported workspace key envelope version".into());
                }
                let plaintext = crypto::decrypt(
                    vault_key,
                    &envelope.encrypted_key_b64,
                    &envelope.nonce_b64,
                    &crypto::workspace_key_aad(workspace.id, user.id),
                );
                let key: [u8; 32] = match plaintext {
                    Ok(plaintext) => plaintext.try_into().map_err(|_| {
                        "Workspace key envelope has an invalid key length".to_string()
                    })?,
                    Err(_) => {
                        let private = self
                            .ensure_device_key(user, &workspace_db, vault_key)
                            .await?;
                        crypto::open_workspace_key_for_device(
                            &crypto::Ciphertext {
                                ciphertext_b64: envelope.encrypted_key_b64.clone(),
                                nonce_b64: envelope.nonce_b64.clone(),
                            },
                            &private,
                            workspace.id,
                            user.id,
                        )
                        .map_err(|_| {
                            "This device doesn't have the key to this vault yet. Open Inkstone once on the device where you accepted the invite, then sign in here again.".to_string()
                        })?
                    }
                };
                self.share_key_across_devices(&workspace, user, vault_key, &key, &envelope)
                    .await;
                let local_envelope = crypto::encrypt(
                    vault_key,
                    &key,
                    &crypto::workspace_key_aad(workspace.id, user.id),
                )?;
                workspace_db
                    .set_global_metadata(
                        &workspace_key_metadata(workspace.id),
                        &serde_json::to_string(&local_envelope)
                            .map_err(|error| error.to_string())?,
                    )
                    .await
                    .map_err(|error| format!("Failed to store workspace key: {error:?}"))?;
                key
            }
            (None, Err(error)) if error.status == 404 => {
                return Err("This workspace has no encrypted key envelope".into());
            }
            (None, Err(error)) => return Err(error.message),
        };

        if self.0.borrow().needs_key_envelope {
            let Some(vault_key) = vault_key else {
                let mut inner = self.0.borrow_mut();
                inner.workspace_id = Some(workspace.id);
                inner.workspace_name = "Locked vault".into();
                inner.workspace_key = Some(key);
                inner.auth_phase = AuthPhase::UnlockRequired;
                inner.auth_error = None;
                drop(inner);
                self.render();
                return Ok(());
            };
            self.put_key_envelope(&workspace, user, vault_key, &key)
                .await?;
        }

        self.enter_workspace(workspace, key).await
    }

    async fn enter_workspace(
        &self,
        workspace: WorkspaceResponse,
        key: [u8; 32],
    ) -> Result<(), String> {
        let (workspace_db, public_view) = {
            let inner = self.0.borrow();
            (
                inner
                    .workspace_db
                    .clone()
                    .ok_or_else(|| "Local workspace storage is unavailable".to_string())?,
                inner.public_view.is_some(),
            )
        };
        workspace_db
            .activate_workspace_scope(workspace.id)
            .await
            .map_err(|error| format!("Failed to activate workspace storage: {error:?}"))?;

        let ownership_key = workspace_ownership_metadata(workspace.id);
        let stored_ownership = workspace_db
            .get_metadata(&ownership_key)
            .await
            .map_err(|error| format!("Failed to read workspace ownership: {error:?}"))?;
        let ownership = match stored_ownership.as_deref() {
            Some(json) => serde_json::from_str::<HashMap<String, Option<DocId>>>(json)
                .map_err(|error| format!("Invalid workspace ownership metadata: {error}"))?,
            None => HashMap::new(),
        };
        let workspace_metadata = decrypt_workspace_metadata(&workspace, &key)?;
        let workspace_revision = {
            let mut inner = self.0.borrow_mut();
            inner.advance_workspace_revision();
            inner.workspace_id = Some(workspace.id);
            inner.workspace_name = workspace_metadata.name;
            inner.workspace_description = workspace_metadata.description;
            inner.workspace_public_link = workspace_metadata.public_link;
            let name = inner.workspace_name.clone();
            inner.vault_names.insert(workspace.id, name);
            inner.workspace_key = Some(key);
            inner.active_workspace_ownership = ownership;
            inner.remote_docs.clear();
            inner.workspace_revision
        };
        let recent_notes = workspace_db
            .get_recent_notes()
            .await
            .map_err(|error| format!("Failed to load recent notes: {error:?}"))?;
        let ownership_paths = self
            .0
            .borrow()
            .active_workspace_ownership
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        self.0.borrow_mut().recent_notes = recent_notes
            .into_iter()
            .filter(|path| ownership_paths.contains(path))
            .collect();
        if !public_view {
            workspace_db
                .set_global_metadata(SELECTED_WORKSPACE_KEY, &workspace.id.to_string())
                .await
                .map_err(|error| format!("Failed to preserve selected workspace: {error:?}"))?;
        }
        self.persist_active_workspace_ownership(workspace.id)
            .await?;
        self.discover_remote_docs(workspace.id, key, workspace_revision)
            .await?;
        let members = if public_view {
            let inner = self.0.borrow();
            let role = if inner.guest_name.is_some() && workspace.public_link_editable {
                "editor"
            } else {
                "viewer"
            };
            inner
                .auth_user
                .iter()
                .map(|user| WorkspaceMemberResponse {
                    user_id: user.id,
                    email: String::new(),
                    role: role.into(),
                    joined_at: workspace.created_at,
                    guest_name: None,
                })
                .collect()
        } else {
            let members_path = format!("/api/workspaces/{}/members", workspace.id);
            api::json::<PaginatedResponse<WorkspaceMemberResponse>, serde_json::Value>(
                "GET",
                &members_path,
                None,
            )
            .await
            .map(|response| response.items)
            .unwrap_or_default()
        };
        {
            let mut inner = self.0.borrow_mut();
            inner.members = members;
            inner.auth_phase = AuthPhase::Ready;
            inner.recovery_key_display = None;
            inner.new_account_setup_pending = false;
            inner.auth_error = None;
            inner.sync_error = None;
        }
        self.refresh_entries();
        self.start_discovery_timer();
        self.render();
        if !public_view {
            self.load_vault_names();
        }
        Ok(())
    }

    async fn persist_active_workspace_ownership(
        &self,
        workspace_id: uuid::Uuid,
    ) -> Result<(), String> {
        let (workspace_db, ownership) = {
            let inner = self.0.borrow();
            if inner.workspace_id != Some(workspace_id) {
                return Ok(());
            }
            (
                inner
                    .workspace_db
                    .clone()
                    .ok_or_else(|| "Local workspace storage is unavailable".to_string())?,
                inner.active_workspace_ownership.clone(),
            )
        };
        let json = serde_json::to_string(&ownership)
            .map_err(|error| format!("Failed to encode workspace ownership: {error}"))?;
        workspace_db
            .set_metadata(&workspace_ownership_metadata(workspace_id), &json)
            .await
            .map_err(|error| format!("Failed to persist workspace ownership: {error:?}"))
    }

    async fn associate_active_path(&self, path: &str, doc_id: Option<DocId>) -> Result<(), String> {
        let workspace_id = {
            let mut inner = self.0.borrow_mut();
            if inner.local_mode {
                return Ok(());
            }
            let workspace_id = inner
                .workspace_id
                .ok_or_else(|| "No active workspace".to_string())?;
            inner
                .active_workspace_ownership
                .insert(path.to_string(), doc_id);
            inner.advance_workspace_revision();
            workspace_id
        };
        self.persist_active_workspace_ownership(workspace_id).await
    }

    async fn refresh_inactive_document(
        &self,
        workspace_id: uuid::Uuid,
        key: [u8; 32],
        doc_id: DocId,
        remote_seq: Option<i64>,
        db: &IndexedDb,
    ) -> Result<bool, String> {
        if self.0.borrow().selected_doc_id == doc_id
            && self.0.borrow().client_state.phase() != SessionPhase::Closed
        {
            return Ok(false);
        }
        let storage_key = document_storage_key(Some(workspace_id), doc_id);
        let mut record = match db
            .get_document_record(&storage_key)
            .await
            .map_err(|error| format!("local record read failed: {error:?}"))?
        {
            Some(json) => storage::deserialize_record(&json)
                .map_err(|error| format!("local record is corrupt: {error}"))?,
            None => storage::new_record(doc_id),
        };
        if remote_seq.is_some_and(|seq| record.confirmed_seq >= seq) {
            return Ok(false);
        }
        let mut changed = false;
        let snapshot_endpoint = format!("/api/docs/{}/snapshot", doc_id.0);
        match api::json::<inkstone_core::protocol::SnapshotResponse, serde_json::Value>(
            "GET",
            &snapshot_endpoint,
            None,
        )
        .await
        {
            Ok(snapshot) if snapshot.base_seq > record.confirmed_seq => {
                if snapshot.aad_version != crypto::AAD_VERSION {
                    return Err("snapshot has an unsupported AAD version".into());
                }
                let plaintext = crypto::decrypt(
                    &key,
                    &snapshot.encrypted_snapshot_b64,
                    &snapshot.nonce_b64,
                    &crypto::snapshot_aad(workspace_id, doc_id.0, snapshot.base_seq),
                )?;
                let payload: storage::SyncedDocumentSnapshot =
                    serde_json::from_slice(&plaintext)
                        .map_err(|error| format!("decrypted snapshot is invalid: {error}"))?;
                if payload.version != storage::SYNCED_DOCUMENT_SNAPSHOT_VERSION
                    || payload.document.id != doc_id
                {
                    return Err("snapshot identity or version is invalid".into());
                }
                record.confirmed_doc = payload.document;
                record.confirmed_seq = snapshot.base_seq;
                record.text_snapshot = Some(payload.text_snapshot);
                changed = true;
            }
            Ok(_) => {}
            Err(error) if error.status == 404 => {}
            Err(error) => return Err(format!("snapshot download failed: {}", error.message)),
        }

        let replica_id = text_replica_id(self.0.borrow().device_id, doc_id);
        let mut state = TextClientState::new(record.confirmed_doc.clone(), replica_id)
            .map_err(|error| error.to_string())?;
        let hydration_effects = state.transition(TextClientEvent::Hydrate {
            document: record.confirmed_doc.clone(),
            snapshot: record.text_snapshot.clone(),
            pending_updates: record
                .pending_updates
                .iter()
                .filter(|update| matches!(update.op, DocumentOp::ApplyTextUpdateV2 { .. }))
                .cloned()
                .collect(),
        });
        if let Some(error) = hydration_effects
            .into_iter()
            .find_map(|effect| match effect {
                TextClientEffect::NotifyError(error) => Some(error),
                _ => None,
            })
        {
            return Err(format!("CRDT hydration failed: {error}"));
        }

        loop {
            let endpoint = format!(
                "/api/docs/{}/updates?after_seq={}&limit=500",
                doc_id.0, record.confirmed_seq
            );
            let page = api::json::<PaginatedResponse<EncryptedUpdateResponse>, serde_json::Value>(
                "GET", &endpoint, None,
            )
            .await
            .map_err(|error| format!("tail download failed: {}", error.message))?;
            if page.items.is_empty() {
                break;
            }
            let page_len = page.items.len();
            for encrypted in page.items {
                if encrypted.seq != record.confirmed_seq + 1
                    || encrypted.aad_version != crypto::AAD_VERSION
                {
                    return Err("snapshot tail is non-contiguous or has unsupported AAD".into());
                }
                let plaintext = crypto::decrypt(
                    &key,
                    &encrypted.encrypted_update_b64,
                    &encrypted.nonce_b64,
                    &crypto::update_aad(workspace_id, doc_id.0, encrypted.update_id.0),
                )?;
                let update: DocumentUpdate = serde_json::from_slice(&plaintext)
                    .map_err(|error| format!("decrypted tail update is invalid: {error}"))?;
                if update.doc_id != doc_id || update.update_id != encrypted.update_id {
                    return Err("decrypted tail update identity mismatch".into());
                }
                storage::note_edit(&mut record, update.applied_at_ms);
                if !matches!(update.op, DocumentOp::ApplyTextUpdateV2 { .. }) {
                    // Blocks, ink and moves replay on the document itself;
                    // only text goes through the CRDT, which then learns
                    // the new structure.
                    let mut document = record.confirmed_doc.clone();
                    if let inkstone_core::ReplayOutcome::Fatal { reason } =
                        inkstone_core::sequenced_replay(&mut document, update)
                    {
                        return Err(format!(
                            "tail update {} could not be replayed: {reason}",
                            encrypted.seq
                        ));
                    }
                    storage::preserve_collaborative_text(&mut document, &record.confirmed_doc);
                    state.adopt_structure(&document);
                    record.confirmed_doc = document;
                    record.confirmed_seq = encrypted.seq;
                    changed = true;
                    continue;
                }
                let op = update.op.clone();
                let effects = state.transition(TextClientEvent::RemoteUpdate { update });
                let mut merge_error = None;
                let (token, checkpoint) = effects
                    .into_iter()
                    .find_map(|effect| match effect {
                        TextClientEffect::PersistRemoteMerge { token, checkpoint } => {
                            Some((token, checkpoint))
                        }
                        TextClientEffect::NotifyError(error) => {
                            merge_error = Some(error);
                            None
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        format!(
                            "tail update {} could not be merged ({}): {op:?}",
                            encrypted.seq,
                            merge_error.unwrap_or_default()
                        )
                        .chars()
                        .take(400)
                        .collect::<String>()
                    })?;
                record.confirmed_doc = checkpoint.document.clone();
                record.text_snapshot = Some(checkpoint.text_snapshot.clone());
                record
                    .pending_updates
                    .retain(|pending| pending.update_id != checkpoint.merged_update_id);
                record.confirmed_seq = encrypted.seq;
                state.transition(TextClientEvent::RemoteMergePersisted { token });
                changed = true;
            }
            if page_len < 500 {
                break;
            }
        }
        if changed {
            let json = serde_json::to_string(&record).map_err(|error| error.to_string())?;
            db.put_document_record(&storage_key, &json)
                .await
                .map_err(|error| format!("local background checkpoint failed: {error:?}"))?;
        }
        Ok(changed)
    }

    async fn discover_remote_docs(
        &self,
        workspace_id: uuid::Uuid,
        key: [u8; 32],
        workspace_revision: u64,
    ) -> Result<bool, String> {
        let endpoint = format!("/api/workspaces/{workspace_id}/docs");
        let docs =
            api::json::<PaginatedResponse<DocResponse>, serde_json::Value>("GET", &endpoint, None)
                .await
                .map_err(|error| error.message)?
                .items;
        let workspace_db = {
            let inner = self.0.borrow();
            if inner.workspace_id != Some(workspace_id)
                || inner.workspace_key != Some(key)
                || inner.workspace_revision != workspace_revision
            {
                return Ok(false);
            }
            inner
                .workspace_db
                .clone()
                .ok_or_else(|| "Local workspace storage is unavailable".to_string())?
        };
        let document_db = IndexedDb::open("inkstone_local_db", 1)
            .await
            .map_err(|error| format!("Local document storage is unavailable: {error:?}"))?;
        let mut remote_ids = HashSet::new();
        let mut discovered = Vec::new();
        let mut quarantined = Vec::new();
        let mut background_changed = false;
        let can_edit = can_edit_workspace(&self.0.borrow());
        for doc in docs {
            if {
                let inner = self.0.borrow();
                inner.workspace_id != Some(workspace_id)
                    || inner.workspace_key != Some(key)
                    || inner.workspace_revision != workspace_revision
            } {
                return Ok(false);
            }
            let doc_id = DocId(doc.id);
            let previous_path = self.0.borrow().active_workspace_ownership.iter().find_map(
                |(path, owned_doc_id)| (*owned_doc_id == Some(doc_id)).then(|| path.clone()),
            );
            let projection_key = remote_path_envelope_metadata(workspace_id, DocId(doc.id));
            let local_projection = match workspace_db.get_metadata(&projection_key).await {
                Ok(value) => {
                    value.and_then(|json| serde_json::from_str::<LocalPathProjection>(&json).ok())
                }
                Err(error) => {
                    quarantined.push(format!("{} metadata: {error:?}", doc.id));
                    continue;
                }
            };
            let path = if let Some(mut projection) =
                local_projection.filter(|item| item.pending && can_edit)
            {
                let patch_endpoint = format!("/api/docs/{}", doc.id);
                let response = match api::json::<DocResponse, _>(
                    "PATCH",
                    &patch_endpoint,
                    Some(&UpdateDocPathProjectionRequest {
                        encrypted_path_b64: projection.encrypted.ciphertext_b64.clone(),
                        path_nonce_b64: projection.encrypted.nonce_b64.clone(),
                        projection_version: projection.projection_version,
                    }),
                )
                .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        quarantined.push(format!(
                            "{} path projection retry: {}",
                            doc.id, error.message
                        ));
                        continue;
                    }
                };
                if response.path_projection_version < projection.projection_version {
                    quarantined.push(format!(
                        "{} path projection retry did not reach the server",
                        doc.id
                    ));
                    continue;
                }
                if response.path_projection_version == projection.projection_version
                    && response.encrypted_path_b64 == projection.encrypted.ciphertext_b64
                    && response.path_nonce_b64 == projection.encrypted.nonce_b64
                {
                    projection.pending = false;
                } else {
                    let plaintext = match crypto::decrypt(
                        &key,
                        &response.encrypted_path_b64,
                        &response.path_nonce_b64,
                        &crypto::path_aad(workspace_id, doc.id),
                    ) {
                        Ok(plaintext) => plaintext,
                        Err(error) => {
                            quarantined.push(format!(
                                "{} winning path projection decryption: {error}",
                                doc.id
                            ));
                            continue;
                        }
                    };
                    let path = match String::from_utf8(plaintext) {
                        Ok(path) => path,
                        Err(_) => {
                            quarantined.push(format!("{} winning path is not UTF-8", doc.id));
                            continue;
                        }
                    };
                    projection = LocalPathProjection {
                        path,
                        encrypted: crypto::Ciphertext {
                            ciphertext_b64: response.encrypted_path_b64,
                            nonce_b64: response.path_nonce_b64,
                        },
                        pending: false,
                        projection_version: response.path_projection_version,
                    };
                }
                if let Err(error) = workspace_db
                    .set_metadata(
                        &projection_key,
                        &serde_json::to_string(&projection).map_err(|error| error.to_string())?,
                    )
                    .await
                {
                    quarantined.push(format!("{} path projection metadata: {error:?}", doc.id));
                    continue;
                }
                projection.path
            } else {
                let plaintext = match crypto::decrypt(
                    &key,
                    &doc.encrypted_path_b64,
                    &doc.path_nonce_b64,
                    &crypto::path_aad(workspace_id, doc.id),
                ) {
                    Ok(plaintext) => plaintext,
                    Err(error) => {
                        quarantined.push(format!("{} path decryption: {error}", doc.id));
                        continue;
                    }
                };
                let path = match String::from_utf8(plaintext) {
                    Ok(path) => path,
                    Err(_) => {
                        quarantined.push(format!("{} path is not UTF-8", doc.id));
                        continue;
                    }
                };
                let projection = LocalPathProjection {
                    path: path.clone(),
                    encrypted: crypto::Ciphertext {
                        ciphertext_b64: doc.encrypted_path_b64.clone(),
                        nonce_b64: doc.path_nonce_b64.clone(),
                    },
                    pending: false,
                    projection_version: doc.path_projection_version,
                };
                if let Err(error) = workspace_db
                    .set_metadata(
                        &projection_key,
                        &serde_json::to_string(&projection).map_err(|error| error.to_string())?,
                    )
                    .await
                {
                    quarantined.push(format!("{} path projection metadata: {error:?}", doc.id));
                    continue;
                }
                path
            };
            let workspace_path = match WorkspacePath::new(&path) {
                Ok(path) => path,
                Err(error) => {
                    quarantined.push(format!("{} invalid path: {error}", doc.id));
                    continue;
                }
            };
            if self.0.borrow().workspace_revision != workspace_revision {
                return Ok(false);
            }
            let existing_doc_id = match workspace_db.get_doc_id(&path).await {
                Ok(doc_id) => doc_id,
                Err(error) => {
                    quarantined.push(format!("{} document mapping: {error:?}", doc.id));
                    continue;
                }
            };
            if existing_doc_id.is_some_and(|existing| existing != doc.id) {
                quarantined.push(format!(
                    "{} path {path} conflicts with another local document",
                    doc.id
                ));
                continue;
            }
            if let Some(previous_path) = previous_path.as_deref().filter(|old| *old != path) {
                let Ok(previous_workspace_path) = WorkspacePath::new(previous_path) else {
                    quarantined.push(format!("{} prior local path is invalid", doc.id));
                    continue;
                };
                match workspace_db.read_file(&previous_workspace_path).await {
                    Ok(Some(_)) => {
                        if let Err(error) = workspace_db
                            .rename_file(&previous_workspace_path, &workspace_path)
                            .await
                        {
                            quarantined.push(format!("{} remote rename: {error:?}", doc.id));
                            continue;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        quarantined.push(format!("{} prior local path: {error:?}", doc.id));
                        continue;
                    }
                }
            }
            match workspace_db.read_file(&workspace_path).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    if let Err(error) = workspace_db.write_file(&workspace_path, &[]).await {
                        quarantined.push(format!("{} local placeholder: {error:?}", doc.id));
                        continue;
                    }
                }
                Err(error) => {
                    quarantined.push(format!("{} local placeholder: {error:?}", doc.id));
                    continue;
                }
            }
            if let Err(error) = workspace_db.set_doc_id(&path, doc.id).await {
                quarantined.push(format!("{} document mapping: {error:?}", doc.id));
                continue;
            }
            match self
                .refresh_inactive_document(workspace_id, key, doc_id, doc.current_seq, &document_db)
                .await
            {
                Ok(true) => background_changed = true,
                Ok(false) => {}
                Err(error) => {
                    quarantined.push(format!("{} content: {error}", doc.id));
                }
            }
            remote_ids.insert(doc_id);
            discovered.push((path, doc_id));
        }
        let peripheral_changed = self
            .discover_remote_assets_and_folders(
                workspace_id,
                key,
                workspace_revision,
                &workspace_db,
            )
            .await?;
        let stale_documents = {
            let inner = self.0.borrow();
            if inner.workspace_revision != workspace_revision {
                return Ok(false);
            }
            inner
                .active_workspace_ownership
                .iter()
                .filter_map(|(path, doc_id)| {
                    let doc_id = (*doc_id)?;
                    (!remote_ids.contains(&doc_id) && !inner.locally_new_docs.contains(&doc_id))
                        .then(|| (path.clone(), doc_id))
                })
                .collect::<Vec<_>>()
        };
        let mut selected_deleted = false;
        let mut protected_local_ids = HashSet::new();
        for (path, doc_id) in &stale_documents {
            if self.0.borrow().workspace_revision != workspace_revision {
                return Ok(false);
            }
            let has_pending_updates = document_db
                .get_document_record(&document_storage_key(Some(workspace_id), *doc_id))
                .await
                .map_err(|error| format!("Failed to inspect deleted document state: {error:?}"))?
                .and_then(|json| storage::deserialize_record(&json).ok())
                .is_some_and(|record| !record.pending_updates.is_empty());
            if self.0.borrow().workspace_revision != workspace_revision {
                return Ok(false);
            }
            if has_pending_updates {
                protected_local_ids.insert(*doc_id);
            } else {
                selected_deleted |= self.0.borrow().selected_path.as_deref() == Some(path);
            }
        }
        let changed = {
            let mut inner = self.0.borrow_mut();
            if inner.workspace_id != Some(workspace_id)
                || inner.workspace_key != Some(key)
                || inner.workspace_revision != workspace_revision
            {
                return Ok(false);
            }
            let old_ids = inner.remote_docs.clone();
            let mut changed = old_ids != remote_ids;
            let locally_new = inner.locally_new_docs.clone();
            inner.active_workspace_ownership.retain(|_, doc_id| {
                doc_id.is_none_or(|doc_id| {
                    remote_ids.contains(&doc_id)
                        || locally_new.contains(&doc_id)
                        || protected_local_ids.contains(&doc_id)
                })
            });
            inner.remote_docs = remote_ids;
            for (path, doc_id) in discovered {
                let old_path = inner.active_workspace_ownership.iter().find_map(
                    |(owned_path, owned_doc_id)| {
                        (*owned_doc_id == Some(doc_id)).then(|| owned_path.clone())
                    },
                );
                if old_path.as_deref() != Some(&path) {
                    if let Some(old_path) = old_path {
                        if inner.selected_path.as_deref() == Some(&old_path) {
                            inner.selected_path = Some(path.clone());
                        }
                        inner.active_workspace_ownership.remove(&old_path);
                    }
                    inner.active_workspace_ownership.insert(path, Some(doc_id));
                    changed = true;
                }
            }
            changed || background_changed || peripheral_changed
        };
        if selected_deleted {
            self.0.borrow_mut().selected_path = None;
            self.dispatch(ClientEvent::Close);
        }
        if !protected_local_ids.is_empty() {
            self.set_sync_error_if_changed(
                "A remotely deleted document has unsynced local edits and was preserved locally"
                    .into(),
            );
        }
        if !quarantined.is_empty() {
            self.set_sync_error_if_changed(format!(
                "Quarantined {} remote document(s); discovery continued: {}",
                quarantined.len(),
                quarantined.join("; ")
            ));
        }
        self.persist_active_workspace_ownership(workspace_id)
            .await?;
        Ok(changed)
    }

    async fn discover_remote_assets_and_folders(
        &self,
        workspace_id: uuid::Uuid,
        key: [u8; 32],
        workspace_revision: u64,
        workspace_db: &WorkspaceDb,
    ) -> Result<bool, String> {
        let assets_endpoint = format!("/api/workspaces/{workspace_id}/assets");
        let assets = api::json::<PaginatedResponse<AssetResponse>, serde_json::Value>(
            "GET",
            &assets_endpoint,
            None,
        )
        .await
        .map_err(|error| error.message)?
        .items;
        let projections_endpoint = format!("/api/workspaces/{workspace_id}/path-projections");
        let folders = api::json::<PaginatedResponse<PathProjectionResponse>, serde_json::Value>(
            "GET",
            &projections_endpoint,
            None,
        )
        .await
        .map_err(|error| error.message)?
        .items;
        let mut changed = false;
        let mut remote_asset_ids = HashSet::new();
        let mut remote_folder_ids = HashSet::new();

        for asset in assets.into_iter().filter(|asset| asset.complete) {
            if self.0.borrow().workspace_revision != workspace_revision {
                return Ok(false);
            }
            let fetched = format!("{}:{}", asset.version, asset.metadata_version);
            if workspace_db
                .get_metadata(&asset_fetched_metadata(asset.id))
                .await
                .ok()
                .flatten()
                .as_deref()
                == Some(fetched.as_str())
            {
                remote_asset_ids.insert(asset.id);
                continue;
            }
            let mut chunks = Vec::with_capacity(asset.chunk_count as usize);
            for index in 0..asset.chunk_count {
                let endpoint = format!(
                    "/api/assets/{}/versions/{}/chunks/{index}",
                    asset.id, asset.version
                );
                let chunk =
                    api::json::<AssetChunkResponse, serde_json::Value>("GET", &endpoint, None)
                        .await
                        .map_err(|error| {
                            format!("asset {} chunk {index}: {}", asset.id, error.message)
                        })?;
                chunks.push(EncryptedAssetChunk {
                    index,
                    ciphertext_b64: chunk.ciphertext_b64,
                    nonce_b64: chunk.nonce_b64,
                });
            }
            let encrypted = EncryptedAsset {
                asset_id: asset.id,
                version: asset.version,
                metadata_ciphertext_b64: asset.encrypted_metadata_b64,
                metadata_nonce_b64: asset.metadata_nonce_b64,
                chunks,
            };
            let (metadata, bytes) = decrypt_asset(&key, workspace_id, &encrypted)
                .map_err(|error| format!("asset {} authentication: {error}", asset.id))?;
            let path = WorkspacePath::new(&metadata.path)
                .map_err(|error| format!("asset {} path: {error}", asset.id))?;
            if path.is_internal() || path.is_markdown() {
                return Err(format!("asset {} has a reserved path", asset.id));
            }
            use sha2::Digest;
            let digest =
                base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&bytes));
            if digest != metadata.content_sha256 {
                return Err(format!("asset {} digest mismatch", asset.id));
            }
            let local_entries = workspace_db
                .list_entries()
                .await
                .map_err(|error| error.to_string())?;
            let mut previous_path = None;
            for entry in local_entries
                .iter()
                .filter(|entry| entry.kind == WorkspaceEntryKind::Asset)
            {
                let id = workspace_db
                    .get_metadata(&asset_id_metadata(entry.path.as_str()))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| uuid::Uuid::parse_str(&value).ok());
                if id == Some(asset.id) {
                    previous_path = Some(entry.path.clone());
                    break;
                }
            }
            if let Some(previous) = previous_path.filter(|previous| previous != &path) {
                if local_entries.iter().any(|entry| entry.path == path) {
                    return Err(format!("remote asset move collides at {}", path.as_str()));
                }
                workspace_db
                    .rename_file(&previous, &path)
                    .await
                    .map_err(|error| error.to_string())?;
                self.0
                    .borrow_mut()
                    .active_workspace_ownership
                    .remove(previous.as_str());
                changed = true;
            }
            let existing_id = workspace_db
                .get_metadata(&asset_id_metadata(path.as_str()))
                .await
                .map_err(|error| format!("asset identity cache: {error}"))?
                .and_then(|value| uuid::Uuid::parse_str(&value).ok());
            if existing_id.is_some_and(|id| id != asset.id)
                || self
                    .0
                    .borrow()
                    .active_workspace_ownership
                    .get(path.as_str())
                    .is_some_and(Option::is_some)
            {
                return Err(format!("remote asset path collision at {}", path.as_str()));
            }
            let local_version = workspace_db
                .get_metadata(&asset_version_metadata(path.as_str()))
                .await
                .ok()
                .flatten()
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(0);
            if existing_id != Some(asset.id)
                || local_version < asset.metadata_version
                || workspace_db
                    .read_file(&path)
                    .await
                    .map_err(|error| error.to_string())?
                    .is_none()
            {
                workspace_db
                    .write_file(&path, &bytes)
                    .await
                    .map_err(|error| error.to_string())?;
                workspace_db
                    .set_metadata(&asset_id_metadata(path.as_str()), &asset.id.to_string())
                    .await
                    .map_err(|error| error.to_string())?;
                workspace_db
                    .set_metadata(
                        &asset_version_metadata(path.as_str()),
                        &asset.metadata_version.to_string(),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                self.0
                    .borrow_mut()
                    .active_workspace_ownership
                    .insert(path.as_str().into(), None);
                changed = true;
            }
            workspace_db
                .set_metadata(&asset_fetched_metadata(asset.id), &fetched)
                .await
                .map_err(|error| error.to_string())?;
            remote_asset_ids.insert(asset.id);
        }

        for folder in folders {
            if folder.entry_kind != "folder" || folder.aad_version != crypto::AAD_VERSION {
                continue;
            }
            let plaintext = crypto::decrypt(
                &key,
                &folder.encrypted_path_b64,
                &folder.path_nonce_b64,
                &crypto::folder_path_aad(workspace_id, folder.id),
            )?;
            let path_text =
                String::from_utf8(plaintext).map_err(|_| "folder path is not UTF-8".to_string())?;
            let path = WorkspacePath::new(&path_text).map_err(|error| error.to_string())?;
            if path.is_internal() {
                return Err("remote folder uses a reserved path".into());
            }
            let entries = workspace_db
                .list_entries()
                .await
                .map_err(|error| error.to_string())?;
            let mut previous_path = None;
            for entry in entries
                .iter()
                .filter(|entry| entry.kind == WorkspaceEntryKind::Folder)
            {
                let id = workspace_db
                    .get_metadata(&folder_id_metadata(entry.path.as_str()))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| uuid::Uuid::parse_str(&value).ok());
                if id == Some(folder.id) {
                    previous_path = Some(entry.path.clone());
                    break;
                }
            }
            if let Some(previous) = previous_path.filter(|previous| previous != &path) {
                workspace_db
                    .rename_tree(&previous, &path)
                    .await
                    .map_err(|error| error.to_string())?;
                changed = true;
            }
            let entries = workspace_db
                .list_entries()
                .await
                .map_err(|error| error.to_string())?;
            if entries
                .iter()
                .any(|entry| entry.path == path && entry.kind != WorkspaceEntryKind::Folder)
            {
                return Err(format!("remote folder path collision at {}", path.as_str()));
            }
            if !entries.iter().any(|entry| entry.path == path) {
                workspace_db
                    .create_folder(&path)
                    .await
                    .map_err(|error| error.to_string())?;
                changed = true;
            }
            workspace_db
                .set_metadata(&folder_id_metadata(path.as_str()), &folder.id.to_string())
                .await
                .map_err(|error| error.to_string())?;
            workspace_db
                .set_metadata(
                    &folder_version_metadata(path.as_str()),
                    &folder.projection_version.to_string(),
                )
                .await
                .map_err(|error| error.to_string())?;
            remote_folder_ids.insert(folder.id);
        }

        let local_entries = workspace_db
            .list_entries()
            .await
            .map_err(|error| error.to_string())?;
        for entry in local_entries {
            if entry.kind == WorkspaceEntryKind::Asset {
                let id = workspace_db
                    .get_metadata(&asset_id_metadata(entry.path.as_str()))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| uuid::Uuid::parse_str(&value).ok());
                if id.is_some_and(|id| !remote_asset_ids.contains(&id)) {
                    workspace_db
                        .delete_file(&entry.path)
                        .await
                        .map_err(|error| error.to_string())?;
                    self.0
                        .borrow_mut()
                        .active_workspace_ownership
                        .remove(entry.path.as_str());
                    changed = true;
                }
            } else if entry.kind == WorkspaceEntryKind::Folder {
                let id = workspace_db
                    .get_metadata(&folder_id_metadata(entry.path.as_str()))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|value| uuid::Uuid::parse_str(&value).ok());
                if id.is_some_and(|id| !remote_folder_ids.contains(&id)) {
                    workspace_db
                        .delete_folder_marker(&entry.path)
                        .await
                        .map_err(|error| error.to_string())?;
                    changed = true;
                }
            }
        }
        Ok(changed)
    }

    fn logout(&self) {
        let next_device_id = uuid::Uuid::new_v4();
        let (workspace_db, workspace_ids) = {
            let inner = self.0.borrow();
            (
                inner.workspace_db.clone(),
                inner
                    .workspaces
                    .iter()
                    .map(|workspace| workspace.id)
                    .collect::<Vec<_>>(),
            )
        };
        self.0.borrow_mut().auth_phase = AuthPhase::Checking;
        self.render();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) =
                api::empty::<serde_json::Value>("POST", "/api/auth/logout", None).await
            {
                let mut inner = this.0.borrow_mut();
                inner.auth_phase = AuthPhase::Ready;
                inner.auth_error = Some(format!("Sign out failed: {}", error.message));
                drop(inner);
                this.render();
                return;
            }

            let cleanup_error = if let Some(workspace_db) = workspace_db {
                workspace_db.clear_scope();
                let mut cleanup_error = None;
                for workspace_id in workspace_ids {
                    if let Err(error) = workspace_db
                        .delete_global_metadata(&workspace_key_metadata(workspace_id))
                        .await
                    {
                        cleanup_error.get_or_insert_with(|| {
                            format!("Failed to delete a workspace key: {error:?}")
                        });
                    }
                }
                if let Err(error) = workspace_db
                    .set_global_metadata(DEVICE_ID_KEY, &next_device_id.to_string())
                    .await
                {
                    cleanup_error.get_or_insert_with(|| {
                        format!("Failed to rotate the local device identity: {error:?}")
                    });
                }
                let _ = workspace_db
                    .delete_global_metadata(DEVICE_PRIVATE_KEY)
                    .await;
                if let Err(error) = workspace_db
                    .delete_global_metadata(REMEMBERED_VAULT_KEY)
                    .await
                {
                    cleanup_error.get_or_insert_with(|| {
                        format!("Failed to forget this device's unlock: {error:?}")
                    });
                }
                cleanup_error
            } else {
                Some("Local workspace storage is unavailable".to_string())
            };

            this.stop_discovery_timer();
            this.dispatch(ClientEvent::Close);
            let mut inner = this.0.borrow_mut();
            inner.auth_user = None;
            inner.auth_phase = AuthPhase::SignedOut;
            inner.auth_error = None;
            inner.device_id = Some(next_device_id);
            inner.workspace_id = None;
            inner.workspace_key = None;
            inner.account_vault_key = None;
            inner.workspaces.clear();
            inner.members.clear();
            inner.outgoing_invitations.clear();
            inner.key_requests.clear();
            inner.devices.clear();
            inner.invite_email.clear();
            inner.invitation_in_flight = false;
            inner.invite_url = None;
            inner.invite_copy_status = None;
            inner.invitation_generation += 1;
            inner.recent_notes.clear();
            inner.selected_path = None;
            inner.active_workspace_ownership.clear();
            inner.remote_docs.clear();
            inner.advance_workspace_revision();
            inner.storage_error = cleanup_error;
            drop(inner);
            this.render();
        });
    }

    fn create_invitation(&self) {
        let (email, role, expiry, workspace_id, workspace_key, invitation_generation, user_id) = {
            let mut inner = self.0.borrow_mut();
            if inner.invitation_in_flight {
                return;
            }
            inner.invitation_in_flight = true;
            inner.invitation_generation += 1;
            inner.invite_url = None;
            inner.invite_copy_status = None;
            (
                inner.invite_email.trim().to_string(),
                inner.invite_role.clone(),
                inner.invite_expiry_hours,
                inner.workspace_id,
                inner.workspace_key,
                inner.invitation_generation,
                inner.auth_user.as_ref().map(|user| user.id),
            )
        };
        let (Some(workspace_id), Some(workspace_key), Some(user_id)) =
            (workspace_id, workspace_key, user_id)
        else {
            self.0.borrow_mut().invitation_in_flight = false;
            return;
        };
        if email.is_empty() {
            let mut inner = self.0.borrow_mut();
            inner.invitation_in_flight = false;
            inner.auth_error = Some("Invite email is required".into());
            drop(inner);
            self.render();
            return;
        }
        self.render();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/workspaces/{workspace_id}/invitations");
            match api::json::<CreateInvitationResponse, _>(
                "POST",
                &path,
                Some(&CreateInvitationRequest {
                    email,
                    role,
                    expires_in_hours: Some(expiry),
                }),
            )
            .await
            {
                Ok(invitation) => {
                    let current = {
                        let inner = this.0.borrow();
                        invitation_context_matches(
                            inner.auth_phase,
                            inner.workspace_id,
                            inner.workspace_key,
                            inner.invitation_generation,
                            inner.auth_user.as_ref().map(|user| user.id),
                            workspace_id,
                            workspace_key,
                            invitation_generation,
                            user_id,
                        )
                    };
                    if !current {
                        return;
                    }
                    if invitation.workspace_id != workspace_id {
                        let mut inner = this.0.borrow_mut();
                        inner.invitation_in_flight = false;
                        inner.auth_error = Some("Invitation workspace mismatch".into());
                        drop(inner);
                        this.render();
                        return;
                    }
                    let location = web_sys::window().unwrap().location();
                    let base = format!(
                        "{}{}",
                        location.origin().unwrap_or_default(),
                        location.pathname().unwrap_or_else(|_| "/".into())
                    );
                    let mut inner = this.0.borrow_mut();
                    inner.invite_url = (!invitation.token.is_empty())
                        .then(|| format!("{base}#invitation={}", invitation.token));
                    inner.invite_email.clear();
                    inner.invite_copy_status = Some(format!(
                        "Invitation created for {} as {}. It expires {}.",
                        invitation.email,
                        invitation.role,
                        invitation.expires_at.format("%Y-%m-%d %H:%M UTC")
                    ));
                    inner.invitation_in_flight = false;
                    inner.auth_error = None;
                    drop(inner);
                    this.render();
                }
                Err(error) => {
                    let current = {
                        let inner = this.0.borrow();
                        invitation_context_matches(
                            inner.auth_phase,
                            inner.workspace_id,
                            inner.workspace_key,
                            inner.invitation_generation,
                            inner.auth_user.as_ref().map(|user| user.id),
                            workspace_id,
                            workspace_key,
                            invitation_generation,
                            user_id,
                        )
                    };
                    if !current {
                        return;
                    }
                    let mut inner = this.0.borrow_mut();
                    inner.invitation_in_flight = false;
                    inner.auth_error = Some(error.message);
                    drop(inner);
                    this.render();
                }
            }
        });
    }

    fn refresh_settings_data(&self) {
        let (Some(workspace_id), can_admin) = ({
            let inner = self.0.borrow();
            let user_id = inner.auth_user.as_ref().map(|user| user.id);
            (
                inner.workspace_id,
                inner.members.iter().any(|member| {
                    Some(member.user_id) == user_id
                        && matches!(member.role.as_str(), "owner" | "admin")
                }),
            )
        }) else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let members_path = format!("/api/workspaces/{workspace_id}/members");
            if let Ok(response) = api::json::<
                PaginatedResponse<WorkspaceMemberResponse>,
                serde_json::Value,
            >("GET", &members_path, None)
            .await
            {
                this.0.borrow_mut().members = response.items;
            }
            let devices = api::json::<Vec<DeviceResponse>, serde_json::Value>(
                "GET",
                "/api/auth/devices",
                None,
            )
            .await;
            if let Ok(devices) = devices {
                this.0.borrow_mut().devices = devices;
            }
            if can_admin {
                let invitations_path = format!("/api/workspaces/{workspace_id}/invitations");
                if let Ok(response) = api::json::<
                    PaginatedResponse<OutgoingInvitationResponse>,
                    serde_json::Value,
                >("GET", &invitations_path, None)
                .await
                {
                    this.0.borrow_mut().outgoing_invitations = response.items;
                }
                let requests_path = format!("/api/workspaces/{workspace_id}/key-requests");
                if let Ok(response) = api::json::<
                    PaginatedResponse<InvitationKeyRequestResponse>,
                    serde_json::Value,
                >("GET", &requests_path, None)
                .await
                {
                    this.0.borrow_mut().key_requests = response.items;
                }
            }
            this.render();
        });
    }

    fn approve_key_request(&self, request_id: uuid::Uuid) {
        let context = {
            let inner = self.0.borrow();
            let request = inner
                .key_requests
                .iter()
                .find(|item| item.id == request_id)
                .cloned();
            (inner.workspace_id, inner.workspace_key, request)
        };
        let (Some(workspace_id), Some(workspace_key), Some(request)) = context else {
            return;
        };
        let public_key = base64::engine::general_purpose::STANDARD
            .decode(&request.public_key_b64)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
        let Some(public_key) = public_key else {
            self.0.borrow_mut().auth_error =
                Some("Key request contains an invalid public key".into());
            self.render();
            return;
        };
        let encrypted = match crypto::seal_workspace_key_for_device(
            &workspace_key,
            &public_key,
            workspace_id,
            request.user_id,
        ) {
            Ok(encrypted) => encrypted,
            Err(error) => {
                self.0.borrow_mut().auth_error = Some(error);
                self.render();
                return;
            }
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/workspaces/{workspace_id}/key-requests/{request_id}/approve");
            match api::json::<WorkspaceMemberResponse, _>(
                "POST",
                &path,
                Some(&ApproveInvitationKeyRequest {
                    encrypted_key_b64: encrypted.ciphertext_b64,
                    nonce_b64: encrypted.nonce_b64,
                    aad_version: crypto::AAD_VERSION,
                }),
            )
            .await
            {
                Ok(member) => {
                    this.0.borrow_mut().members.push(member);
                    this.0
                        .borrow_mut()
                        .key_requests
                        .retain(|item| item.id != request_id);
                    this.0.borrow_mut().auth_notice =
                        Some("Workspace key approved and membership activated".into());
                }
                Err(error) => this.0.borrow_mut().auth_error = Some(error.message),
            }
            this.render();
        });
    }

    fn settings_delete_action(&self, kind: &str, id: uuid::Uuid) {
        let workspace_id = self.0.borrow().workspace_id;
        let path = match (kind, workspace_id) {
            ("revoke-invitation", Some(workspace)) => {
                format!("/api/workspaces/{workspace}/invitations/{id}")
            }
            ("remove-member", Some(workspace)) => {
                format!("/api/workspaces/{workspace}/members/{id}")
            }
            ("revoke-device", _) => format!("/api/auth/devices/{id}"),
            ("revoke-session", _) => format!("/api/auth/sessions/{id}"),
            _ => return,
        };
        let this = self.clone();
        let kind = kind.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            match api::empty::<serde_json::Value>("DELETE", &path, None).await {
                Ok(()) => {
                    if kind == "revoke-device" {
                        apply_device_revocation(&mut this.0.borrow_mut().devices, id);
                    }
                    this.0.borrow_mut().auth_notice = Some("Security change applied".into());
                    this.refresh_settings_data();
                }
                Err(error) => {
                    this.0.borrow_mut().auth_error = Some(error.message);
                    this.render();
                }
            }
        });
    }

    fn resend_invitation(&self, id: uuid::Uuid) {
        let Some(workspace_id) = self.0.borrow().workspace_id else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/workspaces/{workspace_id}/invitations/{id}/resend");
            match api::json::<GenericMessageResponse, serde_json::Value>("POST", &path, None).await
            {
                Ok(response) => {
                    this.0.borrow_mut().auth_notice = Some(response.message);
                    this.refresh_settings_data();
                }
                Err(error) => {
                    this.0.borrow_mut().auth_error = Some(error.message);
                    this.render();
                }
            }
        });
    }

    fn update_member_role(&self, member_id: uuid::Uuid, role: String) {
        let Some(workspace_id) = self.0.borrow().workspace_id else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let path = format!("/api/workspaces/{workspace_id}/members/{member_id}");
            match api::json::<GenericMessageResponse, _>(
                "PATCH",
                &path,
                Some(&UpdateMemberRoleRequest { role }),
            )
            .await
            {
                Ok(response) => {
                    this.0.borrow_mut().auth_notice = Some(response.message);
                    this.refresh_settings_data();
                }
                Err(error) => {
                    this.0.borrow_mut().auth_error = Some(error.message);
                    this.render();
                }
            }
        });
    }

    fn workspace_membership_action(&self, action: &str, member_id: Option<uuid::Uuid>) {
        let Some(workspace_id) = self.0.borrow().workspace_id else {
            return;
        };
        let path = match action {
            "leave" => format!("/api/workspaces/{workspace_id}/leave"),
            "transfer" => format!("/api/workspaces/{workspace_id}/transfer-ownership"),
            "revoke-others" => "/api/auth/sessions/revoke-others".into(),
            _ => return,
        };
        let this = self.clone();
        let action = action.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let result = if action == "leave" {
                api::empty::<serde_json::Value>("POST", &path, None)
                    .await
                    .map(|()| GenericMessageResponse {
                        message: "Left workspace.".into(),
                    })
            } else if action == "transfer" {
                let Some(new_owner_user_id) = member_id else {
                    return;
                };
                api::json::<GenericMessageResponse, _>(
                    "POST",
                    &path,
                    Some(&TransferOwnershipRequest { new_owner_user_id }),
                )
                .await
            } else {
                api::json::<GenericMessageResponse, serde_json::Value>("POST", &path, None).await
            };
            match result {
                Ok(response) => {
                    this.0.borrow_mut().auth_notice = Some(response.message);
                    this.refresh_settings_data();
                }
                Err(error) => {
                    this.0.borrow_mut().auth_error = Some(error.message);
                    this.render();
                }
            }
        });
    }

    async fn ensure_remote_doc(&self, doc_id: DocId, path: &str) -> Result<(), String> {
        if self.0.borrow().remote_docs.contains(&doc_id) {
            return Ok(());
        }
        let (workspace_id, key, title) = {
            let inner = self.0.borrow();
            (
                inner
                    .workspace_id
                    .ok_or_else(|| "No active workspace".to_string())?,
                inner
                    .workspace_key
                    .ok_or_else(|| "Workspace is locked".to_string())?,
                inner.client_state.view_doc().title.clone(),
            )
        };
        let encrypted_title = crypto::encrypt(
            &key,
            title.as_bytes(),
            &crypto::title_aad(workspace_id, doc_id.0),
        )?;
        let workspace_db = self
            .0
            .borrow()
            .workspace_db
            .clone()
            .ok_or_else(|| "Local workspace storage is unavailable".to_string())?;
        let envelope_key = remote_path_envelope_metadata(workspace_id, doc_id);
        let encrypted = match workspace_db
            .get_metadata(&envelope_key)
            .await
            .map_err(|error| format!("Failed to read remote path envelope: {error:?}"))?
        {
            Some(value) => match serde_json::from_str::<LocalPathProjection>(&value) {
                Ok(projection) if projection.path == path => projection.encrypted,
                Ok(_) => {
                    let encrypted = crypto::encrypt(
                        &key,
                        path.as_bytes(),
                        &crypto::path_aad(workspace_id, doc_id.0),
                    )?;
                    let projection = LocalPathProjection {
                        path: path.to_string(),
                        encrypted: encrypted.clone(),
                        pending: false,
                        projection_version: 0,
                    };
                    workspace_db
                        .set_metadata(
                            &envelope_key,
                            &serde_json::to_string(&projection)
                                .map_err(|error| error.to_string())?,
                        )
                        .await
                        .map_err(|error| {
                            format!("Failed to replace remote path envelope: {error:?}")
                        })?;
                    encrypted
                }
                Err(_) => serde_json::from_str::<crypto::Ciphertext>(&value)
                    .map_err(|error| format!("Invalid saved remote path envelope: {error}"))?,
            },
            None => {
                let encrypted = crypto::encrypt(
                    &key,
                    path.as_bytes(),
                    &crypto::path_aad(workspace_id, doc_id.0),
                )?;
                let projection = LocalPathProjection {
                    path: path.to_string(),
                    encrypted: encrypted.clone(),
                    pending: false,
                    projection_version: 0,
                };
                let encoded = serde_json::to_string(&projection)
                    .map_err(|error| format!("Failed to encode remote path envelope: {error}"))?;
                workspace_db
                    .set_metadata(&envelope_key, &encoded)
                    .await
                    .map_err(|error| format!("Failed to save remote path envelope: {error:?}"))?;
                encrypted
            }
        };
        let endpoint = format!("/api/workspaces/{workspace_id}/docs");
        api::json::<DocResponse, _>(
            "POST",
            &endpoint,
            Some(&CreateDocRequest {
                doc_id: doc_id.0,
                encrypted_title_b64: Some(encrypted_title.ciphertext_b64),
                title_nonce_b64: Some(encrypted_title.nonce_b64),
                encrypted_path_b64: encrypted.ciphertext_b64,
                path_nonce_b64: encrypted.nonce_b64,
            }),
        )
        .await
        .map_err(|error| error.message)?;
        let mut inner = self.0.borrow_mut();
        inner.remote_docs.insert(doc_id);
        inner.locally_new_docs.remove(&doc_id);
        Ok(())
    }

    async fn update_remote_path_projection(&self, doc_id: DocId, path: &str) -> Result<(), String> {
        let (workspace_id, key, workspace_db) = {
            let inner = self.0.borrow();
            (
                inner
                    .workspace_id
                    .ok_or_else(|| "No active workspace".to_string())?,
                inner
                    .workspace_key
                    .ok_or_else(|| "Workspace is locked".to_string())?,
                inner
                    .workspace_db
                    .clone()
                    .ok_or_else(|| "Local workspace storage is unavailable".to_string())?,
            )
        };
        let encrypted = crypto::encrypt(
            &key,
            path.as_bytes(),
            &crypto::path_aad(workspace_id, doc_id.0),
        )?;
        let metadata_key = remote_path_envelope_metadata(workspace_id, doc_id);
        let projection_version = workspace_db
            .get_metadata(&metadata_key)
            .await
            .map_err(|error| format!("Failed to read prior path projection: {error:?}"))?
            .and_then(|value| serde_json::from_str::<LocalPathProjection>(&value).ok())
            .map(|projection| projection.projection_version)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| "Path projection version is exhausted".to_string())?;
        let mut projection = LocalPathProjection {
            path: path.to_string(),
            encrypted,
            pending: true,
            projection_version,
        };
        workspace_db
            .set_metadata(
                &metadata_key,
                &serde_json::to_string(&projection).map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| format!("Failed to persist renamed path projection: {error:?}"))?;
        let endpoint = format!("/api/docs/{}", doc_id.0);
        let response = api::json::<DocResponse, _>(
            "PATCH",
            &endpoint,
            Some(&UpdateDocPathProjectionRequest {
                encrypted_path_b64: projection.encrypted.ciphertext_b64.clone(),
                path_nonce_b64: projection.encrypted.nonce_b64.clone(),
                projection_version,
            }),
        )
        .await
        .map_err(|error| error.message)?;
        let current = workspace_db
            .get_metadata(&metadata_key)
            .await
            .map_err(|error| format!("Failed to verify renamed path projection: {error:?}"))?
            .and_then(|value| serde_json::from_str::<LocalPathProjection>(&value).ok());
        if current.as_ref().is_some_and(|current| {
            current.projection_version != projection_version || current.path != path
        }) {
            return Ok(());
        }
        if response.path_projection_version != projection_version
            || response.encrypted_path_b64 != projection.encrypted.ciphertext_b64
            || response.path_nonce_b64 != projection.encrypted.nonce_b64
        {
            return Err("A newer encrypted path projection won the rename race".into());
        }
        projection.pending = false;
        workspace_db
            .set_metadata(
                &metadata_key,
                &serde_json::to_string(&projection).map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| format!("Failed to confirm renamed path projection: {error:?}"))?;
        Ok(())
    }

    fn schedule_title_projection(
        &self,
        workspace_id: uuid::Uuid,
        key: [u8; 32],
        doc_id: DocId,
        update: &inkstone_core::DocumentUpdate,
        base_seq: i64,
    ) {
        let title = match &update.op {
            DocumentOp::SetTitle { title } => title.clone(),
            DocumentOp::ApplyTextUpdateV2 { payload } if payload.target == TextTarget::Title => {
                self.0
                    .borrow()
                    .text_client_state
                    .as_ref()
                    .map(|state| state.document().title.clone())
                    .unwrap_or_default()
            }
            _ => return,
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = this
                .update_remote_title_projection(workspace_id, key, doc_id, &title, base_seq)
                .await
            {
                let current = {
                    let inner = this.0.borrow();
                    inner.workspace_id == Some(workspace_id)
                        && inner.workspace_key == Some(key)
                        && inner.selected_doc_id == doc_id
                };
                if current {
                    this.set_sync_error(format!("Encrypted title projection is pending: {error}"));
                }
            }
        });
    }

    async fn update_remote_title_projection(
        &self,
        workspace_id: uuid::Uuid,
        key: [u8; 32],
        doc_id: DocId,
        title: &str,
        base_seq: i64,
    ) -> Result<(), String> {
        let can_write = {
            let inner = self.0.borrow();
            let user_id = inner.auth_user.as_ref().map(|user| user.id);
            inner.workspace_id == Some(workspace_id)
                && inner.workspace_key == Some(key)
                && inner.members.iter().any(|member| {
                    Some(member.user_id) == user_id
                        && matches!(member.role.as_str(), "owner" | "admin" | "editor")
                })
        };
        if !can_write {
            return Ok(());
        }
        let encrypted = crypto::encrypt(
            &key,
            title.as_bytes(),
            &crypto::title_aad(workspace_id, doc_id.0),
        )?;
        let endpoint = format!("/api/docs/{}/title", doc_id.0);
        api::json::<DocResponse, _>(
            "PATCH",
            &endpoint,
            Some(&UpdateDocTitleProjectionRequest {
                encrypted_title_b64: encrypted.ciphertext_b64,
                title_nonce_b64: encrypted.nonce_b64,
                base_seq,
            }),
        )
        .await
        .map_err(|error| error.message)?;
        Ok(())
    }

    fn socket_context(&self) -> Result<(uuid::Uuid, [u8; 32], uuid::Uuid), String> {
        let inner = self.0.borrow();
        Ok((
            inner
                .workspace_id
                .ok_or_else(|| "No active workspace".to_string())?,
            inner
                .workspace_key
                .ok_or_else(|| "Workspace is locked".to_string())?,
            inner
                .device_id
                .ok_or_else(|| "Device identity is unavailable".to_string())?,
        ))
    }

    fn queue_presence(&self, state: PresenceKind) {
        if self.0.borrow().local_mode {
            return;
        }
        self.live_broadcast(&state, false);
        self.0.borrow_mut().pending_presence = Some(PresencePayload {
            version: PRESENCE_VERSION,
            sent_at_ms: js_sys::Date::now() as i64,
            state,
        });
        if self.2.borrow().presence_timeout.is_some() {
            return;
        }
        let this = self.clone();
        let callback = Closure::wrap(Box::new(move || {
            this.2.borrow_mut().presence_timeout = None;
            this.send_pending_presence();
        }) as Box<dyn FnMut()>);
        if let Ok(timeout) = web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                40,
            )
        {
            self.2.borrow_mut().presence_timeout = Some((timeout, callback));
        }
    }

    fn send_pending_presence(&self) {
        let payload = self.0.borrow_mut().pending_presence.take();
        let Some(payload) = payload else { return };
        self.0.borrow_mut().last_presence = Some(payload.clone());
        if let Some(frame) = self.seal_presence(payload) {
            self.send_presence_frame(&frame);
        }
    }

    /// Encrypts presence under the next counter, ready for the socket or a
    /// direct Live mode channel.
    fn seal_presence(&self, mut payload: PresencePayload) -> Option<EncryptedPresenceWire> {
        let (workspace_id, key, device_id) = self.socket_context().ok()?;
        let doc_id = self.0.borrow().selected_doc_id;
        let counter = {
            let mut inner = self.0.borrow_mut();
            inner.presence_counter = inner.presence_counter.saturating_add(1);
            inner.presence_counter
        };
        payload.sent_at_ms = js_sys::Date::now() as i64;
        let plaintext = serde_json::to_vec(&payload).ok()?;
        let encrypted = crypto::encrypt(
            &key,
            &plaintext,
            &crypto::presence_aad(workspace_id, doc_id.0, device_id, counter),
        )
        .ok()?;
        Some(EncryptedPresenceWire {
            doc_id: doc_id.0,
            sender_device_id: device_id,
            encrypted_presence_b64: encrypted.ciphertext_b64,
            nonce_b64: encrypted.nonce_b64,
            aad_version: crypto::AAD_VERSION,
            counter,
        })
    }

    fn send_presence_frame(&self, frame: &EncryptedPresenceWire) {
        let Ok(message) = serde_json::to_string(&ClientWsMessage::EncryptedPresence {
            doc_id: frame.doc_id,
            encrypted_presence_b64: frame.encrypted_presence_b64.clone(),
            nonce_b64: frame.nonce_b64.clone(),
            aad_version: frame.aad_version,
            counter: frame.counter,
        }) else {
            return;
        };
        if let Some(binding) = self.2.borrow().socket.as_ref() {
            if binding.socket.ready_state() == web_sys::WebSocket::OPEN {
                let _ = binding.socket.send_with_str(&message);
            }
        }
    }

    /// Presence from the socket, or with `direct` from a Live mode channel.
    /// Each path keeps its own replay counter: a direct frame may overtake
    /// the socket copy of an earlier one, and both must still be accepted.
    fn handle_encrypted_presence(
        &self,
        frame: EncryptedPresenceWire,
        workspace_id: uuid::Uuid,
        doc_id: DocId,
        key: &[u8; 32],
        own_device_id: uuid::Uuid,
        direct: bool,
    ) {
        if !presence_sender_allowed(frame.sender_device_id, own_device_id)
            || frame.doc_id != doc_id.0
            || frame.aad_version != crypto::AAD_VERSION
        {
            return;
        }
        let socket_counter = self
            .0
            .borrow()
            .remote_presence
            .get(&frame.sender_device_id)
            .map(|item| item.counter);
        let previous = if direct {
            self.2.borrow().live.direct_counter(frame.sender_device_id)
        } else {
            socket_counter
        };
        if !accept_presence_counter(previous, frame.counter) {
            return;
        }
        let payload = crypto::decrypt(
            key,
            &frame.encrypted_presence_b64,
            &frame.nonce_b64,
            &crypto::presence_aad(
                workspace_id,
                doc_id.0,
                frame.sender_device_id,
                frame.counter,
            ),
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PresencePayload>(&bytes).ok());
        let Some(payload) = payload.filter(|payload| payload.version == PRESENCE_VERSION) else {
            return;
        };
        let sender = frame.sender_device_id;
        if direct {
            self.2
                .borrow_mut()
                .live
                .note_direct_counter(sender, frame.counter);
        }
        if let PresenceKind::Signal { to, signal } = payload.state {
            if direct {
                return;
            }
            {
                // Keep the counter moving without replacing what the sender
                // is shown doing.
                let mut inner = self.0.borrow_mut();
                let now = js_sys::Date::now();
                let entry = inner
                    .remote_presence
                    .entry(sender)
                    .or_insert_with(|| RemotePresence {
                        payload: PresencePayload {
                            version: PRESENCE_VERSION,
                            sent_at_ms: now as i64,
                            state: PresenceKind::Idle,
                        },
                        counter: frame.counter,
                        received_at_ms: now,
                    });
                entry.counter = frame.counter;
                entry.received_at_ms = now;
            }
            if to.is_none_or(|to| to == own_device_id) {
                self.live_handle_signal(sender, signal);
            }
            return;
        }
        if !direct {
            self.live_noticed(sender);
        }
        let is_ink = matches!(payload.state, PresenceKind::Ink { .. });
        if let PresenceKind::Ink {
            payload: ink_payload,
        } = &payload.state
        {
            if ink_payload.version != INK_PRESENCE_VERSION {
                return;
            }
            let _ = self.1.borrow_mut().apply_remote_preview(
                frame.sender_device_id,
                ink_payload,
                js_sys::Date::now(),
            );
        }
        self.0.borrow_mut().remote_presence.insert(
            frame.sender_device_id,
            RemotePresence {
                payload,
                counter: if direct {
                    socket_counter.unwrap_or(0)
                } else {
                    frame.counter
                },
                received_at_ms: js_sys::Date::now(),
            },
        );
        if !is_ink {
            self.render();
        }
    }

    fn start_presence_timer(&self) {
        self.stop_presence_timer();
        let this = self.clone();
        let callback = Closure::wrap(Box::new(move || {
            let now = js_sys::Date::now();
            let (changed, expired) = {
                let mut inner = this.0.borrow_mut();
                let expired = inner
                    .remote_presence
                    .iter()
                    .filter(|(_, item)| !presence_is_live(item.received_at_ms, now))
                    .map(|(participant, _)| *participant)
                    .collect::<Vec<_>>();
                let before = inner.remote_presence.len();
                inner
                    .remote_presence
                    .retain(|_, item| presence_is_live(item.received_at_ms, now));
                if inner.pending_presence.is_none() {
                    inner.pending_presence = inner.last_presence.clone();
                }
                (before != inner.remote_presence.len(), expired)
            };
            {
                let mut ink = this.1.borrow_mut();
                for participant in &expired {
                    ink.remove_remote_participant(*participant);
                }
                ink.expire_remote_previews(now, PRESENCE_TTL_MS);
            }
            for participant in expired {
                this.live_close_peer(participant);
            }
            this.send_pending_presence();
            this.live_announce();
            if changed {
                this.render();
            }
        }) as Box<dyn FnMut()>);
        if let Ok(interval) = web_sys::window()
            .unwrap()
            .set_interval_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                10_000,
            )
        {
            self.2.borrow_mut().presence_interval = Some((interval, callback));
        }
    }

    fn stop_presence_timer(&self) {
        let mut handles = self.2.borrow_mut();
        if let Some((timeout, _)) = handles.presence_timeout.take() {
            if let Some(window) = web_sys::window() {
                window.clear_timeout_with_handle(timeout);
            }
        }
        if let Some((interval, _)) = handles.presence_interval.take() {
            if let Some(window) = web_sys::window() {
                window.clear_interval_with_handle(interval);
            }
        }
        drop(handles);
        self.live_close_all(true);
        let mut inner = self.0.borrow_mut();
        inner.pending_presence = None;
        inner.last_presence = None;
        inner.remote_presence.clear();
        drop(inner);
        self.1.borrow_mut().clear_remote_previews();
    }

    fn connect_socket(
        &self,
        lifecycle: inkstone_client::LifecycleId,
        connection_id: inkstone_client::ConnectionId,
        doc_id: DocId,
    ) -> Result<(), String> {
        let (workspace_id, key, device_id) = self.socket_context()?;
        let location = web_sys::window()
            .ok_or_else(|| "Browser window is unavailable".to_string())?
            .location();
        let scheme = if location.protocol().unwrap_or_default() == "https:" {
            "wss"
        } else {
            "ws"
        };
        let url = format!(
            "{scheme}://{}/api/sync/docs/{}/ws?device_id={device_id}{}",
            location.host().map_err(|_| "Invalid browser host")?,
            doc_id.0,
            api::link_token()
                .map(|token| format!("&link={token}"))
                .unwrap_or_default()
        );
        let socket = web_sys::WebSocket::new(&url)
            .map_err(|error| format!("Failed to open WebSocket: {error:?}"))?;

        if let Some(binding) = self.2.borrow_mut().socket.take() {
            binding.detach();
        }

        let open_driver = self.clone();
        let on_open = Closure::wrap(Box::new(move |_event: web_sys::Event| {
            open_driver.dispatch(ClientEvent::SocketConnected {
                lifecycle,
                connection_id,
            });
        }) as Box<dyn FnMut(web_sys::Event)>);
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let message_driver = self.clone();
        let on_message = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
            let Some(text) = event.data().as_string() else {
                message_driver.set_sync_error("Received a non-text WebSocket message".into());
                return;
            };
            let message_type = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .get("type")
                        .and_then(|kind| kind.as_str())
                        .map(str::to_string)
                });
            if message_type.as_deref() == Some("encrypted_presence") {
                if let Ok(frame) = serde_json::from_str::<EncryptedPresenceWire>(&text) {
                    message_driver.handle_encrypted_presence(
                        frame,
                        workspace_id,
                        doc_id,
                        &key,
                        device_id,
                        false,
                    );
                }
                return;
            }
            let message = match serde_json::from_str::<ServerWsMessage>(&text) {
                Ok(message) => message,
                Err(error) => {
                    message_driver.set_sync_error(format!("Invalid WebSocket message: {error}"));
                    return;
                }
            };
            match message {
                ServerWsMessage::Ready {
                    doc_id: outer_doc_id,
                    current_seq,
                } if outer_doc_id == doc_id.0 => {
                    message_driver.dispatch(ClientEvent::Ready {
                        lifecycle,
                        connection_id,
                        current_seq,
                    });
                    message_driver.live_announce();
                }
                ServerWsMessage::EncryptedUpdate {
                    doc_id: outer_doc_id,
                    update_id: outer_update_id,
                    seq,
                    encrypted_update_b64,
                    nonce_b64,
                    aad_version,
                    ..
                } => {
                    if aad_version != crypto::AAD_VERSION || outer_doc_id != doc_id.0 {
                        message_driver.set_sync_error(
                            "Rejected a WebSocket update with mismatched outer identity".into(),
                        );
                        return;
                    }
                    let plaintext = crypto::decrypt(
                        &key,
                        &encrypted_update_b64,
                        &nonce_b64,
                        &crypto::update_aad(workspace_id, outer_doc_id, outer_update_id.0),
                    );
                    let update = plaintext
                        .map_err(|error| format!("Update decryption failed: {error}"))
                        .and_then(|bytes| {
                            serde_json::from_slice::<inkstone_core::DocumentUpdate>(&bytes)
                                .map_err(|error| format!("Invalid decrypted update: {error}"))
                        });
                    match update {
                        Ok(update)
                            if update.doc_id.0 == outer_doc_id
                                && update.update_id == outer_update_id =>
                        {
                            let projected =
                                message_driver.project_remote_text_update(update.clone());
                            message_driver.schedule_title_projection(
                                workspace_id,
                                key,
                                doc_id,
                                &update,
                                seq,
                            );
                            message_driver.dispatch(ClientEvent::SequencedUpdate {
                                lifecycle,
                                connection_id,
                                seq,
                                update: projected,
                            });
                        }
                        Ok(_) => message_driver.set_sync_error(
                            "Rejected an update whose encrypted inner identity did not match"
                                .into(),
                        ),
                        Err(error) => message_driver.set_sync_error(error),
                    }
                }
                ServerWsMessage::Error { message } => {
                    if message == RATE_LIMITED {
                        message_driver.resend_pending_text_soon();
                        return;
                    }
                    if message == "Workspace role is read-only" {
                        let mut inner = message_driver.0.borrow_mut();
                        let user_id = inner.auth_user.as_ref().map(|user| user.id);
                        if let Some(member) = inner
                            .members
                            .iter_mut()
                            .find(|member| Some(member.user_id) == user_id)
                        {
                            member.role = "viewer".into();
                        }
                    }
                    message_driver.set_sync_error(message);
                }
                ServerWsMessage::Ready { .. } => {
                    message_driver.set_sync_error("Rejected Ready for a different document".into())
                }
                ServerWsMessage::Pong | ServerWsMessage::Presence { .. } => {}
            }
        }) as Box<dyn FnMut(web_sys::MessageEvent)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let error_driver = self.clone();
        let on_error = Closure::wrap(Box::new(move |_event: web_sys::ErrorEvent| {
            error_driver.set_sync_error("WebSocket transport error".into());
        }) as Box<dyn FnMut(web_sys::ErrorEvent)>);
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let close_driver = self.clone();
        let on_close = Closure::wrap(Box::new(move |event: web_sys::CloseEvent| {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "Sync connection closed ({}{})",
                event.code(),
                if event.reason().is_empty() {
                    String::new()
                } else {
                    format!(": {}", event.reason())
                }
            )));
            close_driver.dispatch(ClientEvent::SocketDisconnected {
                lifecycle,
                connection_id,
            });
            let auth_driver = close_driver.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ =
                    api::json::<AuthUserResponse, serde_json::Value>("GET", "/api/auth/me", None)
                        .await;
                drop(auth_driver);
            });
        }) as Box<dyn FnMut(web_sys::CloseEvent)>);
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        self.2.borrow_mut().socket = Some(SocketBinding {
            socket,
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        });
        self.start_presence_timer();
        Ok(())
    }

    fn disconnect_socket(&self) {
        self.stop_presence_timer();
        if let Some(binding) = self.2.borrow_mut().socket.take() {
            binding.detach();
        }
    }

    pub(crate) fn set_sync_error(&self, message: String) {
        web_sys::console::error_1(&JsValue::from_str(&message));
        self.0.borrow_mut().sync_error = Some(message);
        self.render();
    }

    fn invalidate_expired_session(&self) {
        if self.0.borrow().auth_user.is_none() {
            return;
        }
        if self.0.borrow().public_view.is_some() {
            self.public_link_revoked();
            return;
        }
        self.stop_discovery_timer();
        self.disconnect_socket();
        if let Some(workspace_db) = self.0.borrow().workspace_db.as_ref() {
            workspace_db.clear_scope();
        }
        let mut inner = self.0.borrow_mut();
        expire_auth_context(&mut inner);
        drop(inner);
        self.render();
    }

    fn set_sync_error_if_changed(&self, message: String) {
        let changed = {
            let mut inner = self.0.borrow_mut();
            if inner.sync_error.as_deref() == Some(&message) {
                false
            } else {
                inner.sync_error = Some(message.clone());
                true
            }
        };
        if changed {
            web_sys::console::error_1(&JsValue::from_str(&message));
            self.render();
        }
    }

    fn start_discovery_timer(&self) {
        self.stop_discovery_timer();
        if self.0.borrow().local_mode {
            return;
        }
        let timer_driver = self.clone();
        let callback = Closure::wrap(Box::new(move || {
            timer_driver.trigger_remote_discovery();
        }) as Box<dyn FnMut()>);
        match web_sys::window()
            .unwrap()
            .set_interval_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                7_000,
            ) {
            Ok(interval) => {
                self.2.borrow_mut().discovery_interval = Some((interval, callback));
            }
            Err(error) => {
                self.set_sync_error(format!("Failed to schedule document discovery: {error:?}"));
            }
        }
    }

    fn stop_discovery_timer(&self) {
        if let Some((interval, _)) = self.2.borrow_mut().discovery_interval.take() {
            if let Some(window) = web_sys::window() {
                window.clear_interval_with_handle(interval);
            }
        }
        self.0.borrow_mut().discovery_in_flight = false;
    }

    fn trigger_remote_discovery(&self) {
        let context = {
            let mut inner = self.0.borrow_mut();
            if inner.local_mode || inner.auth_phase != AuthPhase::Ready || inner.discovery_in_flight
            {
                return;
            }
            let (Some(workspace_id), Some(key)) = (inner.workspace_id, inner.workspace_key) else {
                return;
            };
            inner.discovery_in_flight = true;
            (workspace_id, key, inner.workspace_revision)
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let result = this
                .discover_remote_docs(context.0, context.1, context.2)
                .await;
            let members_path = format!("/api/workspaces/{}/members", context.0);
            let guest = this.0.borrow().guest_name.is_some();
            let refreshed_members = if guest {
                api::json::<PublicLinkResponse, serde_json::Value>("GET", "/api/public-link", None)
                    .await
                    .ok()
                    .map(|details| {
                        vec![WorkspaceMemberResponse {
                            user_id: details.user_id,
                            email: String::new(),
                            role: if details.workspace.public_link_editable {
                                "editor".into()
                            } else {
                                "viewer".into()
                            },
                            joined_at: details.workspace.created_at,
                            guest_name: details.guest,
                        }]
                    })
            } else if this.0.borrow().public_view.is_some() {
                None
            } else {
                api::json::<PaginatedResponse<WorkspaceMemberResponse>, serde_json::Value>(
                    "GET",
                    &members_path,
                    None,
                )
                .await
                .ok()
                .map(|response| response.items)
            };
            let current = {
                let mut inner = this.0.borrow_mut();
                if inner.workspace_revision != context.2
                    || inner.workspace_id != Some(context.0)
                    || inner.workspace_key != Some(context.1)
                {
                    false
                } else {
                    inner.discovery_in_flight = false;
                    true
                }
            };
            if !current {
                return;
            }
            if let Some(members) = refreshed_members {
                this.0.borrow_mut().members = members;
            }
            match result {
                Ok(true) => this.refresh_entries(),
                Ok(false) => {}
                Err(error) => this.set_sync_error_if_changed(format!(
                    "Remote document discovery failed: {error}"
                )),
            }
        });
    }

    fn switch_workspace(&self, workspace_id: uuid::Uuid) {
        let (workspace, user, vault_key) = {
            let inner = self.0.borrow();
            (
                inner
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == workspace_id)
                    .cloned(),
                inner.auth_user.clone(),
                inner.account_vault_key,
            )
        };
        let (Some(workspace), Some(user), Some(vault_key)) = (workspace, user, vault_key) else {
            return;
        };
        self.stop_discovery_timer();
        self.dispatch(ClientEvent::Close);
        if let Some(workspace_db) = self.0.borrow().workspace_db.as_ref() {
            workspace_db.clear_scope();
        }
        {
            let mut inner = self.0.borrow_mut();
            inner.auth_phase = AuthPhase::Checking;
            inner.workspace_id = Some(workspace_id);
            inner.selected_path = None;
            inner.entries.clear();
            inner.members.clear();
            inner.remote_docs.clear();
            inner.active_workspace_ownership.clear();
            inner.advance_workspace_revision();
            inner.invite_email.clear();
            inner.invitation_in_flight = false;
            inner.invite_url = None;
            inner.invite_copy_status = None;
            inner.invitation_generation += 1;
            inner.auth_error = None;
        }
        self.render();
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(message) = this
                .activate_workspace(workspace, &user, Some(&vault_key))
                .await
            {
                let mut inner = this.0.borrow_mut();
                inner.auth_phase = AuthPhase::UnlockRequired;
                inner.auth_error = Some(message);
                drop(inner);
                this.render();
            }
        });
    }

    fn create_workspace(&self, name: String, access: vaults::VaultAccess) {
        let (Some(user), Some(vault_key), Some(workspace_db)) = ({
            let inner = self.0.borrow();
            (
                inner.auth_user.clone(),
                inner.account_vault_key,
                inner.workspace_db.clone(),
            )
        }) else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let workspace_id = uuid::Uuid::new_v4();
            let key = match crypto::random_key() {
                Ok(key) => key,
                Err(error) => {
                    this.vault_creation_failed(error);
                    return;
                }
            };
            let metadata = WorkspaceMetadata {
                name,
                description: String::new(),
                public_link: None,
            };
            let encrypted = match serde_json::to_vec(&metadata)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    crypto::encrypt(&key, &bytes, &crypto::workspace_metadata_aad(workspace_id))
                }) {
                Ok(encrypted) => encrypted,
                Err(error) => {
                    this.vault_creation_failed(error);
                    return;
                }
            };
            let workspace = match api::json::<WorkspaceResponse, _>(
                "POST",
                "/api/workspaces",
                Some(&CreateWorkspaceRequest {
                    workspace_id,
                    encrypted_metadata_b64: encrypted.ciphertext_b64,
                    metadata_nonce_b64: encrypted.nonce_b64,
                }),
            )
            .await
            {
                Ok(workspace) => workspace,
                Err(error) => {
                    this.vault_creation_failed(error.message);
                    return;
                }
            };
            let local_envelope = match crypto::encrypt(
                &vault_key,
                &key,
                &crypto::workspace_key_aad(workspace_id, user.id),
            ) {
                Ok(envelope) => envelope,
                Err(error) => {
                    this.vault_creation_failed(error);
                    return;
                }
            };
            if let Err(error) = workspace_db
                .set_global_metadata(
                    &workspace_key_metadata(workspace_id),
                    &serde_json::to_string(&local_envelope).unwrap(),
                )
                .await
            {
                this.vault_creation_failed(format!(
                    "Workspace created but local key storage failed: {error}"
                ));
                return;
            }
            if let Err(error) = this
                .put_key_envelope(&workspace, &user, &vault_key, &key)
                .await
            {
                this.vault_creation_failed(format!(
                    "Workspace created but key upload can be retried: {error}"
                ));
                return;
            }
            this.0.borrow_mut().workspaces.push(workspace.clone());
            if let Err(error) = this
                .activate_workspace(workspace, &user, Some(&vault_key))
                .await
            {
                this.vault_creation_failed(error);
                return;
            }
            this.vault_created(access).await;
        });
    }

    fn delete_current_workspace(&self) {
        let confirmed = web_sys::window()
            .and_then(|window| {
                window
                    .confirm_with_message(
                        "Delete this vault and every note in it, for everyone who has access? This can't be undone.",
                    )
                    .ok()
            })
            .unwrap_or(false);
        if !confirmed {
            return;
        }
        let (workspace_id, user, vault_key, workspace_db) = {
            let inner = self.0.borrow();
            (
                inner.workspace_id,
                inner.auth_user.clone(),
                inner.account_vault_key,
                inner.workspace_db.clone(),
            )
        };
        let (Some(workspace_id), Some(user), Some(workspace_db)) =
            (workspace_id, user, workspace_db)
        else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let endpoint = format!("/api/workspaces/{workspace_id}");
            if let Err(error) = api::empty::<serde_json::Value>("DELETE", &endpoint, None).await {
                this.set_sync_error(error.message);
                return;
            }
            if let Err(error) = workspace_db.clear_active_scope_data().await {
                this.set_sync_error(format!(
                    "Workspace deleted remotely; scoped local cleanup failed: {error}"
                ));
                return;
            }
            let _ = workspace_db
                .delete_global_metadata(&workspace_key_metadata(workspace_id))
                .await;
            let next = {
                let mut inner = this.0.borrow_mut();
                inner
                    .workspaces
                    .retain(|workspace| workspace.id != workspace_id);
                inner.workspaces.first().cloned()
            };
            match next {
                Some(next) => {
                    if let Err(error) = this
                        .activate_workspace(next, &user, vault_key.as_ref())
                        .await
                    {
                        this.set_sync_error(error);
                    }
                }
                None => this.create_workspace("My vault".into(), vaults::VaultAccess::Private),
            }
        });
    }

    fn import_asset(&self, file: web_sys::File) {
        if !can_edit_workspace(&self.0.borrow()) {
            return;
        }
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let stored = match page_files::read_file_bytes(&file).await {
                Ok(bytes) => {
                    this.store_asset(&file.name(), page_files::file_media_type(&file), bytes)
                        .await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = stored {
                this.set_sync_error(error);
                return;
            }
            this.refresh_entries();
            this.render();
        });
    }

    async fn store_asset(
        &self,
        name: &str,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<WorkspacePath, String> {
        let (workspace_id, key, workspace_db, local_mode, candidate) = {
            let mut inner = self.0.borrow_mut();
            let taken = |candidate: &str| {
                inner
                    .entries
                    .iter()
                    .any(|entry| entry.path.as_str() == candidate)
                    || inner.reserved_asset_paths.contains(candidate)
            };
            let mut candidate = format!("Assets/{name}");
            let mut suffix = 2;
            while taken(&candidate) {
                candidate = match name.rsplit_once('.') {
                    Some((stem, extension)) => format!("Assets/{stem} ({suffix}).{extension}"),
                    None => format!("Assets/{name} ({suffix})"),
                };
                suffix += 1;
            }
            inner.reserved_asset_paths.insert(candidate.clone());
            (
                inner.workspace_id.unwrap_or_else(uuid::Uuid::nil),
                inner.workspace_key,
                inner.workspace_db.clone(),
                inner.local_mode,
                candidate,
            )
        };
        let stored = self
            .upload_asset(
                workspace_id,
                key.unwrap_or([0; 32]),
                workspace_db,
                local_mode,
                &candidate,
                media_type,
                bytes,
            )
            .await;
        self.0.borrow_mut().reserved_asset_paths.remove(&candidate);
        stored
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_asset(
        &self,
        workspace_id: uuid::Uuid,
        key: [u8; 32],
        workspace_db: Option<WorkspaceDb>,
        local_mode: bool,
        candidate: &str,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<WorkspacePath, String> {
        let Some(workspace_db) = workspace_db else {
            return Err("No vault is open".into());
        };
        let path = match WorkspacePath::new(candidate) {
            Ok(path) if !path.is_internal() => path,
            _ => {
                return Err("The selected asset name is not a safe workspace path".into());
            }
        };
        use sha2::Digest;
        let digest = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&bytes));
        let metadata = AssetMetadata {
            path: path.as_str().into(),
            media_type,
            plaintext_size: bytes.len() as u64,
            content_sha256: digest,
        };
        let asset_id = uuid::Uuid::new_v4();
        let encrypted = match encrypt_asset(&key, workspace_id, asset_id, 1, &metadata, &bytes) {
            Ok(encrypted) => encrypted,
            Err(error) => {
                return Err(error.to_string());
            }
        };
        if !local_mode {
            let encrypted_size = encrypted
                .chunks
                .iter()
                .filter_map(|chunk| {
                    base64::engine::general_purpose::STANDARD
                        .decode(&chunk.ciphertext_b64)
                        .ok()
                })
                .map(|chunk| chunk.len() as u64)
                .sum();
            let endpoint = format!("/api/workspaces/{workspace_id}/assets");
            if let Err(error) = api::json::<AssetResponse, _>(
                "POST",
                &endpoint,
                Some(&CreateAssetRequest {
                    asset_id,
                    version: 1,
                    chunk_count: encrypted.chunks.len() as u32,
                    encrypted_size,
                    encrypted_metadata_b64: encrypted.metadata_ciphertext_b64.clone(),
                    metadata_nonce_b64: encrypted.metadata_nonce_b64.clone(),
                    aad_version: crypto::AAD_VERSION,
                }),
            )
            .await
            {
                return Err(format!("Asset upload can be retried: {}", error.message));
            }
            for chunk in &encrypted.chunks {
                let endpoint = format!("/api/assets/{asset_id}/versions/1/chunks/{}", chunk.index);
                if let Err(error) = api::empty(
                    "PUT",
                    &endpoint,
                    Some(&PutAssetChunkRequest {
                        ciphertext_b64: chunk.ciphertext_b64.clone(),
                        nonce_b64: chunk.nonce_b64.clone(),
                        aad_version: crypto::AAD_VERSION,
                    }),
                )
                .await
                {
                    return Err(format!(
                        "Asset upload stopped at chunk {}; re-import to retry idempotently: {}",
                        chunk.index, error.message
                    ));
                }
            }
        }
        if let Err(error) = workspace_db.write_file(&path, &bytes).await {
            return Err(format!(
                "Asset uploaded but local cache write failed: {error}"
            ));
        }
        if let Err(error) = workspace_db
            .set_metadata(&asset_id_metadata(path.as_str()), &asset_id.to_string())
            .await
        {
            return Err(format!("Asset cache identity write failed: {error}"));
        }
        if let Err(error) = workspace_db
            .set_metadata(&asset_version_metadata(path.as_str()), "1")
            .await
        {
            return Err(format!("Asset cache version write failed: {error}"));
        }
        Ok(path)
    }

    fn save_workspace_settings(&self) {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let name = document
            .get_element_by_id("workspace-name-setting")
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
            .map(|input| input.value().trim().to_string())
            .unwrap_or_default();
        let description = document
            .get_element_by_id("workspace-description-setting")
            .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            .map(|input| input.value().trim().to_string())
            .unwrap_or_default();
        if name.is_empty() || name.len() > 200 || description.len() > 2000 {
            self.0.borrow_mut().auth_error =
                Some("Vault name must be 1-200 bytes and description at most 2000 bytes.".into());
            self.render();
            return;
        }
        let (local_mode, workspace_id, key, version, public_link) = {
            let inner = self.0.borrow();
            let version = inner
                .workspaces
                .iter()
                .find(|workspace| Some(workspace.id) == inner.workspace_id)
                .map_or(1, |workspace| workspace.metadata_version);
            (
                inner.local_mode,
                inner.workspace_id,
                inner.workspace_key,
                version,
                inner.workspace_public_link.clone(),
            )
        };
        if local_mode {
            let mut inner = self.0.borrow_mut();
            inner.workspace_name = name;
            inner.workspace_description = description;
            inner.auth_notice = Some("Vault details saved locally.".into());
            drop(inner);
            self.render();
            return;
        }
        let (Some(workspace_id), Some(key)) = (workspace_id, key) else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let plaintext = match serde_json::to_vec(&WorkspaceMetadata {
                name: name.clone(),
                description: description.clone(),
                public_link,
            }) {
                Ok(value) => value,
                Err(error) => {
                    this.set_sync_error(error.to_string());
                    return;
                }
            };
            let encrypted = match crypto::encrypt(
                &key,
                &plaintext,
                &crypto::workspace_metadata_aad(workspace_id),
            ) {
                Ok(value) => value,
                Err(error) => {
                    this.set_sync_error(error);
                    return;
                }
            };
            let endpoint = format!("/api/workspaces/{workspace_id}");
            match api::json::<WorkspaceResponse, _>(
                "PATCH",
                &endpoint,
                Some(&UpdateWorkspaceRequest {
                    encrypted_metadata_b64: encrypted.ciphertext_b64,
                    metadata_nonce_b64: encrypted.nonce_b64,
                    metadata_version: version + 1,
                }),
            )
            .await
            {
                Ok(workspace) => {
                    let mut inner = this.0.borrow_mut();
                    if let Some(stored) = inner
                        .workspaces
                        .iter_mut()
                        .find(|stored| stored.id == workspace.id)
                    {
                        *stored = workspace;
                    }
                    inner.vault_names.insert(workspace_id, name.clone());
                    inner.workspace_name = name;
                    inner.workspace_description = description;
                    inner.auth_notice = Some("Encrypted vault details saved.".into());
                    inner.auth_error = None;
                    drop(inner);
                    this.render();
                }
                Err(error) => this.set_sync_error(error.message),
            }
        });
    }

    pub(crate) fn export_vault(&self) {
        self.flush_ink_before_transition();
        let this = self.clone();
        let (
            workspace_db,
            document_db,
            entries,
            workspace_id,
            workspace_key,
            selected_id,
            selected_doc,
        ) = {
            let inner = self.0.borrow();
            (
                inner.workspace_db.clone(),
                inner.db.clone(),
                inner.entries.clone(),
                inner.workspace_id.unwrap_or_else(uuid::Uuid::nil),
                inner.workspace_key,
                inner.selected_doc_id,
                inner.client_state.effective_view_doc().clone(),
            )
        };
        let (Some(workspace_db), Some(document_db)) = (workspace_db, document_db) else {
            self.0.borrow_mut().storage_error =
                Some("Local storage is unavailable for export".into());
            self.render();
            return;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let mut documents = Vec::new();
            let mut assets = Vec::new();
            let mut folders = Vec::new();
            for entry in entries {
                match entry.kind {
                    WorkspaceEntryKind::Folder => folders.push(entry.path),
                    WorkspaceEntryKind::Asset => match workspace_db.read_file(&entry.path).await {
                        Ok(Some(bytes)) => assets.push(ExportAsset {
                            path: entry.path,
                            bytes,
                        }),
                        Ok(None) => {}
                        Err(error) => {
                            this.set_sync_error(format!("Export could not read an asset: {error}"));
                            return;
                        }
                    },
                    WorkspaceEntryKind::Markdown => {
                        let path = entry.path;
                        let doc_id = match workspace_db.get_doc_id(path.as_str()).await {
                            Ok(Some(id)) => DocId(id),
                            Ok(None) => continue,
                            Err(error) => {
                                this.set_sync_error(format!(
                                    "Export could not resolve a document: {error}"
                                ));
                                return;
                            }
                        };
                        let storage_key = document_storage_key(
                            (!this.0.borrow().local_mode).then_some(workspace_id),
                            doc_id,
                        );
                        let record_json = match document_db.get_document_record(&storage_key).await
                        {
                            Ok(value) => value,
                            Err(error) => {
                                this.set_sync_error(format!(
                                    "Export could not read document state: {error:?}"
                                ));
                                return;
                            }
                        };
                        let record = record_json
                            .as_deref()
                            .and_then(|json| storage::deserialize_record(json).ok());
                        let document = if doc_id == selected_id {
                            selected_doc.clone()
                        } else if let Some(record) = &record {
                            record.confirmed_doc.clone()
                        } else {
                            InkstoneDocument::new(doc_id)
                        };
                        let has_pending_updates = if doc_id == selected_id {
                            !this.0.borrow().client_state.pending_local().is_empty()
                                || this.0.borrow().client_state.has_pending_intent()
                        } else {
                            record
                                .as_ref()
                                .is_some_and(|record| !record.pending_updates.is_empty())
                        };
                        let encrypted_crdt_backup = match (workspace_key, record_json) {
                            (Some(key), Some(json)) => crypto::encrypt(
                                &key,
                                json.as_bytes(),
                                format!("inkstone:v1:export-crdt:{workspace_id}:{doc_id}")
                                    .as_bytes(),
                            )
                            .ok()
                            .and_then(|envelope| serde_json::to_vec(&envelope).ok()),
                            (None, Some(json)) => Some(json.into_bytes()),
                            _ => None,
                        };
                        documents.push(ExportDocument {
                            path,
                            document,
                            encrypted_crdt_backup,
                            has_pending_updates,
                        });
                    }
                    WorkspaceEntryKind::Internal => {}
                }
            }
            let bytes = match inkstone_workspace::build_vault_archive(&VaultExport {
                workspace_id,
                documents,
                assets,
                empty_folders: folders,
            }) {
                Ok(bytes) => bytes,
                Err(error) => {
                    this.set_sync_error(format!("Vault export failed: {error}"));
                    return;
                }
            };
            if let Err(error) = download_bytes("inkstone-vault.zip", &bytes, "application/zip") {
                this.set_sync_error(error);
                return;
            }
            this.0.borrow_mut().auth_notice = Some("Vault export downloaded.".into());
            this.render();
        });
    }

    fn import_vault(&self, file: web_sys::File) {
        let (workspace_db, document_db, workspace_id, workspace_key, local_mode, existing) = {
            let inner = self.0.borrow();
            (
                inner.workspace_db.clone(),
                inner.db.clone(),
                inner.workspace_id.unwrap_or_else(uuid::Uuid::nil),
                inner.workspace_key,
                inner.local_mode,
                inner.entries.clone(),
            )
        };
        let (Some(workspace_db), Some(document_db)) = (workspace_db, document_db) else {
            return;
        };
        let this = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            use sha2::Digest;
            let value = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(value) => value,
                Err(error) => {
                    this.set_sync_error(format!("Archive read failed: {error:?}"));
                    return;
                }
            };
            let bytes = js_sys::Uint8Array::new(&value).to_vec();
            let imported = match inkstone_workspace::read_vault_archive(&bytes) {
                Ok(imported) => imported,
                Err(error) => {
                    this.set_sync_error(format!(
                        "Archive validation failed before import: {error}"
                    ));
                    return;
                }
            };
            if imported.manifest.workspace_id != workspace_id {
                this.set_sync_error(
                    "Archive belongs to a different workspace and cannot cross workspace isolation"
                        .into(),
                );
                return;
            }
            if imported
                .files
                .keys()
                .any(|path| existing.iter().any(|entry| entry.path == *path))
                || imported
                    .folders
                    .iter()
                    .any(|path| existing.iter().any(|entry| entry.path == *path))
            {
                this.set_sync_error(
                    "Archive import has a path collision; no changes were applied".into(),
                );
                return;
            }
            let mut records = Vec::new();
            for document in &imported.manifest.documents {
                let backup = imported.crdt_backups.get(&document.id);
                let record = if let Some(backup) = backup {
                    let plaintext = if let (Some(key), Ok(envelope)) = (
                        workspace_key,
                        serde_json::from_slice::<crypto::Ciphertext>(backup),
                    ) {
                        match crypto::decrypt(
                            &key,
                            &envelope.ciphertext_b64,
                            &envelope.nonce_b64,
                            format!(
                                "inkstone:v1:export-crdt:{workspace_id}:{}",
                                DocId(document.id)
                            )
                            .as_bytes(),
                        ) {
                            Ok(value) => value,
                            Err(error) => {
                                this.set_sync_error(format!(
                                    "CRDT backup authentication failed: {error}"
                                ));
                                return;
                            }
                        }
                    } else {
                        backup.clone()
                    };
                    match storage::deserialize_record(&String::from_utf8_lossy(&plaintext)) {
                        Ok(record) if record.doc_id.0 == document.id => record,
                        _ => {
                            this.set_sync_error("CRDT backup identity or schema is invalid; no changes were applied".into());
                            return;
                        }
                    }
                } else {
                    let path = match WorkspacePath::new(&document.path) {
                        Ok(path) => path,
                        Err(error) => {
                            this.set_sync_error(error.to_string());
                            return;
                        }
                    };
                    let markdown =
                        String::from_utf8_lossy(imported.files.get(&path).unwrap()).to_string();
                    let mut record = storage::new_record(DocId(document.id));
                    let (title, body) = match markdown
                        .strip_prefix("# ")
                        .and_then(|value| value.split_once("\n\n"))
                    {
                        Some((title, body)) => (title.to_string(), body.to_string()),
                        None => (String::new(), markdown),
                    };
                    record.confirmed_doc.title = title;
                    record
                        .confirmed_doc
                        .blocks
                        .push(inkstone_core::document::DocumentBlock::new(
                            BlockId::new_v4(),
                            BlockContent::Markdown {
                                text: body.trim_end().into(),
                            },
                        ));
                    record
                };
                records.push((WorkspacePath::new(&document.path).unwrap(), record));
            }
            let mut remote_assets = Vec::new();
            let mut remote_folders = Vec::new();
            if !local_mode {
                let Some(key) = workspace_key else {
                    return;
                };
                for folder in &imported.folders {
                    let id = uuid::Uuid::new_v4();
                    let encrypted = match crypto::encrypt(
                        &key,
                        folder.as_str().as_bytes(),
                        &crypto::folder_path_aad(workspace_id, id),
                    ) {
                        Ok(value) => value,
                        Err(error) => {
                            cleanup_remote_import(workspace_id, &remote_assets, &remote_folders)
                                .await;
                            this.set_sync_error(error);
                            return;
                        }
                    };
                    let endpoint = format!("/api/workspaces/{workspace_id}/path-projections/{id}");
                    if let Err(error) = api::json::<PathProjectionResponse, _>(
                        "PUT",
                        &endpoint,
                        Some(&PutPathProjectionRequest {
                            entry_kind: "folder".into(),
                            encrypted_path_b64: encrypted.ciphertext_b64,
                            path_nonce_b64: encrypted.nonce_b64,
                            projection_version: 1,
                            aad_version: 1,
                        }),
                    )
                    .await
                    {
                        cleanup_remote_import(workspace_id, &remote_assets, &remote_folders).await;
                        this.set_sync_error(error.message);
                        return;
                    }
                    remote_folders.push((folder.clone(), id));
                }
                for asset in &imported.manifest.assets {
                    let path = WorkspacePath::new(&asset.path).unwrap();
                    let bytes = imported.files.get(&path).unwrap();
                    let id = uuid::Uuid::new_v4();
                    let metadata = AssetMetadata {
                        path: asset.path.clone(),
                        media_type: asset_media_type(&asset.path),
                        plaintext_size: bytes.len() as u64,
                        content_sha256: base64::engine::general_purpose::STANDARD
                            .encode(sha2::Sha256::digest(bytes)),
                    };
                    let encrypted = match encrypt_asset(&key, workspace_id, id, 1, &metadata, bytes)
                    {
                        Ok(value) => value,
                        Err(error) => {
                            cleanup_remote_import(workspace_id, &remote_assets, &remote_folders)
                                .await;
                            this.set_sync_error(error.to_string());
                            return;
                        }
                    };
                    let encrypted_size = encrypted
                        .chunks
                        .iter()
                        .filter_map(|chunk| {
                            base64::engine::general_purpose::STANDARD
                                .decode(&chunk.ciphertext_b64)
                                .ok()
                        })
                        .map(|bytes| bytes.len() as u64)
                        .sum();
                    let endpoint = format!("/api/workspaces/{workspace_id}/assets");
                    if let Err(error) = api::json::<AssetResponse, _>(
                        "POST",
                        &endpoint,
                        Some(&CreateAssetRequest {
                            asset_id: id,
                            version: 1,
                            chunk_count: encrypted.chunks.len() as u32,
                            encrypted_size,
                            encrypted_metadata_b64: encrypted.metadata_ciphertext_b64,
                            metadata_nonce_b64: encrypted.metadata_nonce_b64,
                            aad_version: 1,
                        }),
                    )
                    .await
                    {
                        cleanup_remote_import(workspace_id, &remote_assets, &remote_folders).await;
                        this.set_sync_error(error.message);
                        return;
                    }
                    remote_assets.push((path, id));
                    for chunk in encrypted.chunks {
                        let endpoint =
                            format!("/api/assets/{id}/versions/1/chunks/{}", chunk.index);
                        if let Err(error) = api::empty(
                            "PUT",
                            &endpoint,
                            Some(&PutAssetChunkRequest {
                                ciphertext_b64: chunk.ciphertext_b64,
                                nonce_b64: chunk.nonce_b64,
                                aad_version: 1,
                            }),
                        )
                        .await
                        {
                            cleanup_remote_import(workspace_id, &remote_assets, &remote_folders)
                                .await;
                            this.set_sync_error(error.message);
                            return;
                        }
                    }
                }
            }
            let mut created_files = Vec::new();
            let mut created_folders = Vec::new();
            let result: Result<(), String> = async {
                for folder in &imported.folders {
                    workspace_db
                        .create_folder(folder)
                        .await
                        .map_err(|error| error.to_string())?;
                    created_folders.push(folder.clone());
                }
                for (path, record) in &records {
                    workspace_db
                        .write_file(path, imported.files.get(path).unwrap())
                        .await
                        .map_err(|error| error.to_string())?;
                    workspace_db
                        .set_doc_id(path.as_str(), record.doc_id.0)
                        .await
                        .map_err(|error| error.to_string())?;
                    document_db
                        .put_document_record(
                            &document_storage_key(
                                (!local_mode).then_some(workspace_id),
                                record.doc_id,
                            ),
                            &serde_json::to_string(record).map_err(|error| error.to_string())?,
                        )
                        .await
                        .map_err(|error| format!("{error:?}"))?;
                    created_files.push(path.clone());
                }
                for asset in &imported.manifest.assets {
                    let path = WorkspacePath::new(&asset.path).unwrap();
                    workspace_db
                        .write_file(&path, imported.files.get(&path).unwrap())
                        .await
                        .map_err(|error| error.to_string())?;
                    created_files.push(path);
                }
                for (path, id) in &remote_assets {
                    workspace_db
                        .set_metadata(&asset_id_metadata(path.as_str()), &id.to_string())
                        .await
                        .map_err(|error| error.to_string())?;
                    workspace_db
                        .set_metadata(&asset_version_metadata(path.as_str()), "1")
                        .await
                        .map_err(|error| error.to_string())?;
                }
                for (path, id) in &remote_folders {
                    workspace_db
                        .set_metadata(&folder_id_metadata(path.as_str()), &id.to_string())
                        .await
                        .map_err(|error| error.to_string())?;
                    workspace_db
                        .set_metadata(&folder_version_metadata(path.as_str()), "1")
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                for path in created_files.iter().rev() {
                    let _ = workspace_db.delete_file(path).await;
                }
                for path in created_folders.iter().rev() {
                    let _ = workspace_db.delete_folder_marker(path).await;
                }
                for (_, record) in &records {
                    let _ = workspace_db.delete_document_record(&record.doc_id.0).await;
                }
                for (_, id) in remote_assets {
                    let _ = api::empty::<serde_json::Value>(
                        "DELETE",
                        &format!("/api/assets/{id}"),
                        None,
                    )
                    .await;
                }
                for (_, id) in remote_folders {
                    let _ = api::empty::<serde_json::Value>(
                        "DELETE",
                        &format!("/api/workspaces/{workspace_id}/path-projections/{id}"),
                        None,
                    )
                    .await;
                }
                this.set_sync_error(format!("Archive import rolled back: {error}"));
                return;
            }
            {
                let mut inner = this.0.borrow_mut();
                for (_, record) in records {
                    inner.locally_new_docs.insert(record.doc_id);
                }
            }
            this.refresh_entries();
            this.index_workspace();
            this.0.borrow_mut().auth_notice = Some("Vault archive imported.".into());
            this.render();
        });
    }

    fn copy_invitation_link(&self) {
        let Some(url) = self.0.borrow().invite_url.clone() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let navigator = js_sys::Reflect::get(window.as_ref(), &JsValue::from_str("navigator"));
        let clipboard = navigator.as_ref().ok().and_then(|navigator| {
            js_sys::Reflect::get(navigator, &JsValue::from_str("clipboard")).ok()
        });
        let write_text = clipboard.as_ref().and_then(|clipboard| {
            js_sys::Reflect::get(clipboard, &JsValue::from_str("writeText"))
                .ok()
                .and_then(|function| function.dyn_into::<js_sys::Function>().ok())
                .and_then(|function| function.call1(clipboard, &JsValue::from_str(&url)).ok())
                .and_then(|promise| promise.dyn_into::<js_sys::Promise>().ok())
        });
        if let Some(promise) = write_text {
            self.0.borrow_mut().invite_copy_status = Some("Copying invitation link...".into());
            self.render();
            let this = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let message = if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
                    "Invitation link copied"
                } else {
                    "Copy was blocked. Select the link above and copy it manually."
                };
                this.0.borrow_mut().invite_copy_status = Some(message.into());
                this.render();
            });
        } else {
            self.0.borrow_mut().invite_copy_status = Some(
                "Clipboard access is unavailable. Select the link above and copy it manually."
                    .into(),
            );
            self.render();
            if let Ok(Some(element)) = window.document().unwrap().query_selector(".invite-url") {
                if let Ok(textarea) = element.dyn_into::<web_sys::HtmlTextAreaElement>() {
                    let _ = textarea.focus();
                    textarea.select();
                }
            }
        }
    }

    fn effective_document(inner: &DriverInner) -> InkstoneDocument {
        let mut document = inner.client_state.effective_view_doc().clone();
        let Some(text_state) = &inner.text_client_state else {
            return document;
        };
        let text_document = text_state.optimistic_document();
        document.title.clone_from(&text_document.title);
        for block in &mut document.blocks {
            let Some(text_block) = text_document.get_block(block.id) else {
                continue;
            };
            match (&mut block.content, &text_block.content) {
                (BlockContent::Markdown { text }, BlockContent::Markdown { text: current }) => {
                    text.clone_from(current)
                }
                (BlockContent::Math { source }, BlockContent::Math { source: current }) => {
                    source.clone_from(current)
                }
                _ => {}
            }
        }
        document
    }

    fn initialize_text_state(
        &self,
        document: InkstoneDocument,
        snapshot: Option<inkstone_core::CollaborativeTextSnapshot>,
        pending_updates: Vec<DocumentUpdate>,
    ) -> Result<(), String> {
        let replica_id = {
            let inner = self.0.borrow();
            text_replica_id(inner.device_id, document.id)
        };
        let mut state = TextClientState::new(document.clone(), replica_id)
            .map_err(|error| error.to_string())?;
        let effects = state.transition(TextClientEvent::Hydrate {
            document,
            snapshot,
            pending_updates,
        });
        if let Some(error) = effects.into_iter().find_map(|effect| match effect {
            TextClientEffect::NotifyError(error) => Some(error),
            _ => None,
        }) {
            return Err(error);
        }
        self.0.borrow_mut().text_client_state = Some(state);
        Ok(())
    }

    /// Makes sure the text state exists and knows `target` (a block may have
    /// been inserted since it was hydrated).
    pub(crate) fn ensure_text_target(&self, target: TextTarget) {
        let mut inner = self.0.borrow_mut();
        if !document_ready(&inner) {
            return;
        }
        if inner.text_client_state.is_none() {
            let document = inner.client_state.effective_view_doc().clone();
            let replica_id = text_replica_id(inner.device_id, document.id);
            inner.text_client_state = TextClientState::new(document, replica_id).ok();
        }
        let needs_target = inner.text_client_state.as_ref().is_some_and(|state| {
            let document = state.document();
            match target {
                TextTarget::Title => false,
                TextTarget::Markdown(block_id) => !matches!(
                    document.get_block(block_id).map(|block| &block.content),
                    Some(BlockContent::Markdown { .. })
                ),
                TextTarget::Math(block_id) => !matches!(
                    document.get_block(block_id).map(|block| &block.content),
                    Some(BlockContent::Math { .. })
                ),
            }
        });
        if needs_target {
            // Structure only: re-hydrating from text that already holds
            // unpersisted typing would replay that typing on top of itself.
            let structure = inner.client_state.effective_view_doc().clone();
            if let Some(state) = inner.text_client_state.as_mut() {
                state.adopt_structure(&structure);
            }
        }
    }

    /// Makes the text state know every text block on the page, so ink can
    /// pin to text in blocks inserted since it was hydrated.
    pub(crate) fn ensure_page_text_targets(&self) {
        let missing = {
            let inner = self.0.borrow();
            let known = inner
                .text_client_state
                .as_ref()
                .map(TextClientState::document);
            inner
                .client_state
                .effective_view_doc()
                .blocks
                .iter()
                .find_map(|block| {
                    let target = match block.content {
                        BlockContent::Markdown { .. } => TextTarget::Markdown(block.id),
                        BlockContent::Math { .. } => TextTarget::Math(block.id),
                        _ => return None,
                    };
                    known
                        .is_none_or(|known| known.get_block(block.id).is_none())
                        .then_some(target)
                })
        };
        if let Some(target) = missing {
            self.ensure_text_target(target);
        }
    }

    pub(crate) fn dispatch_text_edit(&self, target: TextTarget, before: &str, after: &str) {
        if !can_edit_workspace(&self.0.borrow()) || !document_ready(&self.0.borrow()) {
            return;
        }
        self.ensure_text_target(target);
        // If a keystroke never reached the model, the field is ahead of it and
        // this edit's range no longer fits; diff against the model instead.
        let model_text = {
            let inner = self.0.borrow();
            inner
                .text_client_state
                .as_ref()
                .filter(|state| state.document().id == inner.selected_doc_id)
                .and_then(|state| target_text(&state.optimistic_document(), target))
        };
        let Some(mut edit) = minimal_text_edit(before, after) else {
            return;
        };
        if let Some(model_text) = model_text.filter(|text| {
            text.as_str() != before && edit.end_utf16 as usize > text.encode_utf16().count()
        }) {
            match minimal_text_edit(&model_text, after) {
                Some(repair) => edit = repair,
                None => return,
            }
        }
        if !matches!(target, TextTarget::Title) {
            self.record_change(page_history::ChangeKind::Typing(target));
        }
        let effects = {
            let mut inner = self.0.borrow_mut();
            let selected = inner.selected_doc_id;
            let Some(state) = inner
                .text_client_state
                .as_mut()
                .filter(|state| state.document().id == selected)
            else {
                return;
            };
            state.transition(TextClientEvent::LocalEdit {
                target,
                edit,
                update_id: UpdateId::new_v4(),
                applied_at_ms: js_sys::Date::now() as i64,
            })
        };
        for effect in effects {
            self.execute_text_effect(effect);
        }
        if let Some((selection, _, _, _)) = self.capture_relative_selection(target) {
            self.queue_presence(PresenceKind::Text { selection });
        }
    }

    fn capture_relative_selection(
        &self,
        target: TextTarget,
    ) -> Option<(RelativeSelection, String, i32, i32)> {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return None;
        };
        let Some(active) = document.active_element() else {
            return None;
        };
        let (active_target, start, end, direction) = if active.class_name()
            == "document-title-input"
        {
            let input: web_sys::HtmlInputElement = active.clone().unchecked_into();
            (
                TextTarget::Title,
                input.selection_start().ok().flatten(),
                input.selection_end().ok().flatten(),
                input
                    .selection_direction()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "none".into()),
            )
        } else if active.class_list().contains("block-textarea") {
            let textarea: web_sys::HtmlTextAreaElement = active.clone().unchecked_into();
            let Some(block_id) = textarea
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
            else {
                return None;
            };
            let target = if textarea.get_attribute("data-block-kind").as_deref() == Some("math") {
                TextTarget::Math(block_id)
            } else {
                TextTarget::Markdown(block_id)
            };
            (
                target,
                textarea.selection_start().ok().flatten(),
                textarea.selection_end().ok().flatten(),
                textarea
                    .selection_direction()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "none".into()),
            )
        } else {
            return None;
        };
        if active_target != target {
            return None;
        }
        let (Some(start), Some(end)) = (start, end) else {
            return None;
        };
        let mut inner = self.0.borrow_mut();
        let Some(state) = inner.text_client_state.as_mut() else {
            return None;
        };
        state
            .relative_selection(target, start, end)
            .ok()
            .map(|selection| {
                (
                    selection,
                    direction,
                    active.scroll_left(),
                    active.scroll_top(),
                )
            })
    }

    fn project_remote_text_update(&self, update: DocumentUpdate) -> DocumentUpdate {
        let target = match &update.op {
            DocumentOp::ApplyTextUpdateV2 { payload } => payload.target,
            _ => {
                let mut inner = self.0.borrow_mut();
                if let Some(state) = inner.text_client_state.as_mut() {
                    let mut structure = state.confirmed_document().clone();
                    if matches!(
                        inkstone_core::sequenced_replay(&mut structure, update.clone()),
                        inkstone_core::ReplayOutcome::Applied
                            | inkstone_core::ReplayOutcome::Adjusted { .. }
                    ) {
                        state.adopt_structure(&structure);
                    }
                }
                return update;
            }
        };
        let selection = self.capture_relative_selection(target);
        self.0
            .borrow_mut()
            .remote_selection_queue
            .push_back(selection);
        let effects = {
            let mut inner = self.0.borrow_mut();
            inner
                .text_client_state
                .as_mut()
                .map(|state| {
                    state.transition(TextClientEvent::RemoteUpdate {
                        update: update.clone(),
                    })
                })
                .unwrap_or_default()
        };
        for effect in effects {
            self.execute_text_effect(effect);
        }
        let title = self.0.borrow().client_state.confirmed_doc().title.clone();
        DocumentUpdate {
            version: inkstone_core::DOCUMENT_UPDATE_VERSION,
            update_id: update.update_id,
            doc_id: update.doc_id,
            applied_at_ms: update.applied_at_ms,
            op: DocumentOp::SetTitle { title },
        }
    }

    /// The server drops updates past its per-second limit; send whatever is
    /// still unconfirmed again once the window has passed.
    fn resend_pending_text_soon(&self) {
        if RESEND_SCHEDULED.with(|scheduled| scheduled.replace(true)) {
            return;
        }
        let this = self.clone();
        let resend = Closure::once_into_js(move || {
            RESEND_SCHEDULED.with(|scheduled| scheduled.set(false));
            let effects = {
                let mut inner = this.0.borrow_mut();
                if inner.client_state.phase() != SessionPhase::Live {
                    return;
                }
                inner
                    .text_client_state
                    .as_mut()
                    .map(|state| state.transition(TextClientEvent::Connected))
                    .unwrap_or_default()
            };
            for effect in effects {
                this.execute_text_effect(effect);
            }
        });
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                resend.unchecked_ref(),
                1_100,
            );
        }
    }

    /// Writes a text update to this device's copy of the note.
    fn persist_text_update(&self, update: DocumentUpdate) {
        let this = self.clone();
        let doc_id = update.doc_id;
        let update_id = update.update_id;
        let storage_key = document_storage_key(this.0.borrow().workspace_id, doc_id);
        let snapshot = this
            .0
            .borrow()
            .text_client_state
            .as_ref()
            .map(TextClientState::persistence_snapshot);
        let text_document = this
            .0
            .borrow()
            .text_client_state
            .as_ref()
            .map(|state| state.document().clone());
        wasm_bindgen_futures::spawn_local(async move {
            let Some(db) = this.0.borrow().db.clone() else {
                let effects = this
                    .0
                    .borrow_mut()
                    .text_client_state
                    .as_mut()
                    .map(|state| {
                        state
                            .transition(TextClientEvent::LocalUpdatePersistenceFailed { update_id })
                    })
                    .unwrap_or_default();
                for effect in effects {
                    this.execute_text_effect(effect);
                }
                this.render();
                return;
            };
            let result = db
                .update_document_record(&storage_key, move |current| {
                    let mut record = current
                        .map(|json| storage::deserialize_record(&json))
                        .transpose()
                        .map_err(|error| JsValue::from_str(&error.to_string()))?
                        .unwrap_or_else(|| storage::new_record(doc_id));
                    if let (Some(document), Some(snapshot)) = (text_document, snapshot) {
                        storage::apply_text_checkpoint(
                            &mut record,
                            document,
                            snapshot,
                            vec![update],
                            None,
                        );
                    } else {
                        storage::append_pending_update(&mut record, update);
                    }
                    serde_json::to_string(&record)
                        .map_err(|error| JsValue::from_str(&error.to_string()))
                })
                .await;
            let event = if result.is_ok() {
                TextClientEvent::LocalUpdatePersisted { update_id }
            } else {
                TextClientEvent::LocalUpdatePersistenceFailed { update_id }
            };
            let effects = this
                .0
                .borrow_mut()
                .text_client_state
                .as_mut()
                .map(|state| state.transition(event))
                .unwrap_or_default();
            for effect in effects {
                this.execute_text_effect(effect);
            }
            if result.is_ok() && !this.0.borrow().client_state.local_persistence_failed() {
                let mut inner = this.0.borrow_mut();
                inner.persistence_status = "Saved".to_string();
                inner.local_persistence_error = None;
            }
            if result.is_ok() {
                let current_path = this.0.borrow().selected_path.clone();
                if let Some(path) = current_path {
                    this.update_search_index_for_active_doc(&path);
                }
                this.resume_deferred_navigation();
                this.refresh_status_in_place();
            } else {
                this.render();
            }
        });
    }

    fn execute_text_effect(&self, effect: TextClientEffect) {
        let this = self.clone();
        match effect {
            TextClientEffect::PersistLocalUpdate { update } => {
                if self.structure_saving() {
                    self.0.borrow_mut().deferred_text_persist = Some(update);
                    return;
                }
                this.persist_text_update(update);
            }

            TextClientEffect::PersistRemoteMerge { token, checkpoint } => {
                let echo = self
                    .0
                    .borrow()
                    .text_client_state
                    .as_ref()
                    .is_some_and(|state| {
                        state
                            .pending_updates()
                            .iter()
                            .any(|update| update.update_id == checkpoint.merged_update_id)
                    });
                let doc_id = checkpoint.document.id;
                let storage_key = document_storage_key(this.0.borrow().workspace_id, doc_id);
                wasm_bindgen_futures::spawn_local(async move {
                    let db = this.0.borrow().db.clone();
                    let result = if let Some(db) = db {
                        db.update_document_record(&storage_key, move |current| {
                            let mut record = current
                                .map(|json| storage::deserialize_record(&json))
                                .transpose()
                                .map_err(|error| JsValue::from_str(&error.to_string()))?
                                .unwrap_or_else(|| storage::new_record(doc_id));
                            storage::apply_text_checkpoint(
                                &mut record,
                                checkpoint.document,
                                checkpoint.text_snapshot,
                                Vec::new(),
                                Some(checkpoint.merged_update_id),
                            );
                            storage::note_edit(&mut record, js_sys::Date::now() as i64);
                            serde_json::to_string(&record)
                                .map_err(|error| JsValue::from_str(&error.to_string()))
                        })
                        .await
                    } else {
                        Err(JsValue::from_str(
                            "IndexedDB handle unavailable for text merge",
                        ))
                    };
                    let event = if result.is_ok() {
                        TextClientEvent::RemoteMergePersisted { token }
                    } else {
                        TextClientEvent::RemoteMergePersistenceFailed { token }
                    };
                    let effects = this
                        .0
                        .borrow_mut()
                        .text_client_state
                        .as_mut()
                        .map(|state| state.transition(event))
                        .unwrap_or_default();
                    if result.is_ok() {
                        let selection = this
                            .0
                            .borrow_mut()
                            .remote_selection_queue
                            .pop_front()
                            .flatten();
                        if !echo {
                            this.0.borrow_mut().pending_relative_selection = selection;
                        }
                    }
                    for effect in effects {
                        this.execute_text_effect(effect);
                    }
                    if echo && result.is_ok() {
                        this.refresh_status_in_place();
                    } else {
                        this.render();
                    }
                });
            }
            TextClientEffect::SendUpdate(update) => self.execute_effect(Effect::SendUpdate(update)),
            TextClientEffect::NotifyError(message) => {
                let is_persistence_failure = self
                    .0
                    .borrow()
                    .text_client_state
                    .as_ref()
                    .is_some_and(TextClientState::local_persistence_failed);
                if is_persistence_failure {
                    let message = format!("Failed to persist local update: {message}");
                    web_sys::console::error_1(&JsValue::from_str(&message));
                    let mut inner = self.0.borrow_mut();
                    inner.persistence_status = "Save failed".to_string();
                    inner.local_persistence_error = Some(message);
                } else {
                    self.0.borrow_mut().sync_error = Some(message);
                }
                self.render();
            }
        }
    }

    pub fn dispatch(&self, event: ClientEvent) {
        if !client_event_allowed(&self.0.borrow(), &event) {
            self.set_sync_error("This workspace is read-only".into());
            return;
        }
        if let ClientEvent::LocalIntent { op, .. } = &event {
            let structural = match op {
                DocumentOp::InsertBlock { block, .. } => {
                    !matches!(block.content, BlockContent::Ink { .. })
                }
                DocumentOp::ReplaceBlockContent { content, .. } => {
                    !matches!(content, BlockContent::Ink { .. })
                }
                DocumentOp::DeleteBlock { .. } | DocumentOp::MoveBlock { .. } => true,
                _ => false,
            };
            if structural {
                self.record_change(page_history::ChangeKind::Structure);
            }
        }
        let quiet = {
            let mut inner = self.0.borrow_mut();
            let quiet = match &event {
                ClientEvent::SequencedUpdate { update, .. } => {
                    let id = update.update_id;
                    inner
                        .client_state
                        .pending_local()
                        .iter()
                        .any(|p| p.update_id == id)
                        || inner.text_client_state.as_ref().is_some_and(|text| {
                            text.pending_updates().iter().any(|p| p.update_id == id)
                        })
                }
                ClientEvent::ConfirmationPersisted { .. } => inner.quiet_dispatch,
                ClientEvent::LocalUpdatePersisted { .. } => matches!(
                    inner.client_state.pending_intent_op(),
                    Some(
                        DocumentOp::ApplyPageInkUpdate { .. }
                            | DocumentOp::SetInkAnchor { .. }
                            | DocumentOp::RemoveInkAnchors { .. }
                    )
                ),
                _ => false,
            };
            inner.quiet_dispatch = quiet && matches!(event, ClientEvent::SequencedUpdate { .. });
            quiet
        };
        let connected = matches!(event, ClientEvent::Ready { .. });
        let text_connection_event = match &event {
            ClientEvent::Ready { .. } => Some(TextClientEvent::Connected),
            ClientEvent::SocketDisconnected { .. } | ClientEvent::Close => {
                Some(TextClientEvent::Disconnected)
            }
            _ => None,
        };

        let switches_document =
            matches!(event, ClientEvent::OpenDocument { .. } | ClientEvent::Close);
        if switches_document {
            // A held text save still belongs to the note being left.
            let deferred = self.0.borrow_mut().deferred_text_persist.take();
            if let Some(update) = deferred {
                self.persist_text_update(update);
            }
        }
        let effects = {
            let mut inner = self.0.borrow_mut();
            let effects = inner.client_state.transition(event);
            if connected && inner.client_state.phase() != SessionPhase::Error {
                inner.sync_error = None;
            }
            if switches_document {
                // The previous note's text must never take edits meant for
                // the one opening; it gets its own once it has loaded.
                inner.text_client_state = None;
                inner.editing_block = None;
                inner.pending_block_caret = None;
                inner.slash_menu = None;
            }
            effects
        };
        if let Some(event) = text_connection_event {
            let text_effects = self
                .0
                .borrow_mut()
                .text_client_state
                .as_mut()
                .map(|state| state.transition(event))
                .unwrap_or_default();
            for effect in text_effects {
                self.execute_text_effect(effect);
            }
        }

        let is_rendering = RENDERING.with(|rendering| rendering.get());
        let should_render = !is_rendering && {
            let inner = self.0.borrow();
            let state = &inner.client_state;
            !state.has_pending_intent() && state.intent_queue_len() == 0
        };

        if should_render {
            if quiet && self.0.borrow().client_state.phase() != SessionPhase::Error {
                self.refresh_status_in_place();
                let (view_doc, editable) = {
                    let inner = self.0.borrow();
                    (
                        Driver::effective_document(&inner),
                        can_edit_workspace(&inner) && inner.document_mode == DocumentMode::Edit,
                    )
                };
                self.sync_page_surface(&view_doc, editable);
            } else {
                self.render();
            }
        }

        for effect in effects {
            self.execute_effect(effect);
        }
        self.flush_deferred_text_persist();
    }

    /// Whether any edit has yet to reach this device's storage.
    fn has_unsaved_edits(&self) -> bool {
        self.structure_saving()
            || self
                .0
                .borrow()
                .text_client_state
                .as_ref()
                .is_some_and(TextClientState::has_pending_persistence)
    }

    fn structure_saving(&self) -> bool {
        let inner = self.0.borrow();
        inner.client_state.has_pending_intent() || inner.client_state.intent_queue_len() > 0
    }

    fn flush_deferred_text_persist(&self) {
        if self.structure_saving() {
            return;
        }
        let deferred = self.0.borrow_mut().deferred_text_persist.take();
        if let Some(update) = deferred {
            self.persist_text_update(update);
        }
    }

    fn execute_effect(&self, effect: Effect) {
        let this = self.clone();
        match effect {
            Effect::LoadLocalState { lifecycle, doc_id } => {
                let workspace_id = this.0.borrow().workspace_id;
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

                    let storage_key = document_storage_key(workspace_id, doc_id);
                    let mut record_result = db.get_document_record(&storage_key).await;
                    let workspace_key = this.0.borrow().workspace_key;
                    if let (Some(workspace_id), Some(key)) = (workspace_id, workspace_key) {
                        let local_seq = record_result
                            .as_ref()
                            .ok()
                            .and_then(|record| record.as_deref())
                            .and_then(|json| storage::deserialize_record(json).ok())
                            .map(|record| record.confirmed_seq)
                            .unwrap_or(0);
                        let endpoint = format!("/api/docs/{}/snapshot", doc_id.0);
                        if let Ok(snapshot) = api::json::<
                            inkstone_core::protocol::SnapshotResponse,
                            serde_json::Value,
                        >("GET", &endpoint, None)
                        .await
                            && snapshot.aad_version == crypto::AAD_VERSION
                            && snapshot.base_seq > local_seq
                        {
                            let hydrated = crypto::decrypt(
                                &key,
                                &snapshot.encrypted_snapshot_b64,
                                &snapshot.nonce_b64,
                                &crypto::snapshot_aad(workspace_id, doc_id.0, snapshot.base_seq),
                            )
                            .and_then(|bytes| {
                                serde_json::from_slice::<storage::SyncedDocumentSnapshot>(&bytes)
                                    .map_err(|error| format!("Invalid decrypted snapshot: {error}"))
                            });
                            match hydrated {
                                Ok(snapshot_payload)
                                    if snapshot_payload.version
                                        == storage::SYNCED_DOCUMENT_SNAPSHOT_VERSION
                                        && snapshot_payload.document.id == doc_id =>
                                {
                                    let (pending_updates, edited_at_ms) = record_result
                                        .as_ref()
                                        .ok()
                                        .and_then(|record| record.as_deref())
                                        .and_then(|json| storage::deserialize_record(json).ok())
                                        .map(|record| (record.pending_updates, record.edited_at_ms))
                                        .unwrap_or_default();
                                    let record = storage::LocalDocumentRecord {
                                        version: storage::LOCAL_DOCUMENT_RECORD_VERSION,
                                        doc_id,
                                        confirmed_seq: snapshot.base_seq,
                                        confirmed_doc: snapshot_payload.document.clone(),
                                        pending_updates,
                                        text_snapshot: Some(snapshot_payload.text_snapshot),
                                        text_projection: Some(snapshot_payload.document.clone()),
                                        edited_at_ms,
                                    };
                                    if let Ok(json) = serde_json::to_string(&record) {
                                        if db.put_document_record(&storage_key, &json).await.is_ok()
                                        {
                                            record_result = Ok(Some(json));
                                        }
                                    }
                                }
                                Ok(_) => {
                                    this.0.borrow_mut().document_error = Some(
                                        "The remote snapshot has an unsupported version or document identity"
                                            .into(),
                                    );
                                }
                                Err(error) => {
                                    this.0.borrow_mut().document_error =
                                        Some(format!("Remote snapshot was quarantined: {error}"));
                                }
                            }
                        }
                    }
                    if this.0.borrow().selected_doc_id != doc_id {
                        return;
                    }

                    match record_result {
                        Ok(Some(record_json)) => match storage::deserialize_record(&record_json) {
                            Ok(record) => {
                                let text_pending = record
                                    .pending_updates
                                    .iter()
                                    .filter(|update| {
                                        matches!(update.op, DocumentOp::ApplyTextUpdateV2 { .. })
                                    })
                                    .cloned()
                                    .collect::<Vec<_>>();
                                let document_pending = record
                                    .pending_updates
                                    .iter()
                                    .filter(|update| {
                                        !matches!(update.op, DocumentOp::ApplyTextUpdateV2 { .. })
                                    })
                                    .cloned()
                                    .collect::<Vec<_>>();
                                let text_snapshot = record.text_snapshot.clone();
                                let text_projection = record.text_projection.clone();
                                this.0.borrow_mut().persistence_status =
                                    "Loaded from IndexedDB".to_string();
                                this.dispatch(ClientEvent::LocalHydrationLoaded {
                                    lifecycle,
                                    confirmed_doc: Some(record.confirmed_doc),
                                    confirmed_seq: record.confirmed_seq,
                                    pending_updates: document_pending,
                                });
                                let mut text_document =
                                    this.0.borrow().client_state.effective_view_doc().clone();
                                if let Some(projection) = text_projection.as_ref() {
                                    storage::preserve_collaborative_text(
                                        &mut text_document,
                                        projection,
                                    );
                                }
                                if let Err(error) = this.initialize_text_state(
                                    text_document,
                                    text_snapshot,
                                    text_pending,
                                ) {
                                    this.0.borrow_mut().storage_error = Some(format!(
                                        "Collaborative text hydration failed: {error}"
                                    ));
                                    this.render();
                                    return;
                                }
                                this.complete_pending_recent(doc_id);
                            }
                            Err(e) => {
                                let err_str = format!("Parse Error: {:?}", e);
                                this.0.borrow_mut().persistence_status =
                                    format!("Error: {}", err_str);
                                this.0.borrow_mut().document_error = Some(format!(
                                    "This note's local record was quarantined. Other workspace notes remain available. {err_str}"
                                ));
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
                            let document = this.0.borrow().client_state.view_doc().clone();
                            if let Err(error) =
                                this.initialize_text_state(document, None, Vec::new())
                            {
                                this.0.borrow_mut().storage_error = Some(error);
                            }
                            this.complete_pending_recent(doc_id);

                            let should_add_welcome = {
                                let mut inner = this.0.borrow_mut();
                                let known_local_new = inner.locally_new_docs.remove(&doc_id);
                                inner.client_state.view_doc().blocks.is_empty()
                                    && (inner.local_mode || known_local_new)
                            };
                            if should_add_welcome {
                                let update_id = UpdateId::new_v4();
                                let applied_at_ms = js_sys::Date::now() as i64;
                                let block_id = BlockId::new_v4();
                                let block = inkstone_core::document::DocumentBlock::new(
                                    block_id,
                                    // A new note is a blank page, ready to type into.
                                    inkstone_core::document::BlockContent::Markdown {
                                        text: String::new(),
                                    },
                                );
                                this.dispatch(ClientEvent::LocalIntent {
                                    op: DocumentOp::InsertBlock { idx: 0, block },
                                    update_id,
                                    applied_at_ms,
                                });
                                // A blank page has nothing to undo.
                                this.clear_history();
                                this.render();
                                this.focus_new_note_title();
                            }
                        }
                        Err(e) => {
                            let err_str = format!("IndexedDB Read Error: {:?}", e);
                            this.0.borrow_mut().persistence_status = format!("Error: {}", err_str);
                            this.0.borrow_mut().document_error = Some(format!(
                                "This note could not be read and was quarantined: {err_str}"
                            ));
                            this.render();
                        }
                    }
                });
            }
            Effect::PersistLocalUpdate { update_id, update } => {
                let doc_id = update.doc_id;
                let storage_key = document_storage_key(this.0.borrow().workspace_id, doc_id);
                let ink_block_id = match &update.op {
                    DocumentOp::ReplaceBlockContent {
                        block_id,
                        content: BlockContent::Ink { .. },
                    } => Some(*block_id),
                    DocumentOp::ApplyInkUpdateV2 { payload } => Some(payload.block_id),
                    DocumentOp::InsertBlock { block, .. }
                        if matches!(&block.content, BlockContent::Ink { .. }) =>
                    {
                        Some(block.id)
                    }
                    _ => None,
                };
                let projected_ink = ink_block_id.and_then(|block_id| {
                    this.0
                        .borrow()
                        .client_state
                        .effective_view_doc()
                        .get_block(block_id)
                        .map(|block| block.content.clone())
                });
                if let Some(block_id) = ink_block_id {
                    let phase = this
                        .0
                        .borrow_mut()
                        .ink_save_tracker
                        .saving(doc_id, block_id, update_id);
                    if let Some(phase) = phase {
                        this.update_ink_save_phase(block_id, phase);
                    }
                }
                wasm_bindgen_futures::spawn_local(async move {
                    let db = {
                        let inner = this.0.borrow();
                        inner.db.clone()
                    };

                    let db = match db {
                        Some(d) => d,
                        None => {
                            this.handle_local_persistence_failure(
                                doc_id,
                                ink_block_id,
                                update_id,
                                "IndexedDB handle unavailable for update".to_string(),
                            );
                            return;
                        }
                    };

                    let result = db
                        .update_document_record(&storage_key, move |current| {
                            let mut record = match current {
                                Some(json) => storage::deserialize_record(&json)
                                    .map_err(|error| JsValue::from_str(&error.to_string()))?,
                                None => storage::new_record(doc_id),
                            };
                            if let (Some(block_id), Some(content)) = (ink_block_id, projected_ink)
                                && let Some(block) = record.confirmed_doc.get_block_mut(block_id)
                            {
                                block.content = content;
                            }
                            storage::append_pending_update(&mut record, update);
                            serde_json::to_string(&record)
                                .map_err(|error| JsValue::from_str(&error.to_string()))
                        })
                        .await;
                    match result {
                        Ok(()) => {
                            {
                                let mut inner = this.0.borrow_mut();
                                inner.persistence_status = "Saved".to_string();
                                inner.local_persistence_error = None;
                            }
                            this.finish_ink_update(
                                doc_id,
                                ink_block_id,
                                update_id,
                                InkSavePhase::Saved,
                            );
                            this.dispatch(ClientEvent::LocalUpdatePersisted { update_id });
                            let current_path = {
                                let inner = this.0.borrow();
                                (inner.selected_doc_id == doc_id)
                                    .then(|| inner.selected_path.clone())
                                    .flatten()
                            };
                            if let Some(path) = current_path {
                                this.update_search_index_for_active_doc(&path);
                            } else {
                                this.index_workspace();
                            }
                            this.resume_deferred_navigation();
                        }
                        Err(error) => this.handle_local_persistence_failure(
                            doc_id,
                            ink_block_id,
                            update_id,
                            format!("IndexedDB document update failed: {error:?}"),
                        ),
                    }
                });
            }
            Effect::PersistConfirmation { token, checkpoint } => {
                let lifecycle = this.0.borrow().client_state.lifecycle_id();
                let storage_key =
                    document_storage_key(this.0.borrow().workspace_id, checkpoint.doc_id);
                wasm_bindgen_futures::spawn_local(async move {
                    let doc_id = checkpoint.doc_id;
                    let db = {
                        let inner = this.0.borrow();
                        inner.db.clone()
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

                    let result = db
                        .update_document_record(&storage_key, move |current| {
                            let mut record = match current {
                                Some(json) => storage::deserialize_record(&json)
                                    .map_err(|error| JsValue::from_str(&error.to_string()))?,
                                None => storage::new_record(doc_id),
                            };
                            storage::apply_confirmation(&mut record, checkpoint);
                            serde_json::to_string(&record)
                                .map_err(|error| JsValue::from_str(&error.to_string()))
                        })
                        .await;
                    match result {
                        Ok(()) => {
                            let is_current = this
                                .0
                                .borrow()
                                .client_state
                                .is_confirmation_completion_current(lifecycle, doc_id, token);
                            if !is_current {
                                return;
                            }
                            let current_path = {
                                let inner = this.0.borrow();
                                (inner.selected_doc_id == doc_id)
                                    .then(|| inner.selected_path.clone())
                                    .flatten()
                            };
                            this.dispatch(ClientEvent::ConfirmationPersisted { token });
                            if let Some(path) = current_path {
                                this.update_search_index_for_active_doc(&path);
                            } else {
                                this.index_workspace();
                            }
                        }
                        Err(e) => {
                            let is_current = this
                                .0
                                .borrow()
                                .client_state
                                .is_confirmation_completion_current(lifecycle, doc_id, token);
                            if !is_current {
                                return;
                            }
                            this.0.borrow_mut().persistence_status =
                                "Failed to save confirmation".to_string();
                            this.0.borrow_mut().storage_error =
                                Some(format!("IndexedDB confirmation update failed: {e:?}"));
                            this.dispatch(ClientEvent::ConfirmationPersistenceFailed { token });
                            this.render();
                        }
                    }
                });
            }
            Effect::ConnectSocket {
                lifecycle,
                connection_id,
                doc_id,
            } => {
                if this.0.borrow().local_mode {
                    return;
                }
                let path = {
                    let inner = this.0.borrow();
                    (inner.selected_doc_id == doc_id)
                        .then(|| inner.selected_path.clone())
                        .flatten()
                };
                wasm_bindgen_futures::spawn_local(async move {
                    let Some(path) = path else {
                        this.set_sync_error(
                            "Cannot sync a document without a workspace path".into(),
                        );
                        return;
                    };
                    if let Err(error) = this.ensure_remote_doc(doc_id, &path).await {
                        let is_current = {
                            let inner = this.0.borrow();
                            inner.client_state.lifecycle_id() == lifecycle
                                && inner.client_state.active_connection_id() == Some(connection_id)
                                && inner.selected_doc_id == doc_id
                        };
                        if is_current {
                            this.set_sync_error(format!(
                                "Remote document bootstrap failed: {error}"
                            ));
                            this.dispatch(ClientEvent::SocketDisconnected {
                                lifecycle,
                                connection_id,
                            });
                        }
                        return;
                    }
                    let is_current = {
                        let inner = this.0.borrow();
                        inner.client_state.lifecycle_id() == lifecycle
                            && inner.client_state.active_connection_id() == Some(connection_id)
                            && inner.selected_doc_id == doc_id
                    };
                    if is_current {
                        if let Err(error) = this.connect_socket(lifecycle, connection_id, doc_id) {
                            this.set_sync_error(error);
                            this.dispatch(ClientEvent::SocketDisconnected {
                                lifecycle,
                                connection_id,
                            });
                        }
                    }
                });
            }
            Effect::DisconnectSocket => this.disconnect_socket(),
            Effect::FetchUpdates {
                lifecycle,
                fetch_id,
                purpose: _,
                doc_id,
                after_seq,
                through_seq,
                limit,
            } => {
                let context = this.socket_context();
                wasm_bindgen_futures::spawn_local(async move {
                    let (workspace_id, key, _) = match context {
                        Ok(context) => context,
                        Err(error) => {
                            this.dispatch(ClientEvent::CatchUpFailed {
                                lifecycle,
                                fetch_id,
                                reason: error,
                            });
                            return;
                        }
                    };
                    let endpoint = format!(
                        "/api/docs/{}/updates?after_seq={after_seq}&through_seq={through_seq}&limit={limit}",
                        doc_id.0
                    );
                    let response = api::json::<
                        PaginatedResponse<EncryptedUpdateResponse>,
                        serde_json::Value,
                    >("GET", &endpoint, None)
                    .await;
                    let encrypted_items = match response {
                        Ok(response) => response.items,
                        Err(error) => {
                            this.dispatch(ClientEvent::CatchUpFailed {
                                lifecycle,
                                fetch_id,
                                reason: error.message,
                            });
                            return;
                        }
                    };
                    let mut items = Vec::with_capacity(encrypted_items.len());
                    for outer in encrypted_items {
                        if outer.aad_version != crypto::AAD_VERSION {
                            this.dispatch(ClientEvent::CatchUpFailed {
                                lifecycle,
                                fetch_id,
                                reason: "Unsupported update AAD version".into(),
                            });
                            return;
                        }
                        let update = crypto::decrypt(
                            &key,
                            &outer.encrypted_update_b64,
                            &outer.nonce_b64,
                            &crypto::update_aad(workspace_id, doc_id.0, outer.update_id.0),
                        )
                        .map_err(|error| format!("Update decryption failed: {error}"))
                        .and_then(|bytes| {
                            serde_json::from_slice::<inkstone_core::DocumentUpdate>(&bytes)
                                .map_err(|error| format!("Invalid decrypted update: {error}"))
                        });
                        let update = match update {
                            Ok(update)
                                if update.doc_id == doc_id
                                    && update.update_id == outer.update_id =>
                            {
                                update
                            }
                            Ok(_) => {
                                this.dispatch(ClientEvent::CatchUpFailed {
                                    lifecycle,
                                    fetch_id,
                                    reason: "Encrypted update identity mismatch".into(),
                                });
                                return;
                            }
                            Err(reason) => {
                                this.dispatch(ClientEvent::CatchUpFailed {
                                    lifecycle,
                                    fetch_id,
                                    reason,
                                });
                                return;
                            }
                        };
                        let projected = this.project_remote_text_update(update.clone());
                        this.schedule_title_projection(
                            workspace_id,
                            key,
                            doc_id,
                            &update,
                            outer.seq,
                        );
                        items.push(inkstone_client::EncryptedUpdateMeta {
                            seq: outer.seq,
                            update: projected,
                        });
                    }
                    this.dispatch(ClientEvent::CatchUpPageLoaded {
                        lifecycle,
                        fetch_id,
                        items,
                    });
                });
            }
            Effect::SendUpdate(update) => {
                let context = this.socket_context();
                let socket = this
                    .2
                    .borrow()
                    .socket
                    .as_ref()
                    .map(|binding| binding.socket.clone());
                match (context, socket) {
                    (Ok((workspace_id, key, _)), Some(socket))
                        if socket.ready_state() == web_sys::WebSocket::OPEN =>
                    {
                        let result = serde_json::to_vec(&update)
                            .map_err(|error| error.to_string())
                            .and_then(|plaintext| {
                                crypto::encrypt(
                                    &key,
                                    &plaintext,
                                    &crypto::update_aad(
                                        workspace_id,
                                        update.doc_id.0,
                                        update.update_id.0,
                                    ),
                                )
                            })
                            .and_then(|encrypted| {
                                serde_json::to_string(&ClientWsMessage::EncryptedUpdate {
                                    doc_id: update.doc_id.0,
                                    update_id: update.update_id,
                                    encrypted_update_b64: encrypted.ciphertext_b64,
                                    nonce_b64: encrypted.nonce_b64,
                                    aad_version: crypto::AAD_VERSION,
                                })
                                .map_err(|error| error.to_string())
                            })
                            .and_then(|message| {
                                socket
                                    .send_with_str(&message)
                                    .map_err(|error| format!("WebSocket send failed: {error:?}"))
                            });
                        if let Err(error) = result {
                            this.set_sync_error(error);
                            let _ = socket.close();
                        }
                    }
                    (Err(error), _) => this.set_sync_error(error),
                    _ => this.set_sync_error("Cannot send update while WebSocket is closed".into()),
                }
            }
            Effect::UploadSnapshot(snapshot) => {
                let context = this.socket_context();
                let text_checkpoint = this.0.borrow().text_client_state.as_ref().map(|state| {
                    (
                        state.confirmed_document().clone(),
                        state.confirmed_snapshot(),
                    )
                });
                wasm_bindgen_futures::spawn_local(async move {
                    let (workspace_id, key, _) = match context {
                        Ok(context) => context,
                        Err(error) => {
                            this.set_sync_error(error);
                            return;
                        }
                    };
                    let Some((text_document, text_snapshot)) = text_checkpoint else {
                        this.set_sync_error(
                            "Snapshot upload deferred until text CRDT state is hydrated".into(),
                        );
                        return;
                    };
                    let mut document = snapshot.confirmed_doc;
                    storage::preserve_collaborative_text(&mut document, &text_document);
                    let payload = storage::SyncedDocumentSnapshot {
                        version: storage::SYNCED_DOCUMENT_SNAPSHOT_VERSION,
                        document,
                        text_snapshot,
                    };
                    let plaintext = match serde_json::to_vec(&payload) {
                        Ok(plaintext) => plaintext,
                        Err(error) => {
                            this.set_sync_error(format!("Snapshot encoding failed: {error}"));
                            return;
                        }
                    };
                    let encrypted = match crypto::encrypt(
                        &key,
                        &plaintext,
                        &crypto::snapshot_aad(workspace_id, snapshot.doc_id.0, snapshot.base_seq),
                    ) {
                        Ok(encrypted) => encrypted,
                        Err(error) => {
                            this.set_sync_error(error);
                            return;
                        }
                    };
                    let endpoint = format!("/api/docs/{}/snapshot", snapshot.doc_id.0);
                    if let Err(error) = api::json::<inkstone_core::protocol::SnapshotResponse, _>(
                        "POST",
                        &endpoint,
                        Some(&PostSnapshotRequest {
                            encrypted_snapshot_b64: encrypted.ciphertext_b64,
                            nonce_b64: encrypted.nonce_b64,
                            base_seq: snapshot.base_seq,
                            aad_version: crypto::AAD_VERSION,
                        }),
                    )
                    .await
                    {
                        this.set_sync_error(format!("Snapshot upload failed: {}", error.message));
                    }
                });
            }
            Effect::ScheduleReconnect {
                lifecycle,
                reconnect_token,
                delay_ms,
            } => {
                if this.0.borrow().local_mode {
                    return;
                }
                if let Some((timeout, _)) = this.2.borrow_mut().reconnect_timeout.take() {
                    if let Some(window) = web_sys::window() {
                        window.clear_timeout_with_handle(timeout);
                    }
                }
                let timer_driver = this.clone();
                let callback = Closure::wrap(Box::new(move || {
                    timer_driver.2.borrow_mut().reconnect_timeout = None;
                    timer_driver.dispatch(ClientEvent::ReconnectTimerFired {
                        lifecycle,
                        reconnect_token,
                    });
                }) as Box<dyn FnMut()>);
                match web_sys::window()
                    .unwrap()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        callback.as_ref().unchecked_ref(),
                        delay_ms.min(i32::MAX as u64) as i32,
                    ) {
                    Ok(timeout) => {
                        this.2.borrow_mut().reconnect_timeout = Some((timeout, callback));
                    }
                    Err(error) => {
                        this.set_sync_error(format!("Failed to schedule reconnect: {error:?}"))
                    }
                }
            }
            Effect::CancelReconnect => {
                if let Some((timeout, _)) = this.2.borrow_mut().reconnect_timeout.take() {
                    if let Some(window) = web_sys::window() {
                        window.clear_timeout_with_handle(timeout);
                    }
                }
            }
            Effect::NotifyError(msg) => {
                web_sys::console::error_1(&JsValue::from_str(&msg));
                this.0.borrow_mut().sync_error = Some(msg);
                this.render();
            }
        }
    }

    /// While text is focused the page is not rebuilt; status that changes
    /// on its own (sync phase, saving, Live mode) is updated where it is.
    fn refresh_status_in_place(&self) {
        ui::publish(self, &self.0.borrow());
    }

    fn reconcile_focused_text_controls(&self) -> bool {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return false;
        };
        let Some(active) = document.active_element() else {
            return false;
        };
        let active_is_title = active.class_name() == "document-title-input";
        let active_is_block = active.class_list().contains("block-textarea");
        if !active_is_title && !active_is_block {
            return false;
        }
        let focused = active.clone();

        let (view, composing, relative_selection, disabled, local_ahead) = {
            let mut inner = self.0.borrow_mut();
            let composing = inner.composing_text_target;
            // Edits still queued for persistence are in the DOM but not yet in
            // the view document; writing the view back would drop them.
            let local_ahead = inner
                .text_client_state
                .as_ref()
                .is_some_and(TextClientState::has_pending_persistence);
            let relative_selection = if composing.is_none() {
                inner.pending_relative_selection.take()
            } else {
                None
            };
            (
                Driver::effective_document(&inner),
                composing,
                relative_selection,
                inner.local_persistence_error.is_some(),
                local_ahead,
            )
        };
        if let Ok(Some(element)) = document.query_selector(".document-title-input") {
            let input: web_sys::HtmlInputElement = element.unchecked_into();
            input.set_disabled(disabled);
            if !(active_is_title && (composing == Some(TextTarget::Title) || local_ahead)) {
                if input.value() != view.title {
                    input.set_value(&view.title);
                }
                input
                    .set_attribute("data-canonical-value", &view.title)
                    .ok();
            }
        }
        for block in &view.blocks {
            let value = match &block.content {
                BlockContent::Markdown { text } => Some(text.as_str()),
                BlockContent::Math { source } => Some(source.as_str()),
                _ => None,
            };
            let Some(value) = value else { continue };
            let selector = format!(".block-textarea[data-block-id=\"{}\"]", block.id);
            let Ok(Some(element)) = document.query_selector(&selector) else {
                continue;
            };
            let textarea: web_sys::HtmlTextAreaElement = element.unchecked_into();
            textarea.set_disabled(disabled);
            let target = match block.content {
                BlockContent::Markdown { .. } => TextTarget::Markdown(block.id),
                BlockContent::Math { .. } => TextTarget::Math(block.id),
                _ => unreachable!(),
            };
            let is_active = active_is_block
                && textarea.get_attribute("data-block-id") == active.get_attribute("data-block-id");
            if !(is_active && (composing == Some(target) || local_ahead)) {
                if textarea.value() != value {
                    textarea.set_value(value);
                }
                textarea.set_attribute("data-canonical-value", value).ok();
            }
        }

        if let Some((selection, direction, scroll_left, scroll_top)) = relative_selection {
            let resolved = self
                .0
                .borrow()
                .text_client_state
                .as_ref()
                .and_then(|state| {
                    Some((
                        state.resolve_position(&selection.anchor).ok()?,
                        state.resolve_position(&selection.focus).ok()?,
                        selection.anchor.target,
                    ))
                });
            if let Some((start, end, target)) = resolved {
                match target {
                    TextTarget::Title if active_is_title => {
                        let input: web_sys::HtmlInputElement = active.unchecked_into();
                        let _ = input.set_selection_range_with_direction(start, end, &direction);
                        input.set_scroll_left(scroll_left);
                        input.set_scroll_top(scroll_top);
                    }
                    TextTarget::Markdown(block_id) | TextTarget::Math(block_id)
                        if active.get_attribute("data-block-id").as_deref()
                            == Some(&block_id.to_string()) =>
                    {
                        let textarea: web_sys::HtmlTextAreaElement = active.unchecked_into();
                        let _ = textarea.set_selection_range_with_direction(start, end, &direction);
                        textarea.set_scroll_left(scroll_left);
                        textarea.set_scroll_top(scroll_top);
                    }
                    _ => {}
                }
            }
        }
        self.reconcile_page_blocks(&document, &focused);
        true
    }

    fn hydrate_markdown_assets(&self, document: &web_sys::Document) {
        let (Some(workspace_db), workspace_id) = ({
            let inner = self.0.borrow();
            (inner.workspace_db.clone(), inner.workspace_id)
        }) else {
            return;
        };
        let Ok(nodes) = document.query_selector_all("img[data-asset-path]") else {
            return;
        };
        let now = js_sys::Date::now();
        let mut pending = Vec::new();
        for index in 0..nodes.length() {
            let Some(element) = nodes
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            let Some(path_value) = element.get_attribute("data-asset-path") else {
                continue;
            };
            if pdf_pages::is_pdf(&path_value) {
                self.mount_pdf(&element, &path_value);
                continue;
            }
            let key = format!("{workspace_id:?}/{path_value}");
            let check = ASSET_URLS.with(|cache| {
                let mut cache = cache.borrow_mut();
                match cache.get_mut(&key) {
                    Some(entry) => {
                        if element.get_attribute("src").as_deref() != Some(&entry.url) {
                            let _ = element.set_attribute("src", &entry.url);
                        }
                        if entry.checking || now - entry.checked_at_ms < ASSET_RECHECK_MS {
                            false
                        } else {
                            entry.checking = true;
                            true
                        }
                    }
                    None => true,
                }
            });
            if check && !pending.iter().any(|(pending_key, _)| pending_key == &key) {
                pending.push((key, path_value));
            }
        }
        for (key, path_value) in pending {
            let Ok(path) = WorkspacePath::new(&path_value) else {
                continue;
            };
            ASSET_URLS.with(|cache| {
                if let Some(entry) = cache.borrow_mut().get_mut(&key) {
                    entry.checking = true;
                }
            });
            let workspace_db = workspace_db.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let bytes = workspace_db.read_file(&path).await.ok().flatten();
                let Some(bytes) = bytes else {
                    ASSET_URLS.with(|cache| {
                        if let Some(entry) = cache.borrow_mut().get_mut(&key) {
                            entry.checking = false;
                        }
                    });
                    return;
                };
                let fingerprint = asset_fingerprint(&bytes);
                let now = js_sys::Date::now();
                let unchanged = ASSET_URLS.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    match cache.get_mut(&key) {
                        Some(entry) if entry.fingerprint == fingerprint => {
                            entry.checking = false;
                            entry.checked_at_ms = now;
                            Some(entry.url.clone())
                        }
                        _ => None,
                    }
                });
                let url = match unchanged {
                    Some(url) => url,
                    None => {
                        let Ok(url) = create_blob_url(&bytes, &asset_media_type(path.as_str()))
                        else {
                            return;
                        };
                        let previous = ASSET_URLS.with(|cache| {
                            cache.borrow_mut().insert(
                                key.clone(),
                                AssetUrl {
                                    url: url.clone(),
                                    fingerprint,
                                    checked_at_ms: now,
                                    checking: false,
                                },
                            )
                        });
                        if let Some(previous) = previous {
                            let _ = web_sys::Url::revoke_object_url(&previous.url);
                        }
                        url
                    }
                };
                if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                    set_asset_src(&document, &path_value, &url);
                }
            });
        }
    }

    fn render(&self) {
        if self.reconcile_focused_text_controls() {
            self.refresh_status_in_place();
            return;
        }
        RENDERING.with(|rendering| rendering.set(true));
        self.flush_ink_runtimes();
        let window = web_sys::window().expect("No global window found");
        let document = window.document().expect("No global document found");
        let root = document
            .get_element_by_id("root")
            .expect("No element with id 'root' found");

        let active_element = document.active_element();
        let control_focus_key = active_element
            .as_ref()
            .and_then(|element| element.get_attribute("data-focus-key"));
        let focus_info = active_element.and_then(|el| {
            let class = el.class_name();
            if class == "document-title-input" {
                let input: web_sys::HtmlInputElement = el.unchecked_into();
                Some((
                    "title".to_string(),
                    input.selection_start().ok().flatten().unwrap_or(0),
                    input.selection_end().ok().flatten().unwrap_or(0),
                    input
                        .selection_direction()
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "none".into()),
                    input.scroll_left(),
                    input.scroll_top(),
                ))
            } else if el.class_list().contains("block-textarea") {
                let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                let block_id = textarea.get_attribute("data-block-id").unwrap_or_default();
                Some((
                    format!("block:{}", block_id),
                    textarea.selection_start().ok().flatten().unwrap_or(0),
                    textarea.selection_end().ok().flatten().unwrap_or(0),
                    textarea
                        .selection_direction()
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "none".into()),
                    textarea.scroll_left(),
                    textarea.scroll_top(),
                ))
            } else {
                None
            }
        });

        // Full renders rebuild every scroller; keep where the reader was.
        let scroll_positions = RETAINED_SCROLLERS
            .iter()
            .filter_map(|selector| {
                let element = document.query_selector(selector).ok().flatten()?;
                let top = element.scroll_top();
                (top > 0).then(|| (*selector, top, element.get_attribute("data-scroll-key")))
            })
            .collect::<Vec<_>>();
        let held_page_height = scroll_positions
            .iter()
            .find(|(selector, _, _)| *selector == ".main-content")
            .and_then(|(_, _, key)| {
                let surface = document.query_selector(".page-surface").ok().flatten()?;
                Some((key.clone(), surface.get_bounding_client_rect().height()))
            });

        let app_container = ui::shell(&root);

        let requested_focus = self.0.borrow_mut().pending_focus_key.take();
        let relative_focus = self
            .0
            .borrow_mut()
            .pending_relative_selection
            .take()
            .and_then(|(selection, direction, scroll_left, scroll_top)| {
                let inner = self.0.borrow();
                let state = inner.text_client_state.as_ref()?;
                let start = state.resolve_position(&selection.anchor).ok()?;
                let end = state.resolve_position(&selection.focus).ok()?;
                let target = match selection.anchor.target {
                    TextTarget::Title => "title".to_string(),
                    TextTarget::Markdown(block_id) | TextTarget::Math(block_id) => {
                        format!("block:{block_id}")
                    }
                };
                Some((target, start, end, direction, scroll_left, scroll_top))
            });
        let focus_info = relative_focus.or(focus_info);
        let mut inner = self.0.borrow().clone();
        ui::publish(self, &inner);
        if app_container.hidden() {
            if let Some(key) = requested_focus {
                ui::focus_soon(&key);
            }
            RENDERING.with(|rendering| rendering.set(false));
            return;
        }
        let client_state = &inner.client_state;
        let view_doc = Driver::effective_document(&inner);
        let phase = client_state.phase();
        let storage_error = &inner.storage_error;
        let entries = &inner.entries;
        let selected_path = &inner.selected_path;

        let can_read_workspace = can_read_workspace(&inner);
        let can_edit_workspace = can_edit_workspace(&inner);
        if !can_edit_workspace {
            inner.document_mode = DocumentMode::Preview;
        }
        let document_available = can_read_workspace
            && storage_error.is_none()
            && phase != SessionPhase::HydratingLocal
            && phase != SessionPhase::Closed
            && phase != SessionPhase::Error;

        let main_content = app_container.children().item(1).unwrap();
        let page = main_content.last_element_child().unwrap();
        page.set_text_content(None);
        let _ = main_content.set_attribute("data-scroll-key", &inner.selected_doc_id.to_string());
        main_content.set_class_name(if inner.document_mode == DocumentMode::Preview {
            "main-content preview-mode"
        } else {
            "main-content"
        });

        if let Some(err) = &inner.document_error {
            let err_div = document.create_element("div").unwrap();
            err_div.set_class_name("document-error");
            err_div.set_attribute("role", "alert").unwrap();
            err_div.set_text_content(Some(err));
            page.append_child(&err_div).unwrap();
        } else if let Some(err) = storage_error {
            let err_div = document.create_element("div").unwrap();
            err_div.set_class_name("storage-error");
            let err_p = document.create_element("p").unwrap();
            err_p.set_text_content(Some(&format!("Local storage error: {}", err)));
            err_div.append_child(&err_p).unwrap();
            page.append_child(&err_div).unwrap();
        } else if phase == SessionPhase::HydratingLocal {
            let loading_div = document.create_element("div").unwrap();
            loading_div.set_class_name("loading-state");
            loading_div.set_text_content(Some("Loading local document..."));
            page.append_child(&loading_div).unwrap();
        } else if phase == SessionPhase::Closed && selected_path.is_some() {
            let closed_div = document.create_element("div").unwrap();
            closed_div.set_class_name("closed-state");
            closed_div.set_text_content(Some("No document open."));
            page.append_child(&closed_div).unwrap();
        }

        if let Some(error) = &inner.local_persistence_error {
            let recovery = document.create_element("div").unwrap();
            recovery.set_class_name("save-recovery");
            recovery.set_attribute("role", "alert").unwrap();
            let message = document.create_element("p").unwrap();
            message.set_text_content(Some(&format!(
                "Your latest change is not saved locally. {}",
                error
            )));
            let retry = document.create_element("button").unwrap();
            retry.set_class_name("retry-save-btn");
            retry.set_text_content(Some("Retry save"));
            recovery.append_child(&message).unwrap();
            recovery.append_child(&retry).unwrap();
            page.append_child(&recovery).unwrap();
        }

        if !client_state.pending_conflicts().is_empty() {
            let recovery = document.create_element("div").unwrap();
            recovery.set_class_name("conflict-recovery");
            recovery.set_attribute("role", "status").unwrap();
            recovery.set_text_content(Some(
                "Some edits clashed with newer changes. They're kept safe on this device.",
            ));
            for conflict in client_state.pending_conflicts() {
                let detail = document.create_element("div").unwrap();
                detail.set_text_content(Some(&format!(
                    "{}: {} ({})",
                    conflict.outcome, conflict.reason, conflict.update_id
                )));
                recovery.append_child(&detail).unwrap();
            }
            page.append_child(&recovery).unwrap();
        }

        if document_available && selected_path.is_some() {
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
                    if let Some(url) = &inner.asset_preview_url {
                        if inner.asset_preview_type.starts_with("image/") {
                            let image = document.create_element("img").unwrap();
                            image.set_class_name("asset-preview-image");
                            image.set_attribute("src", url).unwrap();
                            image
                                .set_attribute(
                                    "alt",
                                    &explorer::file_name_only(entry.path.as_str()),
                                )
                                .unwrap();
                            asset_view.append_child(&image).unwrap();
                        } else if inner.asset_preview_type == "application/pdf" {
                            let pdf = document.create_element("embed").unwrap();
                            pdf.set_class_name("asset-preview-pdf");
                            pdf.set_attribute("src", url).unwrap();
                            pdf.set_attribute("type", "application/pdf").unwrap();
                            asset_view.append_child(&pdf).unwrap();
                        }
                        let download = document.create_element("a").unwrap();
                        download.set_class_name("asset-download");
                        download.set_attribute("href", url).unwrap();
                        download
                            .set_attribute(
                                "download",
                                &explorer::file_name_only(entry.path.as_str()),
                            )
                            .unwrap();
                        download.set_text_content(Some("Download asset"));
                        asset_view.append_child(&download).unwrap();
                    }
                }
                page.append_child(&asset_view).unwrap();
            } else {
                let editor_container = document.create_element("div").unwrap();
                editor_container.set_class_name(if inner.document_mode == DocumentMode::Preview {
                    "editor-container preview-reader"
                } else {
                    "editor-container"
                });

                // ── Link message (non-fatal) ────────────────────
                if let Some(ref msg) = inner.link_message {
                    let msg_div = document.create_element("div").unwrap();
                    msg_div.set_class_name("link-message");
                    let msg_span = document.create_element("span").unwrap();
                    msg_span.set_text_content(Some(msg));
                    msg_div.append_child(&msg_span).unwrap();
                    let dismiss_btn = document.create_element("button").unwrap();
                    dismiss_btn.set_class_name("link-message-dismiss");
                    dismiss_btn.set_text_content(Some("×"));
                    dismiss_btn
                        .set_attribute("aria-label", "Dismiss message")
                        .unwrap();
                    msg_div.append_child(&dismiss_btn).unwrap();
                    editor_container.append_child(&msg_div).unwrap();
                }
                if can_edit_workspace && let Some(request) = &inner.missing_note {
                    let prompt = document.create_element("div").unwrap();
                    prompt.set_class_name("link-message missing-note-confirmation");
                    prompt.set_attribute("role", "alertdialog").unwrap();
                    prompt.set_attribute("aria-modal", "true").unwrap();
                    prompt
                        .set_attribute("aria-label", "Create missing note")
                        .unwrap();
                    prompt
                        .set_text_content(Some(&format!("Note not found: {}", request.raw_target)));
                    let create = document.create_element("button").unwrap();
                    create.set_class_name("create-missing-note-btn");
                    create.set_text_content(Some("Create note"));
                    let dismiss = document.create_element("button").unwrap();
                    dismiss.set_class_name("dismiss-missing-note-btn");
                    dismiss.set_text_content(Some("Dismiss"));
                    dismiss
                        .set_attribute("data-focus-key", "dismiss-missing-note")
                        .unwrap();
                    prompt.append_child(&create).unwrap();
                    prompt.append_child(&dismiss).unwrap();
                    editor_container.append_child(&prompt).unwrap();
                }

                let document_header = document.create_element("section").unwrap();
                document_header.set_class_name(if inner.document_mode == DocumentMode::Preview {
                    "preview-document-header"
                } else {
                    "document-header-card"
                });
                if inner.document_mode == DocumentMode::Edit {
                    let title_input = document.create_element("input").unwrap();
                    let title_input: web_sys::HtmlInputElement = title_input.unchecked_into();
                    title_input.set_type("text");
                    title_input.set_class_name("document-title-input");
                    title_input.set_placeholder("Untitled note");
                    title_input
                        .set_attribute("aria-label", "Document title")
                        .unwrap();
                    title_input.set_disabled(
                        inner.local_persistence_error.is_some() || !document_ready(&inner),
                    );
                    title_input.set_value(&view_doc.title);
                    title_input
                        .set_attribute("data-canonical-value", &view_doc.title)
                        .unwrap();
                    document_header.append_child(&title_input).unwrap();
                } else {
                    let document_type = document.create_element("span").unwrap();
                    document_type.set_class_name("preview-document-type");
                    document_type.set_text_content(Some("MARKDOWN"));
                    document_header.append_child(&document_type).unwrap();
                    let title = document.create_element("h1").unwrap();
                    title.set_id("document-title");
                    title.set_class_name("document-title");
                    title.set_text_content(Some(if view_doc.title.is_empty() {
                        "Untitled note"
                    } else {
                        &view_doc.title
                    }));
                    document_header.append_child(&title).unwrap();
                }

                let metadata = document.create_element("div").unwrap();
                metadata.set_class_name("document-metadata");
                let created_at = view_doc
                    .blocks
                    .iter()
                    .map(|block| block.created_at_ms)
                    .min();
                let updated_at = view_doc
                    .blocks
                    .iter()
                    .map(|block| block.updated_at_ms)
                    .max();
                let metadata_values = vec![
                    (
                        "Created",
                        created_at
                            .map(Driver::format_timestamp)
                            .unwrap_or_else(|| "New note".to_string()),
                    ),
                    (
                        "Updated",
                        updated_at
                            .map(Driver::format_timestamp)
                            .unwrap_or_else(|| "No edits yet".to_string()),
                    ),
                ];
                for (label, value) in metadata_values {
                    let item = document.create_element("div").unwrap();
                    item.set_class_name("metadata-item");
                    let key = document.create_element("span").unwrap();
                    key.set_class_name("metadata-label");
                    key.set_text_content(Some(label));
                    let text = document.create_element("span").unwrap();
                    text.set_class_name("metadata-value");
                    text.set_text_content(Some(&value));
                    item.append_child(&key).unwrap();
                    item.append_child(&text).unwrap();
                    metadata.append_child(&item).unwrap();
                }
                let tags_item = document.create_element("div").unwrap();
                tags_item.set_class_name("metadata-item metadata-tags");
                let mut current_tags: Vec<String> = selected_path
                    .as_ref()
                    .and_then(|path| inner.knowledge.path_to_tags.get(path))
                    .map(|tags| tags.iter().cloned().collect())
                    .unwrap_or_default();
                current_tags.sort();
                if !current_tags.is_empty() {
                    for tag in current_tags {
                        let chip = document.create_element("button").unwrap();
                        chip.set_class_name("metadata-tag");
                        chip.set_attribute("data-tag", &tag).unwrap();
                        chip.set_text_content(Some(&format!("#{}", tag)));
                        tags_item.append_child(&chip).unwrap();
                    }
                    metadata.append_child(&tags_item).unwrap();
                }
                document_header.append_child(&metadata).unwrap();
                if inner.document_mode == DocumentMode::Preview {
                    let rule = document.create_element("hr").unwrap();
                    rule.set_class_name("preview-header-rule");
                    document_header.append_child(&rule).unwrap();
                }
                editor_container.append_child(&document_header).unwrap();

                let page_editable = can_edit_workspace
                    && inner.document_mode == DocumentMode::Edit
                    && document_ready(&inner);
                self.render_page_surface(
                    &document,
                    &editor_container,
                    &inner,
                    &view_doc,
                    selected_path.as_deref().unwrap_or(""),
                    page_editable,
                );
                if page_editable && !client_state.errors().is_empty() {
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

                page.append_child(&editor_container).unwrap();
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
            page.append_child(&err_state).unwrap();
        }

        self.mount_ink_blocks(
            &view_doc,
            &inner.ink_tool,
            inner.ink_brush_width,
            inner.ink_brush_color,
            inner.document_mode == DocumentMode::Preview,
            inner.device_id,
        );
        self.hydrate_markdown_assets(&document);
        if let Some((key, height)) = held_page_height {
            hold_page_height(&document, key, height);
        }
        for (selector, top, key) in scroll_positions {
            if let Ok(Some(element)) = document.query_selector(selector)
                // A different note starts at its top.
                && element.get_attribute("data-scroll-key") == key
            {
                element.set_scroll_top(top);
            }
        }
        self.sync_page_surface(
            &view_doc,
            can_edit_workspace && inner.document_mode == DocumentMode::Edit,
        );

        if let Some(key) = requested_focus.clone().or(control_focus_key) {
            let selector = format!("[data-focus-key=\"{}\"]", key.replace('"', "\\\""));
            match document.query_selector(&selector) {
                Ok(Some(element)) => {
                    if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
                        page_driver::focus_in_place(&element);
                    }
                }
                _ if requested_focus.is_some() => ui::focus_soon(&key),
                _ => {}
            }
        } else if let Some((target, start, end, direction, scroll_left, scroll_top)) = focus_info {
            if can_edit_workspace && document_available {
                if target == "title" {
                    if let Some(el) = document.query_selector(".document-title-input").unwrap() {
                        let input: web_sys::HtmlInputElement = el.unchecked_into();
                        page_driver::focus_in_place(&input);
                        let _ = input.set_selection_start(Some(start));
                        let _ = input.set_selection_end(Some(end));
                        let _ = input.set_selection_direction(Some(&direction));
                        input.set_scroll_left(scroll_left);
                        input.set_scroll_top(scroll_top);
                    }
                } else if target.starts_with("block:") {
                    let block_id = target.strip_prefix("block:").unwrap();
                    let selector = format!(".block-textarea[data-block-id=\"{}\"]", block_id);
                    if let Some(el) = document.query_selector(&selector).unwrap() {
                        let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                        page_driver::focus_in_place(&textarea);
                        let _ = textarea.set_selection_start(Some(start));
                        let _ = textarea.set_selection_end(Some(end));
                        let _ = textarea.set_selection_direction(Some(&direction));
                        textarea.set_scroll_left(scroll_left);
                        textarea.set_scroll_top(scroll_top);
                    }
                }
            }
        }
        self.focus_pending_block_caret(&document);
        RENDERING.with(|rendering| rendering.set(false));
    }

    fn format_timestamp(timestamp_ms: i64) -> String {
        let date = js_sys::Date::new(&JsValue::from_f64(timestamp_ms as f64));
        let iso = String::from(date.to_iso_string());
        iso.chars().take(16).collect::<String>().replace('T', " ")
    }
}

fn setup_event_delegation(driver: &Driver) -> Result<(), JsValue> {
    let window = web_sys::window().expect("No global window found");
    let document = window.document().expect("No global document found");
    let root = document
        .get_element_by_id("root")
        .expect("No element with id 'root' found");

    let on_beforeinput = Closure::wrap(Box::new(move |event: web_sys::InputEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        if element.class_name() != "document-title-input"
            && !element.class_list().contains("block-textarea")
        {
            return;
        }
        if element.get_attribute("data-composing").as_deref() == Some("true") {
            return;
        }
        let value = if element.class_name() == "document-title-input" {
            element
                .clone()
                .unchecked_into::<web_sys::HtmlInputElement>()
                .value()
        } else {
            element
                .clone()
                .unchecked_into::<web_sys::HtmlTextAreaElement>()
                .value()
        };
        element.set_attribute("data-beforeinput-value", &value).ok();
    }) as Box<dyn FnMut(web_sys::InputEvent)>);
    root.add_event_listener_with_callback("beforeinput", on_beforeinput.as_ref().unchecked_ref())?;
    on_beforeinput.forget();

    let d_composition_start = driver.clone();
    let on_composition_start = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        let text_target = if element.class_name() == "document-title-input" {
            Some(TextTarget::Title)
        } else if element.class_list().contains("block-textarea") {
            element
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
                .map(|block_id| {
                    if element.get_attribute("data-block-kind").as_deref() == Some("math") {
                        TextTarget::Math(block_id)
                    } else {
                        TextTarget::Markdown(block_id)
                    }
                })
        } else {
            None
        };
        if let Some(target) = text_target {
            element.set_attribute("data-composing", "true").ok();
            d_composition_start.0.borrow_mut().composing_text_target = Some(target);
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    root.add_event_listener_with_callback(
        "compositionstart",
        on_composition_start.as_ref().unchecked_ref(),
    )?;
    on_composition_start.forget();

    let d_composition_end = driver.clone();
    let on_composition_end = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        let target = d_composition_end
            .0
            .borrow_mut()
            .composing_text_target
            .take();
        let Some(target) = target else {
            return;
        };
        let before = element
            .get_attribute("data-canonical-value")
            .unwrap_or_default();
        let after = if element.class_name() == "document-title-input" {
            element
                .clone()
                .unchecked_into::<web_sys::HtmlInputElement>()
                .value()
        } else {
            element
                .clone()
                .unchecked_into::<web_sys::HtmlTextAreaElement>()
                .value()
        };
        element.remove_attribute("data-composing").ok();
        element.set_attribute("data-canonical-value", &after).ok();
        d_composition_end.dispatch_text_edit(target, &before, &after);
        d_composition_end.render();
    }) as Box<dyn FnMut(web_sys::Event)>);
    root.add_event_listener_with_callback(
        "compositionend",
        on_composition_end.as_ref().unchecked_ref(),
    )?;
    on_composition_end.forget();

    let d_input = driver.clone();
    let on_input = Closure::wrap(Box::new(move |event: web_sys::InputEvent| {
        let target = event.target();
        if let Some(target) = target {
            if let Ok(el) = target.dyn_into::<web_sys::HtmlElement>() {
                let class = el.class_name();

                if class == "new-vault-name" {
                    let input: &web_sys::HtmlInputElement = el.unchecked_ref();
                    d_input.set_new_vault_name(input.value());
                    return;
                }

                if class == "invite-email" {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    d_input.0.borrow_mut().invite_email = input.value();
                    return;
                }

                if class == "invite-role" {
                    let select: web_sys::HtmlSelectElement = el.unchecked_into();
                    d_input.0.borrow_mut().invite_role = select.value();
                    return;
                }

                if class == "invite-expiry" {
                    let select: web_sys::HtmlSelectElement = el.unchecked_into();
                    if let Ok(hours) = select.value().parse() {
                        d_input.0.borrow_mut().invite_expiry_hours = hours;
                    }
                    return;
                }

                if class == "member-role-select" {
                    let select: web_sys::HtmlSelectElement = el.unchecked_into();
                    if let Some(member_id) = select
                        .get_attribute("data-member-id")
                        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                    {
                        d_input.update_member_role(member_id, select.value());
                    }
                    return;
                }

                if class == "ink-width-input" {
                    let input: web_sys::HtmlInputElement = el.unchecked_into();
                    let Some(block_id) = input
                        .get_attribute("data-block-id")
                        .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                        .map(BlockId)
                    else {
                        return;
                    };
                    let width = input.value_as_number() as f32;
                    d_input.0.borrow_mut().ink_brush_width = width;
                    d_input.1.borrow_mut().set_brush_width(block_id, width);
                    return;
                }

                let editing_allowed = {
                    let inner = d_input.0.borrow();
                    let state = &inner.client_state;
                    let phase = state.phase();
                    can_edit_workspace(&inner)
                        && inner.storage_error.is_none()
                        && inner.local_persistence_error.is_none()
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
                    if input.get_attribute("data-composing").as_deref() == Some("true") {
                        return;
                    }
                    let before = input
                        .get_attribute("data-beforeinput-value")
                        .or_else(|| input.get_attribute("data-canonical-value"))
                        .unwrap_or_default();
                    let _ = input.remove_attribute("data-beforeinput-value");
                    input.set_attribute("data-canonical-value", &new_title).ok();
                    d_input.dispatch_text_edit(TextTarget::Title, &before, &new_title);
                } else if el.class_list().contains("block-textarea") {
                    let textarea: web_sys::HtmlTextAreaElement = el.unchecked_into();
                    let text = textarea.value();
                    if textarea.get_attribute("data-composing").as_deref() == Some("true") {
                        return;
                    }
                    let block_kind = textarea
                        .get_attribute("data-block-kind")
                        .unwrap_or_else(|| "markdown".to_string());
                    if let Some(block_id_str) = textarea.get_attribute("data-block-id") {
                        if let Ok(uuid) = uuid::Uuid::parse_str(&block_id_str) {
                            let block_id = BlockId(uuid);
                            let target = if block_kind == "math" {
                                TextTarget::Math(block_id)
                            } else {
                                TextTarget::Markdown(block_id)
                            };
                            let before = textarea
                                .get_attribute("data-beforeinput-value")
                                .or_else(|| textarea.get_attribute("data-canonical-value"))
                                .unwrap_or_default();
                            let _ = textarea.remove_attribute("data-beforeinput-value");
                            textarea.set_attribute("data-canonical-value", &text).ok();
                            d_input.dispatch_text_edit(target, &before, &text);
                        }
                    }
                }
            }
        }
    }) as Box<dyn FnMut(web_sys::InputEvent)>);

    root.add_event_listener_with_callback("input", on_input.as_ref().unchecked_ref())?;
    on_input.forget();

    let d_change = driver.clone();
    let on_change = Closure::wrap(Box::new(move |event: web_sys::Event| {
        let Some(input) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        if input.class_name() == "vault-import-input"
            && let Some(file) = input.files().and_then(|files| files.get(0))
        {
            d_change.import_vault(file);
            input.set_value("");
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    root.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())?;
    on_change.forget();

    page_driver::install_page_listeners(driver, &window, &root)?;
    page_files::install(driver, &window)?;

    let d_click = driver.clone();
    let click_document = document.clone();
    let visibility_document = document.clone();
    let on_click = Closure::wrap(Box::new(move |event: web_sys::MouseEvent| {
        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::Element>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.get_attribute("class").unwrap_or_default();
        d_click.close_export_menu(Some(&el));
        if d_click.handle_vault_click(&el) {
            return;
        }
        if el
            .closest(".sidebar, .workspace-topbar, .inspector, .home")
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        if el.closest(".live-toggle-btn").ok().flatten().is_some() {
            d_click.toggle_live_mode();
            return;
        }
        if class == "owner-setup-btn" {
            d_click.complete_owner_setup();
            return;
        }
        if class == "account-token-btn" {
            d_click.account_token_action();
            return;
        }
        if class.contains("request-password-reset-btn") {
            d_click.request_password_reset();
            return;
        }
        if class.contains("copy-recovery-key-btn") {
            d_click.copy_recovery_key();
            return;
        }
        if class.contains("recover-account-btn") {
            d_click.recover_account();
            return;
        }
        if class == "auth-login-btn" {
            d_click.auth_action("/api/auth/login");
            return;
        }
        if class.contains("auth-register-btn") {
            d_click.auth_action("/api/auth/register");
            return;
        }
        if class == "approve-key-request-btn" {
            if let Some(id) = el
                .get_attribute("data-action-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            {
                d_click.approve_key_request(id);
            }
            return;
        }
        if let Some(action) = el.get_attribute("data-security-action") {
            if let Some(id) = el
                .get_attribute("data-action-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            {
                if action == "resend-invitation" {
                    d_click.resend_invitation(id);
                } else {
                    d_click.settings_delete_action(&action, id);
                }
            }
            return;
        }
        if let Some(action) = el.get_attribute("data-membership-action") {
            let id = el
                .get_attribute("data-action-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok());
            d_click.workspace_membership_action(&action, id);
            return;
        }
        if let Some(action) = el.get_attribute("data-account-email-action") {
            d_click.request_current_account_email(action == "request-verification");
            return;
        }
        if class == "change-password-btn" {
            d_click.change_password();
            return;
        }
        if class == "export-vault-btn" {
            d_click.export_vault();
            return;
        }
        if class.contains("save-workspace-settings-btn") {
            d_click.save_workspace_settings();
            return;
        }
        if class.contains("create-workspace-btn") {
            d_click.open_new_vault();
            return;
        }
        if class.contains("delete-workspace-btn") {
            d_click.delete_current_workspace();
            return;
        }
        if class == "copy-invite-btn" {
            d_click.copy_invitation_link();
            return;
        }
        if class == "unlock-btn" {
            d_click.unlock_workspace();
            return;
        }
        if class.contains("logout-btn") {
            d_click.logout();
            return;
        }
        if class == "invite-btn" {
            d_click.create_invitation();
            return;
        }
        if class == "workspace-switch-btn" {
            if let Some(workspace_id) = el
                .get_attribute("data-workspace-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            {
                d_click.switch_workspace(workspace_id);
            }
            return;
        }
        if class == "retry-save-btn" {
            d_click.retry_local_persistence();
            return;
        }
        if class.contains("retry-sync-btn") {
            d_click.0.borrow_mut().sync_error = None;
            d_click.dispatch(ClientEvent::RetrySync);
            return;
        }

        if let Ok(Some(action_element)) = el.closest("[data-block-action]") {
            if d_click.0.borrow().local_persistence_error.is_some() {
                return;
            }
            let Some(block_id) = action_element
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
            else {
                return;
            };
            let action = action_element
                .get_attribute("data-block-action")
                .unwrap_or_default();
            let block_position = {
                let inner = d_click.0.borrow();
                let document = inner.client_state.effective_view_doc();
                document
                    .block_index(block_id)
                    .map(|index| (index, document.blocks.len()))
            };
            let Some((index, block_count)) = block_position else {
                return;
            };

            d_click.flush_ink_before_transition();
            let op = match action.as_str() {
                "delete" => Some(DocumentOp::DeleteBlock { block_id }),
                "move-up" if index > 0 => Some(DocumentOp::MoveBlock {
                    block_id,
                    new_idx: index - 1,
                }),
                "move-down" if index + 1 < block_count => Some(DocumentOp::MoveBlock {
                    block_id,
                    new_idx: index + 1,
                }),
                _ => None,
            };
            if let Some(op) = op {
                d_click.dispatch(ClientEvent::LocalIntent {
                    op,
                    update_id: UpdateId::new_v4(),
                    applied_at_ms: js_sys::Date::now() as i64,
                });
                d_click.render();
            }
            return;
        }

        if let Ok(Some(action_element)) = el.closest("[data-ink-action]") {
            let Some(block_id) = action_element
                .get_attribute("data-block-id")
                .and_then(|value| uuid::Uuid::parse_str(&value).ok())
                .map(BlockId)
            else {
                return;
            };
            let action = action_element
                .get_attribute("data-ink-action")
                .unwrap_or_default();
            if action != "expand" && d_click.0.borrow().local_persistence_error.is_some() {
                return;
            }
            match action.as_str() {
                "tool" => {
                    let tool = action_element
                        .get_attribute("data-tool")
                        .unwrap_or_else(|| "pen".to_string());
                    let (width, color) = {
                        let mut inner = d_click.0.borrow_mut();
                        inner.ink_tool = tool.clone();
                        (inner.ink_brush_width, inner.ink_brush_color)
                    };
                    {
                        let mut runtimes = d_click.1.borrow_mut();
                        runtimes.set_tool(block_id, &tool);
                        runtimes.set_brush_width(block_id, width);
                        runtimes.set_brush_color(block_id, color);
                    }
                    let active_selector =
                        format!(".ink-tool-btn.active[data-block-id=\"{}\"]", block_id);
                    if let Ok(Some(active)) = click_document.query_selector(&active_selector) {
                        active.set_class_name("ink-tool-btn");
                    }
                    action_element.set_class_name("ink-tool-btn active");
                }
                "color" => {
                    let color = action_element
                        .get_attribute("data-color")
                        .and_then(|value| {
                            let mut channels = value
                                .split(',')
                                .filter_map(|channel| channel.parse::<u8>().ok());
                            Some((channels.next()?, channels.next()?, channels.next()?))
                        });
                    if let Some(color) = color {
                        d_click.0.borrow_mut().ink_brush_color = color;
                        d_click.1.borrow_mut().set_brush_color(block_id, color);
                    }
                }
                "undo" => d_click.undo_ink_block(block_id),
                "redo" => d_click.redo_ink_block(block_id),
                "expand" => {
                    let expanded = {
                        let mut inner = d_click.0.borrow_mut();
                        if inner.expanded_ink_block == Some(block_id) {
                            inner.expanded_ink_block = None;
                            false
                        } else {
                            inner.expanded_ink_block = Some(block_id);
                            true
                        }
                    };
                    if let Ok(Some(wrapper)) =
                        action_element.closest(".block-wrapper, .preview-block")
                    {
                        if let Ok(Some(main_content)) = wrapper.closest(".main-content") {
                            if expanded {
                                let _ = main_content.class_list().add_1("ink-expanded");
                            } else {
                                let _ = main_content.class_list().remove_1("ink-expanded");
                            }
                        }
                        if expanded {
                            let _ = wrapper.class_list().add_1("expanded");
                            let _ = wrapper.set_attribute("role", "dialog");
                            let _ = wrapper.set_attribute("aria-modal", "true");
                            let _ = wrapper.set_attribute("aria-label", "Expanded Ink drawing");
                            action_element.set_text_content(Some("Collapse"));
                            let _ =
                                action_element.set_attribute("aria-label", "Collapse Ink drawing");
                            let _ = action_element.set_attribute("aria-expanded", "true");
                            if let Ok(button) =
                                action_element.clone().dyn_into::<web_sys::HtmlElement>()
                            {
                                let _ = button.focus();
                            }
                        } else {
                            let _ = wrapper.class_list().remove_1("expanded");
                            let _ = wrapper.remove_attribute("role");
                            let _ = wrapper.remove_attribute("aria-modal");
                            let _ = wrapper.remove_attribute("aria-label");
                            action_element.set_text_content(Some("Expand"));
                            let _ =
                                action_element.set_attribute("aria-label", "Expand Ink drawing");
                            let _ = action_element.set_attribute("aria-expanded", "false");
                        }
                    }
                    d_click.1.borrow_mut().resize(block_id);
                }
                _ => {}
            }
            return;
        }

        // ── Preview internal link ─────────────────────────
        let workspace_path = el.get_attribute("data-workspace-path").or_else(|| {
            el.closest("[data-workspace-path]")
                .ok()
                .flatten()
                .and_then(|p| p.get_attribute("data-workspace-path"))
        });
        if let Some(ref target_path) = workspace_path {
            event.prevent_default();
            d_click.navigate_to_link(target_path);
            return;
        }

        let missing_target = el.get_attribute("data-missing-target").or_else(|| {
            el.closest("[data-missing-target]")
                .ok()
                .flatten()
                .and_then(|p| p.get_attribute("data-missing-target"))
        });
        if let Some(target) = missing_target {
            event.prevent_default();
            let source = d_click.0.borrow().selected_path.clone().unwrap_or_default();
            d_click.request_missing_note(&target, &source);
            return;
        }

        // ── Dismiss link message ──────────────────────────
        if class == "link-message-dismiss" {
            d_click.0.borrow_mut().link_message = None;
            d_click.render();
            return;
        }

        // ── Inspector Link Click ───────────────────────────
        let inspector_link_path = el.get_attribute("data-path").or_else(|| {
            el.closest(".link-item[data-path]")
                .ok()
                .flatten()
                .and_then(|row| row.get_attribute("data-path"))
        });
        if let Some(path) = inspector_link_path {
            d_click.select_entry(&path);
            return;
        }

        if class == "create-missing-note-btn" {
            let path = d_click
                .0
                .borrow_mut()
                .missing_note
                .take()
                .map(|request| request.proposed_path);
            if let Some(path) = path {
                d_click.create_file(&path);
            }
            return;
        }
        if class == "dismiss-missing-note-btn" {
            d_click.0.borrow_mut().missing_note = None;
            d_click.render();
            return;
        }

        // ── Inspector Tag Click ────────────────────────────
        if class == "metadata-tag" {
            let tag = el.get_attribute("data-tag").or_else(|| {
                el.parent_element()
                    .and_then(|p| p.get_attribute("data-tag"))
            });
            if let Some(tag) = tag {
                ui::filter_tag(&tag);
            }
            return;
        }

        // ── Block insertion ───────────────────────────────────────
        let editor_editing_allowed = {
            let inner = d_click.0.borrow();
            let state = &inner.client_state;
            let phase = state.phase();
            can_edit_workspace(&inner)
                && inner.storage_error.is_none()
                && inner.local_persistence_error.is_none()
                && phase != SessionPhase::HydratingLocal
                && phase != SessionPhase::Closed
                && phase != SessionPhase::Error
                && inner.selected_path.is_some()
        };
        if !editor_editing_allowed {
            return;
        }
        if class == "add-block-btn"
            && el.get_attribute("data-block-type").as_deref() == Some("image")
        {
            d_click.pick_files_for_note();
            return;
        }
        if class == "add-block-btn" {
            let update_id = UpdateId::new_v4();
            let applied_at_ms = js_sys::Date::now() as i64;
            let block_id = BlockId::new_v4();
            let block_type = el
                .get_attribute("data-block-type")
                .unwrap_or_else(|| "markdown".to_string());
            let content = match block_type.as_str() {
                "math" => BlockContent::Math {
                    source: String::new(),
                },
                "ink" => inkstone_core::new_opaque_ink_content(800.0, 600.0, uuid::Uuid::new_v4())
                    .expect("Stonepen can initialize an empty Ink block"),
                "divider" => BlockContent::Divider,
                "space" => BlockContent::Space {
                    min_height: inkstone_core::DEFAULT_SPACE_HEIGHT,
                },
                _ => BlockContent::Markdown {
                    text: String::new(),
                },
            };
            if matches!(
                content,
                BlockContent::Markdown { .. } | BlockContent::Math { .. }
            ) {
                let mut inner = d_click.0.borrow_mut();
                inner.editing_block = Some(block_id);
                inner.pending_block_caret = Some((block_id, 0));
            }
            let block = inkstone_core::document::DocumentBlock {
                id: block_id,
                created_at_ms: applied_at_ms,
                updated_at_ms: applied_at_ms,
                content,
            };

            let insert_idx = d_click
                .0
                .borrow()
                .client_state
                .effective_view_doc()
                .blocks
                .len();

            d_click.dispatch(ClientEvent::LocalIntent {
                op: DocumentOp::InsertBlock {
                    idx: insert_idx,
                    block,
                },
                update_id,
                applied_at_ms,
            });
            d_click.render();
            return;
        }
    }) as Box<dyn FnMut(web_sys::MouseEvent)>);

    root.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref())?;
    on_click.forget();

    let d_pointerdown = driver.clone();
    let on_pointerdown = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        if !element.id().starts_with("ink-canvas-") {
            return;
        }
        if let Some(block_id) = element
            .get_attribute("data-block-id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        {
            d_pointerdown.mark_ink_changed(block_id);
            d_pointerdown.queue_ink_previews(Some(block_id), true);
        }
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    root.add_event_listener_with_callback("pointerdown", on_pointerdown.as_ref().unchecked_ref())?;
    on_pointerdown.forget();

    let d_pointermove = driver.clone();
    let on_pointermove = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
        let Some(block_id) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .filter(|element| element.id().starts_with("ink-canvas-"))
            .and_then(|element| element.get_attribute("data-block-id"))
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        else {
            return;
        };
        d_pointermove.queue_ink_previews(Some(block_id), false);
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    root.add_event_listener_with_callback("pointermove", on_pointermove.as_ref().unchecked_ref())?;
    on_pointermove.forget();

    let d_pointerup = driver.clone();
    let on_pointerup = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        if !element.id().starts_with("ink-canvas-") {
            return;
        }
        if let Some(block_id) = element
            .get_attribute("data-block-id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        {
            d_pointerup.queue_ink_previews(Some(block_id), true);
            d_pointerup.save_ink_block(block_id);
        }
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    root.add_event_listener_with_callback("pointerup", on_pointerup.as_ref().unchecked_ref())?;
    on_pointerup.forget();

    let d_pointercancel = driver.clone();
    let on_pointercancel = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(element) = target.dyn_into::<web_sys::Element>() else {
            return;
        };
        if !element.id().starts_with("ink-canvas-") {
            return;
        }
        if let Some(block_id) = element
            .get_attribute("data-block-id")
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        {
            d_pointercancel.queue_ink_previews(Some(block_id), true);
            d_pointercancel.save_ink_block(block_id);
        }
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    root.add_event_listener_with_callback(
        "pointercancel",
        on_pointercancel.as_ref().unchecked_ref(),
    )?;
    on_pointercancel.forget();

    let d_lost_capture = driver.clone();
    let on_lost_capture = Closure::wrap(Box::new(move |event: web_sys::PointerEvent| {
        let Some(block_id) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .filter(|element| element.id().starts_with("ink-canvas-"))
            .and_then(|element| element.get_attribute("data-block-id"))
            .and_then(|value| uuid::Uuid::parse_str(&value).ok())
            .map(BlockId)
        else {
            return;
        };
        d_lost_capture.queue_ink_previews(Some(block_id), true);
    }) as Box<dyn FnMut(web_sys::PointerEvent)>);
    root.add_event_listener_with_callback(
        "lostpointercapture",
        on_lost_capture.as_ref().unchecked_ref(),
    )?;
    on_lost_capture.forget();

    let d_visibility = driver.clone();
    let visibility_target = visibility_document.clone();
    let on_visibility = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if visibility_document.hidden() {
            d_visibility.save_all_ink_blocks();
            d_visibility.0.borrow_mut().pending_presence = Some(PresencePayload {
                version: PRESENCE_VERSION,
                sent_at_ms: js_sys::Date::now() as i64,
                state: PresenceKind::Idle,
            });
            d_visibility.send_pending_presence();
        } else {
            d_visibility.trigger_remote_discovery();
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    visibility_target.add_event_listener_with_callback(
        "visibilitychange",
        on_visibility.as_ref().unchecked_ref(),
    )?;
    on_visibility.forget();

    // Closing or reloading while an edit is still being written to this
    // device asks first.
    let d_unload = driver.clone();
    let on_before_unload = Closure::wrap(Box::new(move |event: web_sys::Event| {
        if d_unload.has_unsaved_edits() {
            event.prevent_default();
            js_sys::Reflect::set(&event, &"returnValue".into(), &"".into()).ok();
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    web_sys::window()
        .expect("No global window found")
        .add_event_listener_with_callback(
            "beforeunload",
            on_before_unload.as_ref().unchecked_ref(),
        )?;
    on_before_unload.forget();

    // Layout only depends on width. A phone keyboard changes just the height,
    // and re-rendering then would replace the focused field and close it.
    let d_resize = driver.clone();
    let rendered_width = std::cell::Cell::new(viewport_width());
    let on_resize = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        let width = viewport_width();
        if width != rendered_width.replace(width) {
            d_resize.render();
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    web_sys::window()
        .expect("No global window found")
        .add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref())?;
    on_resize.forget();

    let d_keydown = driver.clone();
    let on_keydown = Closure::wrap(Box::new(move |event: web_sys::KeyboardEvent| {
        if event.is_composing() {
            return;
        }
        if event.key() == "Tab"
            && d_keydown.0.borrow().expanded_ink_block.is_some()
            && d_keydown.0.borrow().document_mode == DocumentMode::Preview
        {
            event.prevent_default();
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                if let Ok(Some(button)) =
                    document.query_selector(".preview-ink.expanded [data-ink-action=\"expand\"]")
                {
                    if let Ok(button) = button.dyn_into::<web_sys::HtmlElement>() {
                        let _ = button.focus();
                    }
                }
            }
            return;
        }
        if event.key() == "Escape"
            && (d_keydown.close_export_menu(None)
                || d_keydown.close_new_vault()
                || d_keydown.close_vault_menu())
        {
            event.prevent_default();
            return;
        }
        if event.key() == "Escape" {
            if d_keydown.0.borrow().settings_open {
                event.prevent_default();
                settings::close();
                return;
            }
            let expanded = d_keydown.0.borrow_mut().expanded_ink_block.take();
            if let Some(block_id) = expanded {
                event.prevent_default();
                if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                    let selector = format!(".expanded[data-block-id=\"{}\"]", block_id);
                    if let Ok(Some(wrapper)) = document.query_selector(&selector) {
                        let _ = wrapper.class_list().remove_1("expanded");
                        let _ = wrapper.remove_attribute("role");
                        let _ = wrapper.remove_attribute("aria-modal");
                        let _ = wrapper.remove_attribute("aria-label");
                        if let Ok(Some(main_content)) = wrapper.closest(".main-content") {
                            let _ = main_content.class_list().remove_1("ink-expanded");
                        }
                        if let Ok(Some(button)) =
                            wrapper.query_selector("[data-ink-action=\"expand\"]")
                        {
                            button.set_text_content(Some("Expand"));
                            let _ = button.set_attribute("aria-label", "Expand Ink drawing");
                            let _ = button.set_attribute("aria-expanded", "false");
                            if let Ok(button) = button.dyn_into::<web_sys::HtmlElement>() {
                                let _ = button.focus();
                            }
                        }
                    }
                }
                d_keydown.1.borrow_mut().resize(block_id);
                return;
            }
            if d_keydown.0.borrow().missing_note.is_some() {
                event.prevent_default();
                d_keydown.0.borrow_mut().missing_note = None;
                d_keydown.render();
                return;
            }
            let focus_key = {
                let mut inner = d_keydown.0.borrow_mut();
                if inner.inspector_open {
                    inner.inspector_open = false;
                    inner.pending_focus_key = Some("inspector-toggle".to_string());
                    Some("inspector-toggle")
                } else if inner.sidebar_open {
                    inner.sidebar_open = false;
                    inner.pending_focus_key = Some("sidebar-toggle".to_string());
                    Some("sidebar-toggle")
                } else {
                    None
                }
            };
            if focus_key.is_some() {
                event.prevent_default();
                d_keydown.render();
                return;
            }
        }
        // Global Ctrl+K / Cmd+K check
        let is_k = event.key() == "k" || event.key() == "K";
        let is_ctrl_cmd = event.ctrl_key() || event.meta_key();
        if is_ctrl_cmd && is_k {
            event.prevent_default();
            ui::quick_search(true);
            return;
        }

        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::HtmlElement>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.class_name();

        if event.key() == "Enter" && class == "guest-name" {
            event.prevent_default();
            d_keydown.submit_guest_join();
            return;
        }
        if event.key() == "Enter" && class == "new-vault-name" {
            event.prevent_default();
            d_keydown.submit_new_vault();
            return;
        }

        if event.key() == "Enter"
            && (class == "owner-email"
                || class == "owner-password"
                || class == "owner-password-confirmation")
        {
            event.prevent_default();
            d_keydown.complete_owner_setup();
            return;
        }
        if event.key() == "Enter" && (class == "auth-email" || class == "auth-password") {
            event.prevent_default();
            d_keydown.auth_action("/api/auth/login");
            return;
        }
        if event.key() == "Enter" && class == "unlock-password" {
            event.prevent_default();
            d_keydown.unlock_workspace();
            return;
        }
    }) as Box<dyn FnMut(web_sys::KeyboardEvent)>);
    web_sys::window()
        .expect("No global window found")
        .document()
        .expect("No global document found")
        .add_event_listener_with_callback("keydown", on_keydown.as_ref().unchecked_ref())?;
    on_keydown.forget();

    let d_focusout = driver.clone();
    let on_focusout = Closure::wrap(Box::new(move |event: web_sys::FocusEvent| {
        if RENDERING.with(|rendering| rendering.get()) {
            return;
        }
        let target = match event.target() {
            Some(t) => t,
            None => return,
        };
        let el = match target.dyn_into::<web_sys::HtmlElement>() {
            Ok(e) => e,
            Err(_) => return,
        };
        let class = el.class_name();
        if class != "document-title-input" && !el.class_list().contains("block-textarea") {
            return;
        }
        if let Some(related) = event.related_target() {
            if let Ok(rel_el) = related.dyn_into::<web_sys::HtmlElement>() {
                let rel_class = rel_el.class_name();
                if rel_class == "document-title-input"
                    || rel_el.class_list().contains("block-textarea")
                {
                    return;
                }
            }
        }
        // Input dispatch persists canonical document changes. Rendering here would
        // replace the clicked control during its mousedown/focusout sequence.
        d_focusout.queue_presence(PresenceKind::Idle);
    }) as Box<dyn FnMut(web_sys::FocusEvent)>);

    root.add_event_listener_with_callback("focusout", on_focusout.as_ref().unchecked_ref())?;
    on_focusout.forget();

    Ok(())
}

thread_local! {
    static DRIVER: RefCell<Option<Driver>> = RefCell::new(None);
    static RESEND_SCHEDULED: Cell<bool> = const { Cell::new(false) };
}

const RATE_LIMITED: &str = "Durable update rate limit exceeded";

fn workspace_key_metadata(workspace_id: uuid::Uuid) -> String {
    format!("\0workspace_key:{workspace_id}")
}

fn sync_status_text(inner: &DriverInner, phase: SessionPhase) -> &'static str {
    if inner.sync_error.is_some() || inner.auth_error.is_some() {
        return "Sync needs attention";
    }
    match phase {
        SessionPhase::Live => "Encrypted sync is live",
        SessionPhase::CatchingUp => "Decrypting remote changes...",
        SessionPhase::Connecting => "Connecting encrypted sync...",
        SessionPhase::Offline => "Offline; changes stay local",
        SessionPhase::Error => "Sync needs attention",
        SessionPhase::HydratingLocal => "Loading local encrypted state...",
        SessionPhase::Closed => "No document connected",
    }
}

/// Label and tooltip of the Live mode toggle.
fn live_toggle_text(live_on: bool, direct: usize) -> (String, String) {
    match (live_on, direct) {
        (false, _) => (
            "Live".into(),
            "Live mode is off. Pen strokes and cursors reach others through the server.".into(),
        ),
        (true, 0) => (
            "Live".into(),
            "Live mode is on. Waiting for someone else on this note.".into(),
        ),
        (true, count) => (
            format!("Live · {count}"),
            format!(
                "Live mode: pen strokes and cursors go straight to {count} other device(s), \
                 without waiting for lost packets. Edits still sync through the server."
            ),
        ),
    }
}

fn recovery_confirmation_checked(document: &web_sys::Document) -> bool {
    document
        .query_selector(".recovery-confirmation")
        .ok()
        .flatten()
        .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
        .is_some_and(|input| input.checked())
}

fn current_user_is_owner(inner: &DriverInner) -> bool {
    let user_id = inner.auth_user.as_ref().map(|user| user.id);
    inner
        .members
        .iter()
        .any(|member| Some(member.user_id) == user_id && member.role == "owner")
}

fn can_manage_members(inner: &DriverInner) -> bool {
    let user_id = inner.auth_user.as_ref().map(|user| user.id);
    inner.members.iter().any(|member| {
        Some(member.user_id) == user_id && matches!(member.role.as_str(), "owner" | "admin")
    })
}

fn workspace_ownership_metadata(workspace_id: uuid::Uuid) -> String {
    format!("\0workspace_ownership:{workspace_id}")
}

fn local_mode_requested(search: &str) -> bool {
    cfg!(feature = "serverless-local")
        && search
            .trim_start_matches('?')
            .split('&')
            .any(|part| part == "local=1")
}

fn remote_path_envelope_metadata(workspace_id: uuid::Uuid, doc_id: DocId) -> String {
    format!("\0remote_path_envelope:{workspace_id}:{doc_id}")
}

fn parse_invite_fragment() -> Option<InviteFragment> {
    let window = web_sys::window()?;
    let invite = parse_invite_fragment_value(&window.location().hash().ok()?)?;
    clear_sensitive_fragment();
    Some(invite)
}

fn parse_invite_fragment_value(hash: &str) -> Option<InviteFragment> {
    hash.trim_start_matches('#').split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "invitation" && !value.is_empty()).then(|| InviteFragment {
            token: value.to_string(),
        })
    })
}

fn parse_account_token_fragment(hash: &str) -> Option<AccountTokenFlow> {
    hash.trim_start_matches('#').split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        if value.is_empty() {
            return None;
        }
        match name {
            "verify-email" => Some(AccountTokenFlow::VerifyEmail(value.to_string())),
            "password-reset" => Some(AccountTokenFlow::PasswordReset(value.to_string())),
            _ => None,
        }
    })
}

fn parse_account_token_fragment_from_window() -> Option<AccountTokenFlow> {
    let window = web_sys::window()?;
    let flow = parse_account_token_fragment(&window.location().hash().ok()?)?;
    clear_sensitive_fragment();
    Some(flow)
}

fn fragment_email(hash: &str) -> Option<String> {
    let value = hash.trim_start_matches('#').split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "email" && !value.is_empty()).then_some(value)
    })?;
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |at: usize| (bytes.get(at).copied()? as char).to_digit(16);
        match (bytes[index], hex(index + 1), hex(index + 2)) {
            (b'%', Some(high), Some(low)) => {
                decoded.push((high * 16 + low) as u8);
                index += 3;
            }
            (byte, ..) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

fn fragment_email_from_window() -> Option<String> {
    fragment_email(&web_sys::window()?.location().hash().ok()?)
}

async fn remembered_vault_key(workspace_db: &WorkspaceDb, user_id: uuid::Uuid) -> Option<[u8; 32]> {
    let value = workspace_db
        .get_global_metadata(REMEMBERED_VAULT_KEY)
        .await
        .ok()??;
    let (owner, key) = value.split_once(':')?;
    if owner != user_id.to_string() {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(key)
        .ok()?
        .try_into()
        .ok()
}

fn parse_owner_setup_fragment(hash: &str) -> Option<String> {
    hash.trim_start_matches('#').split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "owner-setup" && !value.is_empty()).then(|| value.to_string())
    })
}

fn parse_owner_setup_fragment_from_window() -> Option<String> {
    let window = web_sys::window()?;
    let token = parse_owner_setup_fragment(&window.location().hash().ok()?)?;
    clear_sensitive_fragment();
    Some(token)
}

fn clear_sensitive_fragment() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let location = window.location();
    let url = format!(
        "{}{}",
        location.pathname().unwrap_or_else(|_| "/".into()),
        location.search().unwrap_or_default()
    );
    if let Ok(history) = window.history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(&url));
    }
}

fn clear_invite_fragment() {
    clear_sensitive_fragment();
}

pub(crate) fn download_bytes(
    file_name: &str,
    bytes: &[u8],
    media_type: &str,
) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| "Browser window is unavailable".to_string())?;
    let document = window
        .document()
        .ok_or_else(|| "Browser document is unavailable".to_string())?;
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(media_type);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(|error| format!("Failed to create download: {error:?}"))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|error| format!("Failed to create download URL: {error:?}"))?;
    let result = (|| {
        let anchor = document
            .create_element("a")
            .map_err(|error| format!("Failed to create download link: {error:?}"))?
            .dyn_into::<web_sys::HtmlAnchorElement>()
            .map_err(|_| "Failed to create download link".to_string())?;
        anchor.set_href(&url);
        anchor.set_download(file_name);
        anchor.click();
        Ok(())
    })();
    let _ = web_sys::Url::revoke_object_url(&url);
    result
}

pub(crate) fn asset_media_type(path: &str) -> String {
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .into()
}

/// "Ctrl K" or "⌘K", whichever this platform's users expect.
fn shortcut_label(key: &str) -> String {
    let apple = web_sys::window()
        .and_then(|window| window.navigator().platform().ok())
        .is_some_and(|platform| platform.starts_with("Mac") || platform.starts_with("iP"));
    if apple {
        format!("⌘{key}")
    } else {
        format!("Ctrl {key}")
    }
}

/// Classifies a save status message for the status dot.
fn save_label(inner: &DriverInner) -> Option<String> {
    if persistence_state(&inner.persistence_status) == "error" {
        return Some(inner.persistence_status.clone());
    }
    let state = &inner.client_state;
    let saving = state.has_pending_intent()
        || state.intent_queue_len() > 0
        || inner.deferred_text_persist.is_some()
        || inner
            .text_client_state
            .as_ref()
            .is_some_and(TextClientState::has_pending_persistence);
    if saving {
        None
    } else if matches!(state.phase(), SessionPhase::Offline | SessionPhase::Error) {
        Some("Offline – saved on this device".into())
    } else {
        Some("Saved".into())
    }
}

fn persistence_state(status: &str) -> &'static str {
    let lower = status.to_lowercase();
    if lower.contains("fail") || lower.starts_with("error") {
        "error"
    } else if lower.starts_with("saved") || lower.starts_with("offline") {
        "ok"
    } else if lower.starts_with("initializing") {
        "idle"
    } else {
        "pending"
    }
}

/// Below this width the inspector overlays the page instead of docking.
fn inspector_is_drawer() -> bool {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .is_some_and(|width| width <= 1180.0)
}

fn create_blob_url(bytes: &[u8], media_type: &str) -> Result<String, String> {
    let parts = js_sys::Array::new();
    parts.push(&js_sys::Uint8Array::from(bytes));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(media_type);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)
        .map_err(|error| format!("Failed to create asset preview: {error:?}"))?;
    web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|error| format!("Failed to create asset preview URL: {error:?}"))
}

async fn cleanup_remote_import(
    workspace_id: uuid::Uuid,
    assets: &[(WorkspacePath, uuid::Uuid)],
    folders: &[(WorkspacePath, uuid::Uuid)],
) {
    for (_, id) in assets.iter().rev() {
        let _ = api::empty::<serde_json::Value>("DELETE", &format!("/api/assets/{id}"), None).await;
    }
    for (_, id) in folders.iter().rev() {
        let _ = api::empty::<serde_json::Value>(
            "DELETE",
            &format!("/api/workspaces/{workspace_id}/path-projections/{id}"),
            None,
        )
        .await;
    }
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

#[wasm_bindgen]
pub fn active_document_json_for_test() -> Option<String> {
    DRIVER.with(|driver| {
        let driver = driver.borrow();
        let driver = driver.as_ref()?;
        let doc = Driver::effective_document(&driver.0.borrow());
        let mut value = serde_json::to_value(&doc).ok()?;
        if let (Some(layer), Some(ink)) = (doc.ink.as_ref(), value.get_mut("ink")) {
            ink["stonepen_json"] = serde_json::Value::String(layer.stonepen_json());
        }
        serde_json::to_string(&value).ok()
    })
}

#[wasm_bindgen]
pub fn has_unsaved_edits_for_test() -> bool {
    DRIVER.with(|driver| {
        driver
            .borrow()
            .as_ref()
            .is_some_and(Driver::has_unsaved_edits)
    })
}

/// The open note's sync phase and errors, for tests.
#[wasm_bindgen]
pub fn sync_debug_for_test() -> String {
    DRIVER.with(|driver| {
        let driver = driver.borrow();
        let Some(driver) = driver.as_ref() else {
            return String::new();
        };
        let inner = driver.0.borrow();
        format!(
            "{:?} {:?} sync_error={:?}",
            inner.client_state.phase(),
            inner.client_state.errors(),
            inner.sync_error
        )
    })
}

/// `{"open": <direct channels>, "received": <frames taken from them>}`.
#[wasm_bindgen]
pub fn live_stats_for_test() -> String {
    DRIVER.with(|driver| {
        let driver = driver.borrow();
        let Some(driver) = driver.as_ref() else {
            return "{}".into();
        };
        let handles = driver.2.borrow();
        format!(
            r#"{{"open":{},"received":{}}}"#,
            handles.live.open_count(),
            handles.live.received()
        )
    })
}

#[wasm_bindgen]
pub fn disconnect_sync_for_test() {
    DRIVER.with(|driver| {
        if let Some(driver) = driver.borrow().as_ref() {
            if let Some(binding) = driver.2.borrow().socket.as_ref() {
                let _ = binding.socket.close();
            }
        }
    });
}

#[wasm_bindgen(start)]
pub fn start_app() -> Result<(), JsValue> {
    std::panic::set_hook(Box::new(console_error_panic_hook::hook));
    theme::apply(theme::current());
    prefs::apply_all();

    let driver = Driver::new(DocId::new_v4());
    let unauthorized_driver = driver.clone();
    api::set_unauthorized_handler(Rc::new(move || {
        unauthorized_driver.invalidate_expired_session();
    }));
    DRIVER.with(|d| *d.borrow_mut() = Some(driver.clone()));
    setup_event_delegation(&driver)?;
    driver.render();

    let d = driver.clone();
    wasm_bindgen_futures::spawn_local(async move {
        match WorkspaceDb::open().await {
            Ok(ws) => {
                d.0.borrow_mut().workspace_db = Some(ws.clone());

                let (device_id, reused_device_id) =
                    match ws.get_global_metadata(DEVICE_ID_KEY).await {
                        Ok(Some(value)) => match uuid::Uuid::parse_str(&value) {
                            Ok(device_id) => (device_id, true),
                            Err(error) => {
                                d.0.borrow_mut().storage_error =
                                    Some(format!("Stored device ID is invalid: {error}"));
                                d.render();
                                return;
                            }
                        },
                        Ok(None) => {
                            let device_id = uuid::Uuid::new_v4();
                            if let Err(error) = ws
                                .set_global_metadata(DEVICE_ID_KEY, &device_id.to_string())
                                .await
                            {
                                d.0.borrow_mut().storage_error =
                                    Some(format!("Failed to store device ID: {error:?}"));
                                d.render();
                                return;
                            }
                            (device_id, false)
                        }
                        Err(error) => {
                            d.0.borrow_mut().storage_error =
                                Some(format!("Failed to read device ID: {error:?}"));
                            d.render();
                            return;
                        }
                    };
                d.0.borrow_mut().device_id = Some(device_id);

                if let Some((token, key)) = vaults::view_fragment_from_window() {
                    d.start_public_view(token, key).await;
                    return;
                }

                let local_mode = web_sys::window()
                    .and_then(|window| window.location().search().ok())
                    .is_some_and(|query| local_mode_requested(&query));
                d.0.borrow_mut().local_mode = local_mode;

                if local_mode {
                    if let Err(error) = ws.activate_local_scope().await {
                        d.0.borrow_mut().storage_error =
                            Some(format!("Failed to activate local workspace: {error:?}"));
                        d.render();
                        return;
                    }
                    match ws.get_recent_notes().await {
                        Ok(recents) => d.0.borrow_mut().recent_notes = recents,
                        Err(error) => {
                            d.0.borrow_mut().storage_error =
                                Some(format!("Failed to load recent notes: {error:?}"));
                            d.render();
                            return;
                        }
                    }
                    d.0.borrow_mut().auth_phase = AuthPhase::Ready;
                    d.0.borrow_mut().workspace_name = "Local knowledge workspace".into();
                    d.refresh_entries();
                    return;
                }

                match api::json::<AuthUserResponse, serde_json::Value>("GET", "/api/auth/me", None)
                    .await
                {
                    Ok(user) => d.bootstrap_authenticated(user, None).await,
                    Err(error) if error.status == 401 => {
                        if reused_device_id {
                            let replacement = uuid::Uuid::new_v4();
                            if let Err(error) = ws
                                .set_global_metadata(DEVICE_ID_KEY, &replacement.to_string())
                                .await
                            {
                                d.0.borrow_mut().storage_error =
                                    Some(format!("Failed to rotate device ID: {error:?}"));
                                d.render();
                                return;
                            }
                            d.0.borrow_mut().device_id = Some(replacement);
                        }
                        d.0.borrow_mut().auth_phase = AuthPhase::SignedOut;
                        d.0.borrow_mut().auth_error = None;
                        d.render();
                    }
                    Err(error) => {
                        d.0.borrow_mut().auth_phase = AuthPhase::SignedOut;
                        d.0.borrow_mut().auth_error = Some(error.message);
                        d.render();
                    }
                }
            }
            Err(e) => {
                d.0.borrow_mut().storage_error = Some(format!("Failed to open workspace: {:?}", e));
                d.render();
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_files_without_an_extension_become_notes() {
        assert!(!note_name_has_extension("Reading list"));
        assert!(!note_name_has_extension("v1.2/Plan"));
        assert!(!note_name_has_extension(".gitignore"));
        assert!(note_name_has_extension("Plan.md"));
        assert!(note_name_has_extension("papers/scan.pdf"));
    }

    #[test]
    fn production_build_ignores_local_mode_query() {
        assert_eq!(
            local_mode_requested("?local=1"),
            cfg!(feature = "serverless-local")
        );
        assert!(!local_mode_requested("?local=0"));
        assert!(!local_mode_requested("?notlocal=1"));
    }

    #[test]
    fn minimal_diff_keeps_deterministic_prefix_and_suffix() {
        assert_eq!(
            minimal_text_edit("prefix old suffix", "prefix new suffix"),
            Some(TextEdit {
                start_utf16: 7,
                end_utf16: 10,
                replacement: "new".into(),
            })
        );
        assert_eq!(minimal_text_edit("unchanged", "unchanged"), None);
    }

    #[test]
    fn minimal_diff_reports_browser_utf16_offsets_for_emoji() {
        assert_eq!(
            minimal_text_edit("A😀B", "A🦀B"),
            Some(TextEdit {
                start_utf16: 1,
                end_utf16: 3,
                replacement: "🦀".into(),
            })
        );
    }

    #[test]
    fn ime_intermediate_values_collapse_to_one_final_transaction() {
        let baseline = "start ";
        let intermediate_values = ["start k", "start ka", "start か"];
        let committed = minimal_text_edit(baseline, intermediate_values.last().unwrap()).unwrap();

        assert_eq!(committed.start_utf16, 6);
        assert_eq!(committed.end_utf16, 6);
        assert_eq!(committed.replacement, "か");
    }

    #[test]
    fn owner_setup_fragment_requires_a_non_empty_exact_key() {
        assert_eq!(
            parse_owner_setup_fragment("#owner-setup=secret-token"),
            Some("secret-token".into())
        );
        assert_eq!(
            parse_owner_setup_fragment("#other=x&owner-setup=token&last=y"),
            Some("token".into())
        );
        assert_eq!(parse_owner_setup_fragment("#owner-setup="), None);
        assert_eq!(
            fragment_email("#owner-setup=t&email=Owner%2Bx%40example.test"),
            Some("Owner+x@example.test".into())
        );
        assert_eq!(fragment_email("#owner-setup=t"), None);
        assert_eq!(parse_owner_setup_fragment("#owner_setup=token"), None);
    }

    #[test]
    fn invitation_fragment_is_token_only() {
        let invite = parse_invite_fragment_value("#invitation=opaque-token").unwrap();
        assert_eq!(invite.token, "opaque-token");
        assert!(parse_invite_fragment_value("#invite=x&workspace=bad&key=raw").is_none());
    }

    #[test]
    fn account_token_flows_require_exact_non_empty_names() {
        assert_eq!(
            parse_account_token_fragment("#verify-email=secret"),
            Some(AccountTokenFlow::VerifyEmail("secret".into()))
        );
        assert_eq!(
            parse_account_token_fragment("#password-reset=secret"),
            Some(AccountTokenFlow::PasswordReset("secret".into()))
        );
        assert_eq!(parse_account_token_fragment("#password-reset="), None);
    }

    #[test]
    fn advancing_workspace_revision_invalidates_discovery() {
        let mut revision = 4;
        let mut discovery_in_flight = true;

        advance_workspace_revision(&mut revision, &mut discovery_in_flight);

        assert_eq!(revision, 5);
        assert!(!discovery_in_flight);
    }

    #[test]
    fn invitation_context_rejects_workspace_or_account_changes() {
        let workspace_id = uuid::Uuid::from_u128(1);
        let user_id = uuid::Uuid::from_u128(2);
        let key = [3; 32];
        assert!(invitation_context_matches(
            AuthPhase::Ready,
            Some(workspace_id),
            Some(key),
            4,
            Some(user_id),
            workspace_id,
            key,
            4,
            user_id,
        ));
        assert!(!invitation_context_matches(
            AuthPhase::SignedOut,
            Some(workspace_id),
            Some(key),
            4,
            Some(user_id),
            workspace_id,
            key,
            4,
            user_id,
        ));
        assert!(!invitation_context_matches(
            AuthPhase::Ready,
            Some(uuid::Uuid::from_u128(9)),
            Some(key),
            5,
            Some(user_id),
            workspace_id,
            key,
            4,
            user_id,
        ));
    }

    #[test]
    fn presence_rejects_self_replay_and_expired_state() {
        let own = uuid::Uuid::from_u128(1);
        let other = uuid::Uuid::from_u128(2);
        assert!(!presence_sender_allowed(own, own));
        assert!(presence_sender_allowed(other, own));
        assert!(accept_presence_counter(None, 1));
        assert!(accept_presence_counter(Some(4), 5));
        assert!(!accept_presence_counter(Some(5), 5));
        assert!(!accept_presence_counter(Some(5), 4));
        assert!(presence_is_live(1_000.0, 1_000.0 + PRESENCE_TTL_MS));
        assert!(!presence_is_live(1_000.0, 1_001.0 + PRESENCE_TTL_MS));
    }

    #[test]
    fn encrypted_ink_presence_keeps_stonepen_preview_opaque() {
        let workspace_id = uuid::Uuid::from_u128(1);
        let doc_id = uuid::Uuid::from_u128(2);
        let device_id = uuid::Uuid::from_u128(3);
        let recognizable = b"raw_pts brush stroke private-stonepen-preview".to_vec();
        let payload = PresencePayload {
            version: PRESENCE_VERSION,
            sent_at_ms: 7,
            state: PresenceKind::Ink {
                payload: InkPresencePayload {
                    version: INK_PRESENCE_VERSION,
                    block_id: uuid::Uuid::from_u128(4),
                    stream_epoch: uuid::Uuid::from_u128(5),
                    stonepen_preview: recognizable.clone(),
                },
            },
        };
        let plaintext = serde_json::to_vec(&payload).unwrap();
        let aad = crypto::presence_aad(workspace_id, doc_id, device_id, 1);
        let encrypted = crypto::encrypt(&[9; 32], &plaintext, &aad).unwrap();

        for marker in ["raw_pts", "brush", "stroke", "private-stonepen-preview"] {
            assert!(!encrypted.ciphertext_b64.contains(marker));
        }
        let decoded: PresencePayload = serde_json::from_slice(
            &crypto::decrypt(
                &[9; 32],
                &encrypted.ciphertext_b64,
                &encrypted.nonce_b64,
                &aad,
            )
            .unwrap(),
        )
        .unwrap();
        let PresenceKind::Ink { payload } = decoded.state else {
            panic!("expected opaque Ink presence");
        };
        assert_eq!(payload.stonepen_preview, recognizable);
    }

    #[test]
    fn viewer_can_read_but_cannot_edit_workspace() {
        assert_eq!(
            workspace_capabilities(false, false, Some("viewer")),
            (true, false)
        );
        assert_eq!(
            workspace_capabilities(false, false, Some("editor")),
            (true, true)
        );
        assert_eq!(workspace_capabilities(false, true, None), (true, true));
    }

    #[test]
    fn document_storage_keys_are_workspace_scoped() {
        let doc_id = DocId::new_v4();
        assert_ne!(
            document_storage_key(Some(uuid::Uuid::from_u128(1)), doc_id),
            document_storage_key(Some(uuid::Uuid::from_u128(2)), doc_id)
        );
    }

    #[test]
    fn synchronized_delete_keeps_local_state_offline_and_removes_it_after_retry() {
        assert!(!local_delete_is_safe(false, false));
        assert!(local_delete_is_safe(false, true));
        assert!(local_delete_is_safe(true, false));
    }

    #[test]
    fn interrupted_key_approval_workspace_resumes_from_local_marker() {
        let workspace = uuid::Uuid::from_u128(44);
        assert_eq!(
            parse_awaiting_workspace(Some(workspace.to_string())),
            Some(workspace)
        );
        assert_eq!(parse_awaiting_workspace(Some("corrupt".into())), None);
    }

    #[test]
    fn revoked_device_disappears_without_removing_current_device() {
        let current = uuid::Uuid::from_u128(1);
        let revoked = uuid::Uuid::from_u128(2);
        let make_device = |id, is_current| {
            serde_json::from_value::<DeviceResponse>(serde_json::json!({
                "id": id,
                "name": null,
                "created_at": "2026-01-01T00:00:00Z",
                "last_seen_at": "2026-01-01T00:00:00Z",
                "current": is_current,
                "sessions": []
            }))
            .unwrap()
        };
        let mut devices = vec![make_device(current, true), make_device(revoked, false)];
        apply_device_revocation(&mut devices, revoked);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, current);
        assert!(devices[0].current);
    }

    #[test]
    fn session_expiry_invalidates_keys_without_discarding_pending_intent() {
        let doc_id = DocId::new_v4();
        let mut client_state = ClientState::new(doc_id);
        let _ = client_state.transition(ClientEvent::OpenDocument { doc_id });
        let lifecycle = client_state.lifecycle_id();
        let _ = client_state.transition(ClientEvent::LocalHydrationLoaded {
            lifecycle,
            confirmed_doc: None,
            confirmed_seq: 0,
            pending_updates: Vec::new(),
        });
        let _ = client_state.transition(ClientEvent::LocalIntent {
            op: DocumentOp::SetTitle {
                title: "pending".into(),
            },
            update_id: UpdateId::new_v4(),
            applied_at_ms: 1,
        });
        assert!(client_state.has_pending_intent());

        let mut auth_user = Some(AuthUserResponse {
            id: uuid::Uuid::from_u128(1),
            email: "member@example.test".into(),
            device_id: uuid::Uuid::from_u128(2),
        });
        let mut auth_phase = AuthPhase::Ready;
        let mut account_vault_key = Some([1; 32]);
        let mut workspace_key = Some([2; 32]);
        clear_expired_secrets(
            &mut auth_user,
            &mut auth_phase,
            &mut account_vault_key,
            &mut workspace_key,
        );

        assert_eq!(auth_phase, AuthPhase::SignedOut);
        assert!(auth_user.is_none());
        assert!(account_vault_key.is_none());
        assert!(workspace_key.is_none());
        assert!(client_state.has_pending_intent());
    }

    #[test]
    fn ink_save_tracker_ignores_stale_completion() {
        let doc_id = DocId::new_v4();
        let block_id = BlockId::new_v4();
        let first = UpdateId::new_v4();
        let second = UpdateId::new_v4();
        let mut tracker = InkSaveTracker::default();

        assert_eq!(tracker.changed(doc_id, block_id), InkSavePhase::Changed);
        tracker.queued(doc_id, block_id, first);
        assert_eq!(
            tracker.saving(doc_id, block_id, first),
            Some(InkSavePhase::Saving)
        );
        tracker.changed(doc_id, block_id);
        tracker.queued(doc_id, block_id, second);

        assert_eq!(
            tracker.finish(doc_id, block_id, first, InkSavePhase::Saved),
            None
        );
        assert_eq!(tracker.phase(doc_id, block_id), InkSavePhase::Changed);
        assert_eq!(
            tracker.saving(doc_id, block_id, second),
            Some(InkSavePhase::Saving)
        );
        assert_eq!(
            tracker.finish(doc_id, block_id, second, InkSavePhase::Saved),
            Some(InkSavePhase::Saved)
        );
    }

    #[test]
    fn ink_save_tracker_reports_matching_failure_per_block() {
        let doc_id = DocId::new_v4();
        let first_block = BlockId::new_v4();
        let second_block = BlockId::new_v4();
        let first = UpdateId::new_v4();
        let second = UpdateId::new_v4();
        let mut tracker = InkSaveTracker::default();

        tracker.queued(doc_id, first_block, first);
        tracker.queued(doc_id, second_block, second);
        tracker.saving(doc_id, first_block, first);
        tracker.saving(doc_id, second_block, second);
        tracker.finish(doc_id, first_block, first, InkSavePhase::Failed);

        assert_eq!(tracker.phase(doc_id, first_block), InkSavePhase::Failed);
        assert_eq!(tracker.phase(doc_id, second_block), InkSavePhase::Saving);
    }
}
