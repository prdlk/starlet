//! The BYOK analysis run.
//!
//! Nothing here happens on launch. The user opens this dialog, sees what the
//! run will cost, and confirms. During the run the dialog reports progress and
//! offers Cancel; results are written to the store batch by batch, so a
//! cancelled run keeps whatever it had already produced.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, WeakEntity, Window, div, px,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    progress::Progress,
    v_flex,
};
use starlet_ai::{AiEvent, AiProvider, CostEstimate};
use starlet_core::model::{RepoSummary, RepoTag, TagSource};

use crate::search::SearchView;
use crate::services::Backend;

#[derive(Debug, Clone)]
enum Stage {
    /// Waiting for the user to confirm, with the estimate in hand.
    Confirm {
        repos: usize,
        estimate: CostEstimate,
        provider: SharedString,
        model: SharedString,
    },
    Running {
        done: usize,
        total: usize,
    },
    Done {
        repos: usize,
        cost: f64,
    },
    Cancelled,
    Blocked(SharedString),
}

pub struct AnalyzeDialog {
    owner: WeakEntity<SearchView>,
    stage: Stage,
    /// Cleared by Cancel; the orchestrator checks it between batches.
    cancel: Arc<AtomicBool>,
    summaries: Vec<RepoSummary>,
    existing_tags: HashMap<String, Vec<String>>,
    _task: Option<Task<()>>,
}

