const THEME_KEY: &str = "inkstone-theme";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePreference {
    System,
    Light,
    Dark,
    Paper,
    Midnight,
}

impl ThemePreference {
    pub fn parse(value: &str) -> Self {
        match value {
            "light" => Self::Light,
            "dark" => Self::Dark,
            "paper" => Self::Paper,
            "midnight" => Self::Midnight,
            _ => Self::System,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Paper => "paper",
            Self::Midnight => "midnight",
        }
    }
}

pub fn current() -> ThemePreference {
    web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(THEME_KEY).ok().flatten())
        .map(|value| ThemePreference::parse(&value))
        .unwrap_or(ThemePreference::System)
}

pub fn apply(preference: ThemePreference) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if let Some(root) = document.document_element() {
        let _ = root.set_attribute("data-theme", preference.as_str());
    }
}

pub fn persist_and_apply(preference: ThemePreference) {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.local_storage().ok().flatten())
    {
        let _ = storage.set_item(THEME_KEY, preference.as_str());
    }
    apply(preference);
    leptos::prelude::Set::set(&chosen(), preference);
}

fn chosen() -> leptos::prelude::ArcRwSignal<ThemePreference> {
    thread_local! {
        static CHOSEN: leptos::prelude::ArcRwSignal<ThemePreference> =
            leptos::prelude::ArcRwSignal::new(current());
    }
    CHOSEN.with(Clone::clone)
}

pub fn control(class: &'static str) -> impl leptos::IntoView + use<> {
    use leptos::prelude::*;
    let choices = [
        (ThemePreference::System, "System"),
        (ThemePreference::Light, "Light"),
        (ThemePreference::Dark, "Dark"),
        (ThemePreference::Paper, "Paper"),
        (ThemePreference::Midnight, "Midnight"),
    ]
    .map(|(preference, label)| {
        view! {
            <button
                class="theme-choice"
                data-theme-choice=preference.as_str()
                aria-pressed={
                    let theme = chosen();
                    move || if theme.get() == preference { "true" } else { "false" }
                }
                on:click=move |_| persist_and_apply(preference)
            >
                {label}
            </button>
        }
    });
    view! {
        <div class=class role="group" aria-label="Color theme">
            {choices.into_iter().collect_view()}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_values_are_stable_and_unknown_values_use_system() {
        assert_eq!(ThemePreference::parse("light"), ThemePreference::Light);
        assert_eq!(ThemePreference::parse("dark"), ThemePreference::Dark);
        assert_eq!(ThemePreference::parse("system"), ThemePreference::System);
        assert_eq!(ThemePreference::parse("unknown"), ThemePreference::System);
        assert_eq!(ThemePreference::Dark.as_str(), "dark");
        assert_eq!(ThemePreference::parse("paper"), ThemePreference::Paper);
        assert_eq!(ThemePreference::Midnight.as_str(), "midnight");
    }
}
