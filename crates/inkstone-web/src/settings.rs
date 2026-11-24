use crate::{
    DriverInner, can_manage_members, current_user_is_owner, prefs, theme,
    ui::{Ui, bool_str, driver, focus_soon, refresh},
    vaults::{self, VaultAccess},
};
use leptos::prelude::*;

const CATEGORIES: [&str; 7] = [
    "General",
    "Appearance",
    "Sharing",
    "Password & Recovery",
    "Sync & Storage",
    "Devices & Security",
    "Export",
];

#[derive(Clone, PartialEq)]
struct Access {
    key: &'static str,
    icon: &'static str,
    title: &'static str,
    detail: String,
    public: bool,
    url: Option<String>,
    editable: bool,
    admin: bool,
    busy: bool,
    status: Option<String>,
}

#[derive(Clone, PartialEq)]
struct Member {
    id: String,
    label: String,
    role: String,
    manage: bool,
    guest: bool,
    transfer: bool,
}

#[derive(Clone, PartialEq)]
struct Device {
    id: String,
    label: String,
    current: bool,
    sessions: Vec<(String, String, bool)>,
}

#[derive(Clone, PartialEq)]
pub(crate) struct Settings {
    category: String,
    name: String,
    description: String,
    local: bool,
    owner: bool,
    access: Option<Access>,
    members: Vec<Member>,
    can_invite: bool,
    invite_email: String,
    invite_role: String,
    invite_expiry: &'static str,
    inviting: bool,
    invite_url: Option<String>,
    copy_status: Option<String>,
    requests: Vec<(String, String)>,
    invitations: Vec<(String, String, bool)>,
    error: Option<String>,
    notice: Option<String>,
    status: String,
    cache: String,
    devices: Vec<Device>,
}

