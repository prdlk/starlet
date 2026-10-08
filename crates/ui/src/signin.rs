//! GitHub App device-flow sign-in.
//!
//! The dialog shows the user code, copies it, and opens the verification page.
//! Polling runs on the I/O runtime; the dialog only reflects state. The token
//! goes straight from the poll result into the keychain and the session — it is
//! never held in view state and never logged.

use gpui::{
    AnyWindowHandle, App, AppContext as _, ClipboardItem, Context, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    progress::Progress,
    v_flex,
};
use starlet_sync::{DeviceFlow, GitHub, PollOutcome, TokenStore};

use crate::search::{SearchView, SyncStatus};
use crate::services::{AuthStatus, Backend, Session};

/// Where the dialog is in the flow.
#[derive(Debug, Clone)]
enum Stage {
    Requesting,
    /// Waiting for the user to enter `user_code` at `verification_uri`.
    Waiting {
        user_code: SharedString,
        verification_uri: SharedString,
    },
    Succeeded {
        login: SharedString,
    },
    Failed(SharedString),
}

pub struct SignInFlow {
    stage: Stage,
    /// The workspace runs the first sync; the dialog mirrors its progress.
    workspace: WeakEntity<SearchView>,
    window: AnyWindowHandle,
    /// Set once the sync has been seen running, so a later idle status means
    /// it finished rather than that it has not started.
    sync_started: bool,
    _task: Option<Task<()>>,
    _observe: Option<Subscription>,
}

impl SignInFlow {
    /// Open the dialog and begin the flow.
    pub fn start(workspace: WeakEntity<SearchView>, window: &mut Window, cx: &mut App) {
        let Some(client_id) = starlet_sync::client_id() else {
            window.push_notification(
                gpui_component::notification::Notification::error(
                    "No GitHub App client id. Set STARLET_GITHUB_CLIENT_ID and rebuild, or export it before launching. See the README.",
                ),
                cx,
            );
            return;
        };

        let handle = window.window_handle();
        let view = cx.new(|cx| SignInFlow::new(client_id, workspace, handle, cx));
        let content = view.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Sign in to GitHub")
                .w(px(420.))
                .child(content.clone())
        });
    }

    /// Forget the token and drop the client.
    pub fn sign_out(cx: &mut App) {
        if let Err(err) = TokenStore::clear() {
            tracing::warn!("could not clear the keychain entry: {err}");
        }
        cx.set_global(Session::signed_out());
        cx.refresh_windows();
    }

    fn new(
        client_id: String,
        workspace: WeakEntity<SearchView>,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = workspace
            .upgrade()
            .map(|ws| cx.observe(&ws, |this, _, cx| this.sync_changed(cx)));
        let mut this = Self {
            stage: Stage::Requesting,
            workspace,
            window,
            sync_started: false,
            _task: None,
            _observe: observe,
        };
        this.request(client_id, cx);
        this
    }

    fn sync_status(&self, cx: &App) -> Option<SyncStatus> {
        let ws = self.workspace.upgrade()?;
        Some(ws.read(cx).sync_status().clone())
    }

    /// Follow the workspace's sync; close once the first one has finished.
    fn sync_changed(&mut self, cx: &mut Context<Self>) {
        match self.sync_status(cx) {
            Some(SyncStatus::Running { .. }) => self.sync_started = true,
            Some(SyncStatus::Idle)
                if self.sync_started && matches!(self.stage, Stage::Succeeded { .. }) =>
            {
                let _ = self
                    .window
                    .update(cx, |_, window, cx| window.close_dialog(cx));
                return;
            }
            _ => {}
        }
        cx.notify();
    }

    fn request(&mut self, client_id: String, cx: &mut Context<Self>) {
        let rx = Backend::global(cx).spawn(async move {
            let flow = DeviceFlow::new(client_id)?;
            let grant = flow.request_code().await?;
            Ok::<_, starlet_sync::SyncError>((flow, grant))
        });

        self._task = Some(cx.spawn(async move |this, cx| {
            let outcome = match rx.await {
                Ok(Ok(pair)) => pair,
                Ok(Err(err)) => {
                    let _ = this.update(cx, |this, cx| this.fail(err.to_string(), cx));
                    return;
                }
                Err(_) => return,
            };
            let (flow, grant) = outcome;

            let _ = this.update(cx, |this, cx| {
                this.stage = Stage::Waiting {
                    user_code: grant.user_code.clone().into(),
                    verification_uri: grant.verification_uri.clone().into(),
                };
                cx.set_global(Session {
                    status: AuthStatus::Pending {
                        user_code: grant.user_code.clone(),
                        verification_uri: grant.verification_uri.clone(),
                    },
                    github: None,
                    viewer: None,
                });
                cx.notify();
            });

            // Poll until GitHub answers, the grant expires, or the view is gone.
            let mut interval = grant.poll_interval();
            let deadline = std::time::Instant::now() + grant.expires_in();
            let device_code = grant.device_code.clone();

            loop {
                cx.background_executor().timer(interval).await;
                if std::time::Instant::now() > deadline {
                    let _ = this.update(cx, |this, cx| {
                        this.fail("The sign-in code expired. Try again.".into(), cx)
                    });
                    return;
                }

                let flow = flow.clone();
                let device_code = device_code.clone();
                let Ok(rx) = this.update(cx, |_, cx| {
                    Backend::global(cx).spawn(async move { flow.poll_once(&device_code).await })
                }) else {
                    return;
                };

                match rx.await {
                    Ok(Ok(PollOutcome::Pending)) => continue,
                    Ok(Ok(PollOutcome::SlowDown(next))) => {
                        interval = next;
                        continue;
                    }
                    Ok(Ok(PollOutcome::Authorized(token))) => {
                        let _ = this.update(cx, |this, cx| this.authorized(token, cx));
                        return;
                    }
                    Ok(Err(err)) => {
                        let _ = this.update(cx, |this, cx| this.fail(err.to_string(), cx));
                        return;
                    }
                    Err(_) => return,
                }
            }
        }));
    }

    /// Persist the token, install the client, and identify the viewer.
    fn authorized(&mut self, token: String, cx: &mut Context<Self>) {
        let saved = Backend::global(cx).spawn_blocking({
            let token = token.clone();
            move || TokenStore::save(&token)
        });

        let github = match GitHub::new(token) {
            Ok(github) => github,
            Err(err) => return self.fail(err.to_string(), cx),
        };
        cx.set_global(Session {
            status: AuthStatus::SignedIn,
            github: Some(github.clone()),
            viewer: None,
        });

        let viewer_rx = Backend::global(cx).spawn(async move { github.viewer().await });
        self._task = Some(cx.spawn(async move |this, cx| {
            if let Ok(Err(err)) = saved.await {
                tracing::warn!("could not store the token in the keychain: {err}");
            }
            let viewer = match viewer_rx.await {
                Ok(Ok(viewer)) => viewer,
                _ => return,
            };
            let login = viewer.login.clone();
            let _ = cx.update(|cx| {
                if let Some(session) = cx.try_global::<Session>() {
                    let github = session.github.clone();
                    cx.set_global(Session {
                        status: AuthStatus::SignedIn,
                        github,
                        viewer: Some(viewer),
                    });
                }
            });
            let _ = this.update(cx, |this, cx| {
                this.stage = Stage::Succeeded {
                    login: login.into(),
                };
                // The sync may already be done for a small account.
                this.sync_changed(cx);
            });
        }));
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.stage = Stage::Failed(message.clone().into());
        cx.set_global(Session {
            status: AuthStatus::Failed(message),
            github: None,
            viewer: None,
        });
        cx.notify();
    }
}

