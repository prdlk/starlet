//! Settings: appearance and the BYOK provider.
//!
//! Provider keys go to the OS keychain, never to SQLite. Everything else is a
//! `ui.*` row in the key/value table. The dialog writes on change rather than
//! behind a Save button: each field is an independent setting that takes effect
//! immediately, which is what `Switch`-style settings should do.

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    input::{Input, InputEvent, InputState},
    select::{SearchableVec, Select, SelectEvent, SelectState},
    v_flex,
};

use crate::services::Backend;
use crate::theme::Appearance;

/// `ui.*` namespace key for the chosen appearance.
pub const KEY_APPEARANCE: &str = "ui.appearance";

/// Apply and persist an appearance choice.
pub fn set_appearance(appearance: Appearance, window: &mut Window, cx: &mut App) {
    crate::theme::apply(appearance, Some(window), cx);
    let value = match appearance {
        Appearance::Dark => "dark",
        Appearance::Light => "light",
        Appearance::System => "system",
    };
    let store = Backend::global(cx).store();
    Backend::global(cx).spawn(async move {
        let _ = store.set_state(KEY_APPEARANCE, value).await;
    });
}

/// Read the persisted appearance. Dark is the product default, so an unset or
/// unrecognised value means dark rather than "follow the system".
pub fn parse_appearance(value: Option<&str>) -> Appearance {
    match value {
        Some("light") => Appearance::Light,
        Some("system") => Appearance::System,
        _ => Appearance::Dark,
    }
}

fn appearance_from_title(title: &str) -> Appearance {
    Appearance::ALL
        .into_iter()
        .find(|a| a.label() == title)
        .unwrap_or(Appearance::Dark)
}

pub struct SettingsDialog {
    appearance: Entity<SelectState<SearchableVec<SharedString>>>,
    provider: Entity<SelectState<SearchableVec<SharedString>>>,
    model: Entity<InputState>,
    api_key: Entity<InputState>,
    endpoint: Entity<InputState>,
    /// The provider whose key the field currently holds, so switching provider
    /// swaps the key rather than writing one provider's key under another's
    /// keychain account.
    current_provider: String,
    _subscriptions: Vec<Subscription>,
}