fn stamp(at: &chrono::DateTime<chrono::Utc>) -> String {
    at.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn access_of(inner: &DriverInner) -> Option<Access> {
    let workspace = inner
        .workspaces
        .iter()
        .find(|workspace| Some(workspace.id) == inner.workspace_id)?;
    let access = vaults::access_of(workspace);
    let people = workspace.member_count;
    let detail = match access {
        VaultAccess::Private => {
            "Only you can open this vault. Invite people or turn on a public link to share it."
                .to_string()
        }
        VaultAccess::Shared => format!("{people} people can open this vault."),
        VaultAccess::Public => {
            let mut detail = if workspace.public_link_editable {
                "Anyone with the link can edit this vault as a guest."
            } else {
                "Anyone with the link can read this vault."
            }
            .to_string();
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
    Some(Access {
        key: access.key(),
        icon: access.icon(),
        title: access.title(),
        detail,
        public: workspace.public_link,
        url: inner
            .workspace_public_link
            .as_ref()
            .zip(inner.workspace_key)
            .map(|(token, key)| vaults::link_url(token, &key)),
        editable: workspace.public_link_editable,
        admin: can_manage_members(inner),
        busy: inner.public_link_busy,
        status: inner.public_link_status.clone(),
    })
}

pub(crate) fn snapshot(inner: &DriverInner) -> Settings {
    let manage = can_manage_members(inner);
    let owner = current_user_is_owner(inner);
    let me = inner.auth_user.as_ref().map(|user| user.id);
    let bytes: u64 = inner.entries.iter().map(|entry| entry.size_bytes).sum();
    let size = if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / f64::from(1 << 20))
    } else {
        format!("{} KB", (bytes as f64 / 1024.0).ceil())
    };
    Settings {
        category: inner.settings_category.clone(),
        name: inner.workspace_name.clone(),
        description: inner.workspace_description.clone(),
        local: inner.local_mode,
        owner,
        access: access_of(inner),
        members: inner
            .members
            .iter()
            .map(|member| {
                let guest = member.guest_name.is_some();
                let label = match (inner.workspace_id, inner.workspace_key) {
                    (Some(id), Some(key)) if guest => vaults::guest_name(member, id, &key)
                        .map(|name| format!("{name} (guest)"))
                        .unwrap_or_else(|| "Guest".into()),
                    _ if guest => "Guest".into(),
                    _ => member.email.clone(),
                };
                let manage = manage && me != Some(member.user_id);
                Member {
                    id: member.user_id.to_string(),
                    label,
                    role: member.role.clone(),
                    manage,
                    guest,
                    transfer: manage && owner && !guest,
                }
            })
            .collect(),
        can_invite: me.is_some_and(|id| {
            inner.members.iter().any(|member| {
                member.user_id == id && matches!(member.role.as_str(), "owner" | "admin")
            })
        }),
        invite_email: inner.invite_email.clone(),
        invite_role: inner.invite_role.clone(),
        invite_expiry: match inner.invite_expiry_hours {
            24 => "24",
            720 => "720",
            _ => "168",
        },
        inviting: inner.invitation_in_flight,
        invite_url: inner.invite_url.clone(),
        copy_status: inner.invite_copy_status.clone(),
        requests: inner
            .key_requests
            .iter()
            .map(|request| {
                (
                    request.id.to_string(),
                    format!(
                        "{} · {} · Awaiting key approval",
                        request.email, request.role
                    ),
                )
            })
            .collect(),
        invitations: inner
            .outgoing_invitations
            .iter()
            .map(|invitation| {
                (
                    invitation.id.to_string(),
                    format!(
                        "{} · {} · {} · expires {}",
                        invitation.email,
                        invitation.role,
                        invitation.state.replace('_', " "),
                        stamp(&invitation.expires_at)
                    ),
                    invitation.state == "pending",
                )
            })
            .collect(),
        error: inner.auth_error.clone(),
        notice: inner.auth_notice.clone(),
        status: format!("Local status: {}", inner.persistence_status),
        cache: format!(
            "About {size} on this device, {} items.",
            inner.entries.len()
        ),
        devices: inner
            .devices
            .iter()
            .map(|device| Device {
                id: device.id.to_string(),
                label: format!(
                    "{}{} · last seen {}",
                    device.name.as_deref().unwrap_or("Browser device"),
                    if device.current { " (current)" } else { "" },
                    stamp(&device.last_seen_at)
                ),
                current: device.current,
                sessions: device
                    .sessions
                    .iter()
                    .map(|session| {
                        (
                            session.id.to_string(),
                            format!(
                                "Session expires {}{}",
                                stamp(&session.expires_at),
                                if session.current { " · current" } else { "" }
                            ),
                            session.current,
                        )
                    })
                    .collect(),
            })
            .collect(),
    }
}

pub(crate) fn close() {
    driver().0.borrow_mut().settings_open = false;
    refresh();
    focus_soon("settings-open");
}

fn pick(category: &'static str) {
    let driver = driver();
    driver.0.borrow_mut().settings_category = category.into();
    refresh();
    focus_soon("settings-content");
    driver.refresh_settings_data();
}

pub(crate) fn view(ui: Ui) -> impl IntoView {
    let category = Memo::new(move |_| ui.settings.with(|s| s.as_ref().map(|s| s.category.clone())));
    let title = pick_field(ui, |s| s.name.clone());
    move || {
        let category = category.get()?;
        let name = ui
            .settings
            .with_untracked(|s| s.as_ref().map(|s| s.name.clone()));
        let tabs = CATEGORIES.map(|tab| {
            let active = tab == category;
            view! {
                <button
                    class=if active { "settings-category active" } else { "settings-category" }
                    data-settings-category=tab
                    aria-current=if active { "page" } else { "false" }
                    on:click=move |_| pick(tab)
                >
                    {tab}
                </button>
            }
        });
        Some(view! {
            <div
                class="settings-backdrop"
                on:click=|event| {
                    if event.target() == event.current_target() {
                        close();
                    }
                }
            >
                <section class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
                    <header class="settings-header">
                        <div>
                            <h2 id="settings-title">"Vault Settings"</h2>
                            <p>{move || title.get()}</p>
                        </div>
                        <button class="settings-close-btn" aria-label="Close vault settings" on:click=|_| close()>
                            "Close"
                        </button>
                    </header>
                    <div class="settings-layout">
                        <nav class="settings-categories" aria-label="Settings categories">
                            {tabs.into_iter().collect_view()}
                        </nav>
                        <div class="settings-content" tabindex="-1" data-focus-key="settings-content">
                            <h3>{category.clone()}</h3>
                            {section(ui, &category, name.unwrap_or_default())}
                        </div>
                    </div>
                </section>
            </div>
        })
    }
}

fn pick_field<T: PartialEq + Clone + Send + Sync + 'static>(
    ui: Ui,
    field: fn(&Settings) -> T,
) -> Memo<Option<T>> {
    Memo::new(move |_| ui.settings.with(|s| s.as_ref().map(field)))
}

fn description(text: &'static str) -> impl IntoView {
    view! { <p class="settings-description">{text}</p> }
}