impl Render for SignInFlow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.stage.clone() {
            Stage::Requesting => v_flex()
                .gap_2()
                .child(div().text_sm().child("Requesting a device code…"))
                .into_any_element(),

            Stage::Waiting {
                user_code,
                verification_uri,
            } => v_flex()
                .gap_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Enter this code on GitHub to finish signing in."),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_lg()
                                .child(user_code.clone()),
                        )
                        .child(
                            Button::new("copy-code")
                                .outline()
                                .small()
                                .icon(IconName::Copy)
                                .label("Copy")
                                .on_click({
                                    let code = user_code.clone();
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            code.to_string(),
                                        ))
                                    }
                                }),
                        )
                        .child(
                            Button::new("open-github")
                                .primary()
                                .small()
                                .icon(IconName::ExternalLink)
                                .label("Open GitHub")
                                .on_click({
                                    let uri = verification_uri.clone();
                                    move |_, _, cx| {
                                        let uri = uri.to_string();
                                        Backend::global(cx).spawn_blocking(move || {
                                            let _ = open::that_detached(&uri);
                                        });
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(verification_uri),
                )
                .into_any_element(),

            Stage::Succeeded { login } => {
                let signed_in = div()
                    .text_sm()
                    .child(SharedString::from(format!("Signed in as {login}.")));
                let muted = cx.theme().muted_foreground;
                match self.sync_status(cx) {
                    Some(SyncStatus::Running { phase, done, total }) => {
                        let count = match total {
                            Some(total) => format!("{} of {total}", done.min(total)),
                            None => done.to_string(),
                        };
                        v_flex()
                            .gap_3()
                            .child(signed_in)
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(phase.label())
                                    .child(SharedString::from(count)),
                            )
                            // Without a total there is nothing to fill
                            // against; the climbing count carries it.
                            .when_some(total, |this, total| {
                                let pct = if total == 0 {
                                    100.0
                                } else {
                                    done as f32 / total as f32 * 100.0
                                };
                                this.child(Progress::new().value(pct))
                            })
                            .into_any_element()
                    }
                    Some(SyncStatus::Failed(message)) => v_flex()
                        .gap_3()
                        .child(signed_in)
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(SharedString::from(message)),
                        )
                        .into_any_element(),
                    _ => v_flex()
                        .gap_3()
                        .child(signed_in)
                        .child(div().text_xs().text_color(muted).child("Starting sync…"))
                        .child(Progress::new())
                        .into_any_element(),
                }
            }

            Stage::Failed(message) => v_flex()
                .gap_2()
                .child(div().text_sm().text_color(cx.theme().danger).child(message))
                .into_any_element(),
        };

        v_flex().gap_4().min_w(px(340.)).child(body)
    }
}
