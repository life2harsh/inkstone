use crate::{
    AccountTokenFlow, AuthPhase, DriverInner, theme, ui::Ui, vaults::MAX_GUEST_NAME_CHARS,
};
use leptos::prelude::*;

#[derive(Clone, PartialEq)]
enum Screen {
    Unavailable(String),
    Guest { name: String, busy: bool },
    Token { reset: bool },
    Owner { busy: bool },
    Checking,
    SignedOut { invite: bool },
    AwaitingKey,
    Unlock { envelope: bool, recovering: bool },
}

#[derive(Clone, PartialEq)]
pub(crate) struct Auth {
    screen: Screen,
    email: Option<String>,
    recovery: Option<String>,
    error: Option<String>,
    notice: Option<String>,
}

pub(crate) fn snapshot(inner: &DriverInner) -> Option<Auth> {
    if inner.local_mode || inner.auth_phase == AuthPhase::Ready {
        return None;
    }
    let mut error = inner.auth_error.clone();
    let screen = if let Some(message) = &inner.public_view_error {
        Screen::Unavailable(message.clone())
    } else if let Some(join) = &inner.guest_join {
        error = join.error.clone();
        Screen::Guest {
            name: join.name.clone(),
            busy: join.busy,
        }
    } else if let Some(flow) = &inner.account_token_flow {
        Screen::Token {
            reset: matches!(flow, AccountTokenFlow::PasswordReset(_)),
        }
    } else if inner.owner_setup_token.is_some() {
        Screen::Owner {
            busy: inner.owner_setup_in_flight,
        }
    } else {
        match inner.auth_phase {
            AuthPhase::Checking => Screen::Checking,
            AuthPhase::SignedOut => Screen::SignedOut {
                invite: inner.pending_invite.is_some(),
            },
            AuthPhase::AwaitingKey => Screen::AwaitingKey,
            _ => Screen::Unlock {
                envelope: inner.needs_key_envelope,
                recovering: inner.pending_password.is_some(),
            },
        }
    };
    Some(Auth {
        screen,
        email: inner.link_email.clone(),
        recovery: inner.recovery_key_display.clone(),
        error,
        notice: inner.auth_notice.clone(),
    })
}

fn pick<T: PartialEq + Clone + Send + Sync + 'static>(
    ui: Ui,
    field: fn(&Auth) -> T,
) -> Memo<Option<T>> {
    Memo::new(move |_| ui.auth.with(|auth| auth.as_ref().map(field)))
}

fn input(
    class: &'static str,
    kind: &'static str,
    autocomplete: &'static str,
    label: &'static str,
) -> impl IntoView {
    view! { <input class=class type=kind autocomplete=autocomplete placeholder=label aria-label=label /> }
}

fn logout() -> impl IntoView {
    view! { <button class="logout-btn text-btn">"Sign out"</button> }
}

fn recovery_ceremony(ui: Ui) -> impl IntoView {
    let key = pick(ui, |auth| auth.recovery.clone());
    move || {
        key.get().flatten().map(|key| view! {
            <section class="recovery-ceremony">
                <p class="settings-warning">
                    "This is the only way back to your notes if you forget your password. Keep it somewhere safe."
                </p>
                <textarea class="recovery-key-output" readonly aria-label="Account recovery key" prop:value=key></textarea>
                <button class="copy-recovery-key-btn secondary-btn">"Copy recovery key"</button>
                <label>
                    <input class="recovery-confirmation" type="checkbox" />
                    " I saved and verified this recovery key"
                </label>
            </section>
        })
    }
}

fn email_value(ui: Ui) -> String {
    ui.auth
        .with_untracked(|auth| auth.as_ref().and_then(|auth| auth.email.clone()))
        .unwrap_or_default()
}