fn messages(ui: Ui, role: &'static str) -> impl IntoView {
    let error = pick_field(ui, |s| s.error.clone());
    let notice = pick_field(ui, |s| s.notice.clone());
    move || {
        let error = error
            .get()
            .flatten()
            .map(|text| view! { <p class="settings-error" role=role>{text}</p> });
        let notice = (role == "status")
            .then(|| notice.get().flatten())
            .flatten()
            .map(|text| view! { <p class="settings-note" role="status">{text}</p> });
        (error, notice)
    }
}

fn row_action(action: &'static str, id: String, label: &'static str) -> impl IntoView {
    view! {
        <button class="settings-row-action" data-security-action=action data-action-id=id>
            {label}
        </button>
    }
}

fn section(ui: Ui, category: &str, name: String) -> AnyView {
    let flags = pick_field(ui, |s| (s.local, s.owner, s.can_invite));
    let local = move || flags.get().is_some_and(|(local, ..)| local);
    match category {
        "General" => {
            let text = ui.settings.with_untracked(|s| s.as_ref().map(|s| s.description.clone()));
            view! {
                {description("Name and describe this vault.")}
                <label for="workspace-name-setting">"Vault name"</label>
                <input id="workspace-name-setting" class="workspace-name-setting" prop:value=name />
                <label for="workspace-description-setting">"Description"</label>
                <textarea
                    id="workspace-description-setting"
                    class="workspace-description-setting"
                    prop:value=text.unwrap_or_default()
                ></textarea>
                <button class="save-workspace-settings-btn settings-row-action">"Save vault details"</button>
                {move || flags.get().filter(|(local, ..)| !local).map(|(_, owner, _)| view! {
                    <button class="create-workspace-btn settings-row-action">"New vault…"</button>
                    {owner.then(|| view! {
                        <button class="delete-workspace-btn settings-row-action danger-action">
                            "Delete this vault"
                        </button>
                    })}
                })}
            }
            .into_any()
        }
        "Appearance" => view! {
            {description("Make Inkstone look the way you like.")}
            <div class="prefs-row">
                <span class="prefs-label">"Theme"</span>
                {theme::control("theme-control settings-theme-control")}
            </div>
            {prefs::controls(|| driver().layout_page())}
            <p class="settings-note">"In dark themes, black ink shows as white so it stays readable."</p>
        }
        .into_any(),
        "Sharing" => sharing(ui).into_any(),
        "Password & Recovery" => {
            let fields = [
                ("current-password", "Current password", "current-password"),
                ("new-password", "New password", "new-password"),
                ("new-password-confirm", "Confirm new password", "new-password"),
            ]
            .map(|(class, placeholder, autocomplete)| {
                view! {
                    <input
                        class=class
                        type="password"
                        placeholder=placeholder
                        aria-label=placeholder
                        autocomplete=autocomplete
                    />
                }
            });
            view! {
                {description("Your password signs you in and locks your notes. The server never sees it.")}
                <div class="settings-warning">
                    "If you forget your password and lose your recovery key, your notes can't be recovered."
                </div>
                {fields.into_iter().collect_view()}
                <button class="change-password-btn">"Change password"</button>
                {messages(ui, "status")}
            }
            .into_any()
        }
        "Sync & Storage" => {
            let status = pick_field(ui, |s| (s.status.clone(), s.cache.clone()));
            view! {
                <p class="settings-description">
                    {move || if local() {
                        "Notes are stored on this device only."
                    } else {
                        "Changes are saved locally first. Encrypted document updates sync when connected; an error here means remote state may lag behind local state."
                    }}
                </p>
                <div class="settings-status-card">{move || status.get().map(|(status, _)| status)}</div>
                <p class="settings-note">{move || status.get().map(|(_, cache)| cache)}</p>
                {move || (!local()).then(|| view! {
                    <button class="retry-sync-btn settings-row-action">"Retry encrypted sync"</button>
                })}
            }
            .into_any()
        }
        "Devices & Security" => {
            let devices = pick_field(ui, |s| s.devices.clone());
            view! {
                {description("Devices signed in to your account.")}
                {move || devices.get().unwrap_or_default().into_iter().map(|device| view! {
                    <div class="settings-member-row">
                        <span>{device.label}</span>
                        {(!device.current).then(|| row_action("revoke-device", device.id.clone(), "Revoke device"))}
                    </div>
                    {device.sessions.into_iter().map(|(id, label, current)| view! {
                        <div class="settings-session-row">
                            {label}
                            {(!current).then(|| row_action("revoke-session", id, "Revoke session"))}
                        </div>
                    }).collect_view()}
                }).collect_view()}
                <button class="settings-row-action" data-membership-action="revoke-others">
                    "Revoke all other sessions"
                </button>
                <p class="settings-note">"Forgot your password? Sign out to get a reset link."</p>
                <button class="settings-row-action" data-account-email-action="request-verification">
                    "Send email verification link"
                </button>
                <button class="settings-row-action" data-account-email-action="request-account-reset">
                    "Email me a reset link"
                </button>
            }
            .into_any()
        }
        "Export" => view! {
            {description("Download everything as a ZIP of Markdown files and attachments.")}
            <button class="export-vault-btn settings-row-action">"Download complete vault"</button>
            <label class="settings-row-action vault-import-label">
                "Import versioned vault archive"
                <input class="vault-import-input" type="file" accept=".zip,application/zip" />
            </label>
        }
        .into_any(),
        _ => view! {
            {description("Settings for this vault.")}
            <dl class="settings-details">
                <dt>"Vault name"</dt>
                <dd>{name}</dd>
                <dt>"Mode"</dt>
                <dd>{move || if local() { "Local only" } else { "Encrypted sync" }}</dd>
                <dt>"Members"</dt>
                <dd>{move || if local() { "Not applicable" } else { "Managed in Sharing" }}</dd>
            </dl>
        }
        .into_any(),
    }
}