impl AnalyzeDialog {
    pub fn open(owner: Entity<SearchView>, window: &mut Window, cx: &mut App) {
        let repos = owner.read(cx).repos().to_vec();
        let view = cx.new(|cx| AnalyzeDialog::new(owner.downgrade(), repos, cx));
        let content = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Analyze with AI")
                .w(px(460.))
                .child(content.clone())
        });
    }

    fn new(
        owner: WeakEntity<SearchView>,
        repos: Vec<Arc<starlet_core::model::Repo>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let summaries: Vec<RepoSummary> = repos.iter().map(|r| RepoSummary::from(&**r)).collect();
        let existing_tags: HashMap<String, Vec<String>> = repos
            .iter()
            .map(|r| {
                (
                    r.full_name.clone(),
                    r.tags.iter().map(|t| t.name.clone()).collect(),
                )
            })
            .collect();

        let mut this = Self {
            owner,
            stage: Stage::Blocked("Loading provider settings…".into()),
            cancel: Arc::new(AtomicBool::new(false)),
            summaries,
            existing_tags,
            _task: None,
        };
        this.prepare(cx);
        this
    }

    /// Resolve the configured provider and price the run before asking.
    fn prepare(&mut self, cx: &mut Context<Self>) {
        let store = Backend::global(cx).store();
        let rx = Backend::global(cx).spawn(async move {
            let provider = store
                .get_state(starlet_store::KEY_AI_PROVIDER)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| starlet_ai::openai::ID.to_string());
            let model = store
                .get_state(starlet_store::KEY_AI_MODEL)
                .await
                .ok()
                .flatten();
            let endpoint = store
                .get_state(starlet_store::KEY_AI_ENDPOINT)
                .await
                .ok()
                .flatten();
            (provider, model, endpoint)
        });

        let count = self.summaries.len();
        cx.spawn(async move |this, cx| {
            let Ok((provider_id, model, endpoint)) = rx.await else {
                return;
            };
            let key_rx = match this.update(cx, |_, cx| {
                let id = provider_id.clone();
                Backend::global(cx)
                    .spawn_blocking(move || starlet_sync::ProviderKeyStore::load(&id))
            }) {
                Ok(rx) => rx,
                Err(_) => return,
            };
            let key = key_rx.await.ok().flatten().unwrap_or_default();

            let _ = this.update(cx, |this, cx| {
                if count == 0 {
                    this.stage = Stage::Blocked("Sync your stars first.".into());
                    cx.notify();
                    return;
                }
                let Some(provider) =
                    build_provider(&provider_id, &key, model.as_deref(), endpoint.as_deref())
                else {
                    this.stage = Stage::Blocked(
                        format!("Add a {provider_id} API key in Settings to run the analysis.")
                            .into(),
                    );
                    cx.notify();
                    return;
                };
                this.stage = Stage::Confirm {
                    repos: count,
                    estimate: provider.estimate(count),
                    provider: SharedString::from(provider.id()),
                    model: SharedString::from(provider.model().to_string()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let Stage::Confirm { repos, .. } = self.stage.clone() else {
            return;
        };
        self.cancel.store(false, Ordering::Relaxed);
        self.stage = Stage::Running {
            done: 0,
            total: repos,
        };
        cx.notify();

        let store = Backend::global(cx).store();
        let summaries = self.summaries.clone();
        let existing = self.existing_tags.clone();
        let cancel = self.cancel.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        Backend::global(cx).spawn(async move {
            let provider_id = store
                .get_state(starlet_store::KEY_AI_PROVIDER)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| starlet_ai::openai::ID.to_string());
            let model = store
                .get_state(starlet_store::KEY_AI_MODEL)
                .await
                .ok()
                .flatten();
            let endpoint = store
                .get_state(starlet_store::KEY_AI_ENDPOINT)
                .await
                .ok()
                .flatten();
            let key = starlet_sync::ProviderKeyStore::load(&provider_id).unwrap_or_default();
            let Some(provider) =
                build_provider(&provider_id, &key, model.as_deref(), endpoint.as_deref())
            else {
                let _ = tx.send(AiEvent::Failed("No provider configured".into()));
                return;
            };

            let run_id = store
                .begin_ai_run(provider.id(), provider.model(), summaries.len() as i64)
                .await
                .unwrap_or_default();

            let result =
                starlet_ai::analyze(provider.as_ref(), &summaries, &existing, tx.clone(), cancel)
                    .await;

            if let Err(err) = result {
                let _ = tx.send(AiEvent::Failed(err.to_string()));
                return;
            }
            let cost = provider.estimate(summaries.len()).usd;
            let _ = store
                .finish_ai_run(run_id, summaries.len() as i64, cost)
                .await;
        });

        // Persist each batch as it lands so a cancel keeps partial work.
        self._task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let alive = this.update(cx, |this, cx| this.apply(event, cx)).is_ok();
                if !alive {
                    break;
                }
            }
        }));
    }

    fn apply(&mut self, event: AiEvent, cx: &mut Context<Self>) {
        match event {
            AiEvent::Started { batches } => {
                self.stage = Stage::Running {
                    done: 0,
                    total: batches * starlet_ai::BATCH_SIZE,
                };
            }
            AiEvent::Progress { done, total } => {
                self.stage = Stage::Running { done, total };
            }
            AiEvent::Tagged(batch) => {
                let store = Backend::global(cx).store();
                Backend::global(cx).spawn(async move {
                    for entry in batch {
                        let Ok(Some(id)) = store.repo_id_by_full_name(&entry.full_name).await
                        else {
                            continue;
                        };
                        let tags: Vec<RepoTag> = entry
                            .tags
                            .into_iter()
                            .map(|t| RepoTag {
                                source: TagSource::Ai,
                                ..t
                            })
                            .collect();
                        let _ = store.set_ai_tags(id, &tags).await;
                    }
                });
            }
            AiEvent::Grouped(groups) => {
                let store = Backend::global(cx).store();
                Backend::global(cx).spawn(async move {
                    let _ = store.replace_ai_groups(&groups).await;
                });
            }
            AiEvent::Finished { repos, cost } => {
                self.stage = Stage::Done { repos, cost };
                self.reload_owner(cx);
            }
            AiEvent::Cancelled => {
                self.stage = Stage::Cancelled;
                self.reload_owner(cx);
            }
            AiEvent::Failed(message) => {
                self.stage = Stage::Blocked(message.into());
                self.reload_owner(cx);
            }
        }
        cx.notify();
    }

    /// Tag writes happen on the I/O runtime, so give them a moment to land
    /// before asking the workspace to re-read.
    fn reload_owner(&mut self, cx: &mut Context<Self>) {
        let owner = self.owner.clone();
        self._task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            let _ = owner.update(cx, |view, cx| view.refresh_after_tag_change(cx));
        }));
    }
}

fn build_provider(
    id: &str,
    key: &str,
    model: Option<&str>,
    endpoint: Option<&str>,
) -> Option<Box<dyn AiProvider>> {
    let local = id == starlet_ai::ollama::ID;
    if key.trim().is_empty() && !local {
        return None;
    }
    let model = model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| default_model(id).to_string());

    let provider = starlet_ai::provider_for(id, key, model)?;
    // Only Ollama has a user-configurable origin; the hosted providers keep
    // their published endpoints so a typo cannot silently exfiltrate a key.
    if local && let Some(endpoint) = endpoint.map(str::trim).filter(|e| !e.is_empty()) {
        return Some(Box::new(
            starlet_ai::Ollama::new("", provider.model().to_string()).with_base_url(endpoint),
        ));
    }
    Some(provider)
}