impl SettingsDialog {
    pub fn open(window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| SettingsDialog::new(window, cx));
        let content = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("Settings").w(px(480.)).child(content.clone())
        });
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearances: Vec<SharedString> = Appearance::ALL
            .iter()
            .map(|a| SharedString::from(a.label()))
            .collect();
        let providers: Vec<SharedString> = starlet_ai::PROVIDER_IDS
            .iter()
            .map(|id| SharedString::from(*id))
            .collect();

        let appearance = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(appearances),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let provider = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(providers),
                Some(IndexPath::default()),
                window,
                cx,
            )
        });
        let model = cx.new(|cx| InputState::new(window, cx).placeholder("gpt-4o-mini"));
        let api_key = cx.new(|cx| InputState::new(window, cx).placeholder("sk-…").masked(true));
        let endpoint =
            cx.new(|cx| InputState::new(window, cx).placeholder("http://localhost:11434"));

        let subscriptions = vec![
            cx.subscribe_in(&appearance, window, Self::on_appearance_changed),
            cx.subscribe_in(&provider, window, Self::on_provider_changed),
            cx.subscribe_in(&model, window, Self::on_model_changed),
            cx.subscribe_in(&api_key, window, Self::on_api_key_changed),
            cx.subscribe_in(&endpoint, window, Self::on_endpoint_changed),
        ];

        let mut this = Self {
            appearance,
            provider,
            model,
            api_key,
            endpoint,
            current_provider: starlet_ai::openai::ID.to_string(),
            _subscriptions: subscriptions,
        };
        this.load(window, cx);
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = Backend::global(cx).store();
        let rx = Backend::global(cx).spawn(async move {
            (
                store.get_state(KEY_APPEARANCE).await.ok().flatten(),
                store
                    .get_state(starlet_store::KEY_AI_PROVIDER)
                    .await
                    .ok()
                    .flatten(),
                store
                    .get_state(starlet_store::KEY_AI_MODEL)
                    .await
                    .ok()
                    .flatten(),
                store
                    .get_state(starlet_store::KEY_AI_ENDPOINT)
                    .await
                    .ok()
                    .flatten(),
            )
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok((appearance, provider, model, endpoint)) = rx.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let appearance = parse_appearance(appearance.as_deref());
                this.appearance.update(cx, |state, cx| {
                    state.set_selected_value(&SharedString::from(appearance.label()), window, cx);
                });

                let provider = provider.unwrap_or_else(|| starlet_ai::openai::ID.to_string());
                this.current_provider = provider.clone();
                this.provider.update(cx, |state, cx| {
                    state.set_selected_value(&SharedString::from(provider.clone()), window, cx);
                });
                if let Some(model) = model {
                    this.model
                        .update(cx, |state, cx| state.set_value(model, window, cx));
                }
                if let Some(endpoint) = endpoint {
                    this.endpoint
                        .update(cx, |state, cx| state.set_value(endpoint, window, cx));
                }
                this.load_key_for_current_provider(window, cx);
            });
        })
        .detach();
    }

    fn load_key_for_current_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let provider = self.current_provider.clone();
        let rx = Backend::global(cx)
            .spawn_blocking(move || starlet_sync::ProviderKeyStore::load(&provider));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(key) = rx.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                this.api_key.update(cx, |state, cx| {
                    state.set_value(key.unwrap_or_default(), window, cx)
                });
            });
        })
        .detach();
    }

    fn on_appearance_changed(
        &mut self,
        _: &Entity<SelectState<SearchableVec<SharedString>>>,
        event: &SelectEvent<SearchableVec<SharedString>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        set_appearance(appearance_from_title(value), window, cx);
    }

    fn on_provider_changed(
        &mut self,
        _: &Entity<SelectState<SearchableVec<SharedString>>>,
        event: &SelectEvent<SearchableVec<SharedString>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let SelectEvent::Confirm(Some(value)) = event else {
            return;
        };
        self.current_provider = value.to_string();
        let provider = self.current_provider.clone();
        let store = Backend::global(cx).store();
        Backend::global(cx).spawn(async move {
            let _ = store
                .set_state(starlet_store::KEY_AI_PROVIDER, &provider)
                .await;
        });
        self.load_key_for_current_provider(window, cx);
        cx.notify();
    }

    fn on_model_changed(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let model = self.model.read(cx).value().to_string();
        let store = Backend::global(cx).store();
        Backend::global(cx).spawn(async move {
            let _ = store.set_state(starlet_store::KEY_AI_MODEL, &model).await;
        });
    }

    fn on_endpoint_changed(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let endpoint = self.endpoint.read(cx).value().to_string();
        let store = Backend::global(cx).store();
        Backend::global(cx).spawn(async move {
            let _ = store
                .set_state(starlet_store::KEY_AI_ENDPOINT, &endpoint)
                .await;
        });
    }

    /// Blur, not every keystroke: writing a partially typed key to the keychain
    /// would leave a broken credential behind if the user walked away.
    fn on_api_key_changed(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Blur) {
            return;
        }
        let provider = self.current_provider.clone();
        let key = self.api_key.read(cx).unmask_value().to_string();
        Backend::global(cx).spawn_blocking(move || {
            let result = if key.trim().is_empty() {
                starlet_sync::ProviderKeyStore::clear(&provider)
            } else {
                starlet_sync::ProviderKeyStore::save(&provider, key.trim())
            };
            if let Err(err) = result {
                tracing::warn!("could not store the provider key: {err}");
            }
        });
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let local_provider = self.current_provider == starlet_ai::ollama::ID;

        v_flex()
            .gap_5()
            .min_w(px(400.))
            .child(field(
                "Appearance",
                None,
                Select::new(&self.appearance).small().into_any_element(),
                cx,
            ))
            .child(
                v_flex()
                    .gap_3()
                    .child(section_title("AI analysis", cx))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                "Starlet never ships a key. Tagging and grouping run against \
                                 the provider you configure here.",
                            ),
                    )
                    .child(field(
                        "Provider",
                        None,
                        Select::new(&self.provider).small().into_any_element(),
                        cx,
                    ))
                    .child(field(
                        "Model",
                        None,
                        Input::new(&self.model).small().into_any_element(),
                        cx,
                    ))
                    .when(local_provider, |this| {
                        this.child(field(
                            "Endpoint",
                            Some("Ollama runs locally, so no key is needed."),
                            Input::new(&self.endpoint).small().into_any_element(),
                            cx,
                        ))
                    })
                    .when(!local_provider, |this| {
                        this.child(field(
                            "API key",
                            Some("Stored in your OS keychain."),
                            Input::new(&self.api_key).small().into_any_element(),
                            cx,
                        ))
                    }),
            )
    }
}

fn section_title(title: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().foreground)
        .child(title)
}

fn field(
    label: &'static str,
    help: Option<&'static str>,
    control: gpui::AnyElement,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .gap_1p5()
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(control)
        .when_some(help, |this, help| {
            this.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(help),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_defaults_to_dark() {
        assert_eq!(parse_appearance(None), Appearance::Dark);
        assert_eq!(parse_appearance(Some("nonsense")), Appearance::Dark);
        assert_eq!(parse_appearance(Some("light")), Appearance::Light);
        assert_eq!(parse_appearance(Some("system")), Appearance::System);
    }

    #[test]
    fn appearance_labels_round_trip() {
        for appearance in Appearance::ALL {
            assert_eq!(appearance_from_title(appearance.label()), appearance);
        }
    }
}