fn link_button(
    class: &'static str,
    action: &'static str,
    icon: Option<&'static str>,
    label: &'static str,
    disabled: bool,
) -> impl IntoView {
    view! {
        <button type="button" class=class data-vault-action=action disabled=disabled>
            {icon.map(|icon| crate::icons::svg(icon, "icon"))}
            <span>{label}</span>
        </button>
    }
}

fn access_view(access: Access) -> impl IntoView {
    let choices = access.public.then(|| {
        let buttons = [(false, "eye", "Read"), (true, "pencil", "Edit")].map(|(editable, icon, label)| {
            let selected = access.editable == editable;
            view! {
                <button
                    type="button"
                    class=if selected { "link-access-choice selected" } else { "link-access-choice" }
                    data-vault-action="link-access"
                    role="radio"
                    aria-checked=bool_str(selected)
                    data-editable=bool_str(editable)
                    disabled=!access.admin || access.busy
                >
                    {crate::icons::svg(icon, "icon")}
                    <span>{label}</span>
                </button>
            }
        });
        view! {
            <div class="link-access" role="radiogroup" aria-label="Anyone with the link can">
                <span class="link-access-label">"Anyone with the link can"</span>
                {buttons.into_iter().collect_view()}
            </div>
        }
    });
    let actions = if access.admin {
        let buttons = if access.public {
            view! {
                {link_button("settings-row-action", "link-enable", None, "Reset link", access.busy)}
                {link_button("settings-row-action danger-action", "link-disable", None, "Turn off link", access.busy)}
            }
            .into_any()
        } else {
            link_button(
                "settings-row-action",
                "link-enable",
                Some("globe"),
                "Create public link",
                access.busy,
            )
            .into_any()
        };
        view! { <div class="public-link-actions">{buttons}</div> }.into_any()
    } else {
        view! { <p class="settings-note">"Only owners and admins can change the public link."</p> }
            .into_any()
    };
    view! {
        <div class="sharing-access" data-access=access.key>
            {crate::icons::svg(access.icon, "icon")}
            <div class="sharing-access-text">
                <strong>{access.title}</strong>
                <span>{access.detail}</span>
            </div>
        </div>
        <h4>"Public link"</h4>
        <p class="settings-description">
            "Anyone with the link can open every note and file in this vault, without an account. Let them edit and they join as a guest under a name they pick. The key is part of the link, so the server still can't read anything."
        </p>
        <div class="public-link-row">
            {access.public.then(|| match access.url {
                Some(url) => view! {
                    <input class="public-link-url" readonly aria-label="Public link" prop:value=url />
                    {link_button("settings-row-action", "link-copy", Some("copy"), "Copy link", false)}
                }
                .into_any(),
                None => view! {
                    <p class="settings-note">
                        "The link is on, but it was made somewhere this device can't read. Reset it to get a new one."
                    </p>
                }
                .into_any(),
            })}
        </div>
        {choices}
        {actions}
        {access.status.map(|status| view! { <p class="copy-status" role="status">{status}</p> })}
        <h4>"People with access"</h4>
    }
}