fn default_model(id: &str) -> &'static str {
    match id {
        starlet_ai::anthropic::ID => starlet_ai::anthropic::DEFAULT_MODEL,
        starlet_ai::ollama::ID => starlet_ai::ollama::DEFAULT_MODEL,
        _ => starlet_ai::openai::DEFAULT_MODEL,
    }
}

impl Render for AnalyzeDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.stage.clone() {
            Stage::Confirm {
                repos,
                estimate,
                provider,
                model,
            } => v_flex()
                .gap_4()
                .child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child(SharedString::from(format!(
                            "Tag and group {repos} repositories with {provider}."
                        ))))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(format!("Model: {model}"))),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_family(cx.theme().mono_font_family.clone())
                                .child(SharedString::from(if estimate.usd <= 0.0 {
                                    "No cost — runs on your machine".to_string()
                                } else {
                                    format!("≈ ${:.2}", estimate.usd)
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(format!(
                                    "≈ {} input and {} output tokens. An upper bound, not a quote.",
                                    estimate.input_tokens, estimate.output_tokens
                                ))),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel-analyze")
                                .small()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("run-analyze")
                                .primary()
                                .small()
                                .label("Analyze")
                                .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                        ),
                )
                .into_any_element(),

            Stage::Running { done, total } => {
                let fraction = if total == 0 {
                    0.0
                } else {
                    done as f32 / total as f32
                };
                v_flex()
                    .gap_4()
                    .child(div().text_sm().child(SharedString::from(format!(
                        "Analyzing {done} of {total} repositories…"
                    ))))
                    .child(Progress::new().value(fraction * 100.0))
                    .child(
                        h_flex().justify_end().child(
                            Button::new("stop-analyze").small().label("Stop").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.cancel.store(true, Ordering::Relaxed);
                                    cx.notify();
                                }),
                            ),
                        ),
                    )
                    .into_any_element()
            }

            Stage::Done { repos, cost } => v_flex()
                .gap_4()
                .child(div().text_sm().child(SharedString::from(format!(
                    "Tagged {repos} repositories{}.",
                    if cost > 0.0 {
                        format!(" for about ${cost:.2}")
                    } else {
                        String::new()
                    }
                ))))
                .child(
                    h_flex().justify_end().child(
                        Button::new("close-analyze")
                            .primary()
                            .small()
                            .label("Done")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
                .into_any_element(),

            Stage::Cancelled => v_flex()
                .gap_4()
                .child(
                    div()
                        .text_sm()
                        .child("Stopped. Tags produced before you stopped were kept."),
                )
                .child(
                    h_flex().justify_end().child(
                        Button::new("close-analyze")
                            .small()
                            .label("Close")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
                .into_any_element(),

            Stage::Blocked(message) => v_flex()
                .gap_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("open-settings")
                                .small()
                                .label("Settings…")
                                .on_click(|_, window, cx| {
                                    window.close_dialog(cx);
                                    crate::settings::SettingsDialog::open(window, cx);
                                }),
                        )
                        .child(
                            Button::new("close-analyze")
                                .small()
                                .label("Close")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                )
                .into_any_element(),
        };

        v_flex().gap_4().min_w(px(400.)).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hosted_provider_without_a_key_is_refused() {
        assert!(build_provider("openai", "", None, None).is_none());
        assert!(build_provider("anthropic", "   ", None, None).is_none());
    }

    #[test]
    fn ollama_needs_no_key() {
        let provider = build_provider("ollama", "", None, None).expect("local provider");
        assert_eq!(provider.id(), "ollama");
        assert_eq!(provider.estimate(1_000).usd, 0.0);
    }

    #[test]
    fn a_blank_model_falls_back_to_the_provider_default() {
        let provider = build_provider("openai", "sk-test", Some("  "), None).unwrap();
        assert_eq!(provider.model(), starlet_ai::openai::DEFAULT_MODEL);
    }

    #[test]
    fn an_unknown_provider_id_is_refused() {
        assert!(build_provider("nope", "sk-test", None, None).is_none());
    }
}