fn screen(ui: Ui, screen: Screen) -> AnyView {
    match screen {
        Screen::Unavailable(message) => {
            let home = web_sys::window()
                .and_then(|window| window.location().pathname().ok())
                .unwrap_or_else(|| "/".into());
            view! {
                <h2>"This link isn't available"</h2>
                <p class="auth-help">{message}</p>
                <a class="primary-btn link-home-btn" href=home>"Open Inkstone"</a>
            }
            .into_any()
        }
        Screen::Guest { name, .. } => {
            let busy = pick(ui, |auth| matches!(auth.screen, Screen::Guest { busy: true, .. }));
            let busy = move || busy.get().unwrap_or(false);
            view! {
                <h2>"Join as a guest"</h2>
                <p class="auth-help">
                    "You can edit this vault without an account. Pick a name so others know who wrote what."
                </p>
                <label for="guest-name">"Your name"</label>
                <input
                    id="guest-name"
                    class="guest-name"
                    type="text"
                    autocomplete="nickname"
                    maxlength=MAX_GUEST_NAME_CHARS
                    data-focus-key="guest-name"
                    prop:value=name
                />
                <div class="guest-join-actions">
                    <button type="button" class="primary-btn guest-join-btn" data-vault-action="guest-join" disabled=busy>
                        {move || if busy() { "Joining…" } else { "Join" }}
                    </button>
                    <button type="button" class="text-btn guest-view-btn" data-vault-action="guest-view" disabled=busy>
                        "Just read"
                    </button>
                </div>
                <p class="settings-note">"Your name is encrypted with the vault, so the server can't read it either."</p>
            }
            .into_any()
        }
        Screen::Token { reset } => view! {
            <h2>{if reset { "Reset your password" } else { "Verify email address" }}</h2>
            {reset.then(|| view! {
                <p class="settings-warning">
                    "Your notes are locked with your old password. After signing in with the new one, you'll need your recovery key to unlock them."
                </p>
                <input class="reset-email" type="email" autocomplete="email" placeholder="Email" aria-label="Email" prop:value=email_value(ui) />
                {input("reset-password", "password", "new-password", "New password")}
            })}
            <button class="account-token-btn">{if reset { "Reset password" } else { "Verify email" }}</button>
        }
        .into_any(),
        Screen::Owner { busy: true } => view! {
            <h2>"Create your account"</h2>
            <p class="auth-status loading-status" role="status">"Setting things up…"</p>
        }
        .into_any(),
        Screen::Owner { busy: false } => {
            let fields = [
                ("owner-email", "Email", "email", "email", ""),
                ("owner-password", "Password", "password", "new-password", "At least 12 characters."),
                ("owner-password-confirmation", "Confirm password", "password", "new-password", ""),
            ]
            .map(|(id, label, kind, autocomplete, help)| {
                let value = (id == "owner-email").then(|| email_value(ui));
                let help_id = format!("{id}-help");
                view! {
                    <div class="auth-field">
                        <label for=id>{label}</label>
                        <input
                            id=id
                            class=id
                            type=kind
                            autocomplete=autocomplete
                            aria-describedby=(!help.is_empty()).then(|| help_id.clone())
                            prop:value=value.unwrap_or_default()
                        />
                        {(!help.is_empty()).then(|| view! { <p id=help_id class="auth-help">{help}</p> })}
                    </div>
                }
            });
            view! {
                <h2>"Create your account"</h2>
                <p class="auth-subtitle">
                    "Your password also locks your notes. It never leaves this device, so the server can't read them."
                </p>
                {fields.into_iter().collect_view()}
                <button class="owner-setup-btn">"Create account"</button>
                {recovery_ceremony(ui)}
            }
            .into_any()
        }
        Screen::Checking => view! { <p class="auth-status" role="status">"Opening your notes…"</p> }.into_any(),
        Screen::SignedOut { invite } => view! {
            {invite.then(|| view! {
                <h2>"Accept workspace invitation"</h2>
                <p class="invite-notice">"Sign up with the invited email, or sign in to join."</p>
            })}
            <label for="auth-email">"Email"</label>
            <input id="auth-email" class="auth-email" type="email" autocomplete="email" />
            <label for="auth-password">"Password"</label>
            <input id="auth-password" class="auth-password" type="password" autocomplete="current-password" />
            {recovery_ceremony(ui)}
            <div class="auth-actions">
                <button class="auth-login-btn">"Sign in"</button>
                {invite.then(|| view! { <button class="auth-register-btn secondary-btn">"Register and accept"</button> })}
            </div>
            <button class="request-password-reset-btn text-btn">"Forgot your password?"</button>
        }
        .into_any(),
        Screen::AwaitingKey => view! {
            <h2>"Awaiting key approval"</h2>
            <p class="invite-notice">"Waiting for the workspace owner to let this device in. Reload once they have."</p>
            {logout()}
        }
        .into_any(),
        Screen::Unlock { envelope, recovering } => view! {
            <h2>{if envelope { "Finish securing this workspace" } else { "Unlock your notes" }}</h2>
            <p class="auth-subtitle">
                {if recovering {
                    "Your password changed since your notes were locked. Enter your recovery key to unlock them."
                } else {
                    "Enter your password to unlock your notes on this device."
                }}
            </p>
            {(!recovering).then(|| view! {
                {input("unlock-password", "password", "current-password", "Password")}
                <button class="unlock-btn">"Unlock"</button>
            })}
            {logout()}
            <section class="recovery-unlock">
                <h3>{if recovering { "Recovery key" } else { "Reset your password? Use your recovery key" }}</h3>
                {input("recovery-key-input", "password", "off", "Recovery key")}
                {(!recovering).then(|| input("recovery-password", "password", "current-password", "Your current password"))}
                <button class=if recovering { "recover-account-btn" } else { "recover-account-btn secondary-btn" }>
                    "Unlock with recovery key"
                </button>
            </section>
        }
        .into_any(),
    }
}

pub(crate) fn view(ui: Ui) -> impl IntoView {
    let shape = Memo::new(move |_| {
        ui.auth.with(|auth| {
            auth.as_ref().map(|auth| match &auth.screen {
                Screen::Guest { .. } => Screen::Guest {
                    name: String::new(),
                    busy: false,
                },
                other => other.clone(),
            })
        })
    });
    let error = pick(ui, |auth| auth.error.clone());
    let notice = pick(ui, |auth| auth.notice.clone());
    move || {
        shape.get()?;
        let current = ui
            .auth
            .with_untracked(|auth| auth.as_ref().map(|auth| auth.screen.clone()))?;
        Some(view! {
            <main class="auth-shell">
                <section class="auth-card">
                    <h1>"Inkstone"</h1>
                    <p class="auth-subtitle">"Notes, ink and ideas, on one page."</p>
                    {screen(ui, current)}
                    {move || error.get().flatten().map(|text| view! { <p class="auth-error" role="alert">{text}</p> })}
                    {move || notice.get().flatten().map(|text| view! { <p class="auth-status" role="status">{text}</p> })}
                    <p class="theme-control-label">"Appearance"</p>
                    {theme::control("theme-control auth-theme-control")}
                </section>
            </main>
        })
    }
}