fn sharing(ui: Ui) -> impl IntoView {
    let access = pick_field(ui, |s| s.access.clone());
    let members = pick_field(ui, |s| s.members.clone());
    let can_invite = pick_field(ui, |s| s.can_invite);
    let inviting = pick_field(ui, |s| s.inviting);
    let link = pick_field(ui, |s| (s.invite_url.clone(), s.copy_status.clone()));
    let pending = pick_field(ui, |s| (s.requests.clone(), s.invitations.clone()));
    let member_rows = move || {
        members.get().unwrap_or_default().into_iter().map(|member| {
            let roles: &[&str] = if member.guest { &["viewer", "editor"] } else { &["viewer", "editor", "admin", "owner"] };
            let controls = member.manage.then(|| {
                let options = roles.iter().map(|role| view! {
                    <option value=*role selected=*role == member.role>{*role}</option>
                }).collect_view();
                view! {
                    <select class="member-role-select" data-member-id=member.id.clone()>{options}</select>
                    {row_action("remove-member", member.id.clone(), "Remove")}
                    {member.transfer.then(|| view! {
                        <button class="settings-row-action" data-membership-action="transfer" data-action-id=member.id.clone()>
                            "Transfer ownership"
                        </button>
                    })}
                }
            });
            view! {
                <div class="settings-member-row">
                    <span>{member.label}</span>
                    <span class="member-role">{member.role.clone()}</span>
                    {controls}
                </div>
            }
        }).collect_view()
    };
    let invite_form = move || {
        can_invite.get().unwrap_or(false).then(|| {
            let (email, role, expiry) = ui.settings.with_untracked(|s| {
                s.as_ref().map(|s| (s.invite_email.clone(), s.invite_role.clone(), s.invite_expiry)).unwrap_or_default()
            });
            let select = |id: &'static str, label: &'static str, values: [(&'static str, &'static str); 3], current: String| {
                let options = values.map(|(value, text)| view! {
                    <option value=value selected=value == current>{text}</option>
                });
                view! {
                    <div>
                        <label for=id>{label}</label>
                        <select id=id class=id>{options.into_iter().collect_view()}</select>
                    </div>
                }
            };
            view! {
                <div class="invite-form">
                    <label for="settings-invite-email">"Invite email"</label>
                    <input id="settings-invite-email" class="invite-email" type="email" autocomplete="email" prop:value=email />
                    <div class="invite-options">
                        {select("invite-role", "Role", [("viewer", "Viewer"), ("editor", "Editor"), ("admin", "Admin")], role)}
                        {select("invite-expiry", "Expires", [("24", "24 hours"), ("168", "7 days"), ("720", "30 days")], expiry.to_string())}
                    </div>
                    <button class="invite-btn" disabled=move || inviting.get().unwrap_or(false)>
                        {move || if inviting.get().unwrap_or(false) { "Creating invitation..." } else { "Create invitation link" }}
                    </button>
                </div>
            }
        })
    };
    view! {
        {move || access.get().flatten().map(access_view)}
        <div class="settings-member-list">{member_rows}</div>
        {invite_form}
        {move || link.get().map(|(url, status)| view! {
            {url.map(|url| view! {
                <textarea class="invite-url" readonly aria-label="Invitation share URL" prop:value=url></textarea>
                <button class="copy-invite-btn">"Copy invitation link"</button>
            })}
            {status.map(|status| view! { <p class="copy-status" role="status">{status}</p> })}
        })}
        {move || can_invite.get().unwrap_or(false).then(|| {
            let (requests, invitations) = pending.get().unwrap_or_default();
            view! {
                <h4>"Key approval requests"</h4>
                {requests.into_iter().map(|(id, text)| view! {
                    <div class="settings-member-row">
                        <span>{text}</span>
                        <button class="approve-key-request-btn" data-action-id=id>"Encrypt key and approve"</button>
                    </div>
                }).collect_view()}
                <h4>"Outgoing invitations"</h4>
                {invitations.into_iter().map(|(id, text, pending)| view! {
                    <div class="settings-member-row">
                        <span>{text}</span>
                        {pending.then(|| view! {
                            {row_action("resend-invitation", id.clone(), "Resend")}
                            {row_action("revoke-invitation", id, "Revoke")}
                        })}
                    </div>
                }).collect_view()}
            }
        })}
        <button class="settings-row-action danger-action" data-membership-action="leave">"Leave workspace"</button>
        {messages(ui, "alert")}
    }
}
