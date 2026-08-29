//! The repository detail sheet.
//!
//! Opens with everything already in memory and fills in the two expensive
//! fields — contributors and the README — from the network on first open. Both
//! are cached in SQLite, the README for seven days, so reopening a sheet is
//! free.

use std::sync::Arc;

use chrono::Utc;
use gpui::{
    App, AppContext as _, ClipboardItem, Context, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Task, Window, div, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::{
    ActiveTheme as _, IconName, Sizable as _, WindowExt as _,
    avatar::Avatar,
    button::{Button, ButtonVariants as _},
    divider::Divider,
    h_flex, v_flex,
};
use starlet_core::model::{Contributor, Repo, TagSource};
use starlet_sync::SyncEngine;

use crate::format;
use crate::results::language_dot;
use crate::services::{Backend, Session};

/// Asynchronously-filled panel content.
#[derive(Debug, Clone, PartialEq)]
enum Fetch<T> {
    Idle,
    Loading,
    Ready(T),
    /// Nothing to show, and that is the final answer.
    Empty,
}

pub struct DetailSheet {
    repo: Arc<Repo>,
    contributors: Fetch<Vec<Contributor>>,
    readme: Fetch<String>,
    _task: Option<Task<()>>,
}

impl DetailSheet {
    /// Open the sheet for `repo`.
    pub fn open(repo: Arc<Repo>, window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| DetailSheet::new(repo, cx));
        let content = view.clone();
        window.open_sheet(cx, move |sheet, _, _| {
            sheet.size(px(520.)).child(content.clone())
        });
    }

    fn new(repo: Arc<Repo>, cx: &mut Context<Self>) -> Self {
        let cached = repo.contributors.clone();
        let mut this = Self {
            repo,
            contributors: if cached.is_empty() {
                Fetch::Idle
            } else {
                Fetch::Ready(cached)
            },
            readme: Fetch::Idle,
            _task: None,
        };
        this.fetch(cx);
        this
    }

    /// Fetch whatever is missing. Without a session both stay empty rather
    /// than showing a spinner that can never resolve.
    fn fetch(&mut self, cx: &mut Context<Self>) {
        let Some(github) = Session::client(cx) else {
            if matches!(self.contributors, Fetch::Idle) {
                self.contributors = Fetch::Empty;
            }
            self.readme = Fetch::Empty;
            return;
        };

        let store = Backend::global(cx).store();
        let id = self.repo.id;
        let full_name = self.repo.full_name.clone();
        let want_contributors = matches!(self.contributors, Fetch::Idle);

        if want_contributors {
            self.contributors = Fetch::Loading;
        }
        self.readme = Fetch::Loading;

        let rx = Backend::global(cx).spawn(async move {
            let engine = SyncEngine::new(github, store);
            let contributors = if want_contributors {
                engine.fetch_contributors(id, &full_name).await.ok()
            } else {
                None
            };
            let readme = engine.fetch_readme(id, &full_name).await.ok().flatten();
            (contributors, readme)
        });

        self._task = Some(cx.spawn(async move |this, cx| {
            let Ok((contributors, readme)) = rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(contributors) = contributors {
                    this.contributors = if contributors.is_empty() {
                        Fetch::Empty
                    } else {
                        Fetch::Ready(contributors)
                    };
                } else if matches!(this.contributors, Fetch::Loading) {
                    this.contributors = Fetch::Empty;
                }
                this.readme = match readme {
                    Some(md) if !md.trim().is_empty() => Fetch::Ready(md),
                    _ => Fetch::Empty,
                };
                cx.notify();
            });
        }));
    }

    fn render_header(&self, cx: &App) -> gpui::AnyElement {
        let repo = self.repo.clone();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(format!("{}/", repo.owner))),
                    )
                    .child(div().text_lg().child(SharedString::from(repo.name.clone()))),
            )
            .when_some(repo.description.clone(), |this, description| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(description)),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("open-repo")
                            .primary()
                            .small()
                            .icon(IconName::ExternalLink)
                            .label("Open on GitHub")
                            .on_click({
                                let url = repo.html_url.clone();
                                move |_, _, cx| {
                                    let url = url.clone();
                                    Backend::global(cx).spawn_blocking(move || {
                                        let _ = open::that_detached(&url);
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("copy-repo-url")
                            .outline()
                            .small()
                            .icon(IconName::Copy)
                            .label("Copy URL")
                            .on_click({
                                let url = repo.html_url.clone();
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))
                                }
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_facts(&self, cx: &App) -> gpui::AnyElement {
        let repo = &self.repo;
        let now = Utc::now();
        v_flex()
            .gap_2()
            .child(fact("Stars", format::compact_count(repo.stargazers), cx))
            .child(fact(
                "Last commit",
                format::absolute_date(repo.last_commit_at),
                cx,
            ))
            .child(fact("Starred", format::absolute_date(repo.starred_at), cx))
            .child(fact(
                "Synced",
                format::relative_time(repo.synced_at, now),
                cx,
            ))
            .when(repo.archived, |this| {
                this.child(fact("State", SharedString::new_static("Archived"), cx))
            })
            .when(repo.fork, |this| {
                this.child(fact("Kind", SharedString::new_static("Fork"), cx))
            })
            .into_any_element()
    }

    fn render_languages(&self, cx: &App) -> Option<gpui::AnyElement> {
        let shares = format::language_shares(&self.repo.languages);
        if shares.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .gap_2()
                .child(section("Languages", cx))
                .child(
                    h_flex()
                        .w_full()
                        .h(px(6.))
                        .rounded_full()
                        .overflow_hidden()
                        .children(shares.iter().map(|(name, share)| {
                            div()
                                .h_full()
                                .w(relative(*share))
                                .bg(format::language_color(name))
                        })),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_3()
                        .children(shares.iter().map(|(name, share)| {
                            h_flex()
                                .gap_1p5()
                                .items_center()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(language_dot(name))
                                .child(SharedString::from(format!("{name} {:.1}%", share * 100.0)))
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_tags(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if self.repo.tags.is_empty() {
            return None;
        }
        let repo_id = self.repo.id;
        Some(
            v_flex()
                .gap_2()
                .child(section("Tags", cx))
                .child(h_flex().flex_wrap().gap_1p5().children(
                    self.repo.tags.iter().enumerate().map(|(ix, tag)| {
                        let ai = tag.source == TagSource::Ai;
                        let name = tag.name.clone();
                        h_flex()
                            .gap_1()
                            .items_center()
                            .px_2()
                            .py_0p5()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .text_xs()
                            .text_color(if ai {
                                cx.theme().muted_foreground
                            } else {
                                cx.theme().foreground
                            })
                            .child(SharedString::from(tag.name.clone()))
                            .when(ai, |this| {
                                // An AI tag is a suggestion until the user keeps
                                // it; promoting turns it into a user tag that no
                                // later run can overwrite.
                                this.child(
                                    Button::new(("promote-tag", ix))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Check)
                                        .tooltip("Keep this tag")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.promote_tag(repo_id, name.clone(), cx)
                                        })),
                                )
                            })
                    }),
                ))
                .into_any_element(),
        )
    }

    fn promote_tag(&mut self, repo_id: i64, name: String, cx: &mut Context<Self>) {
        let store = Backend::global(cx).store();
        let rx = Backend::global(cx).spawn(async move {
            let _ = store.promote_tag(repo_id, &name).await;
            store.load_repo(repo_id).await.ok().flatten()
        });
        cx.spawn(async move |this, cx| {
            let Ok(Some(repo)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.repo = Arc::new(repo);
                cx.notify();
            });
        })
        .detach();
    }

    fn render_contributors(&self, cx: &App) -> gpui::AnyElement {
        let body = match &self.contributors {
            Fetch::Ready(contributors) => h_flex()
                .flex_wrap()
                .gap_2()
                .children(contributors.iter().map(|c| {
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(Avatar::new().src(c.avatar_url.clone()).xsmall())
                        .child(div().text_xs().child(SharedString::from(c.login.clone())))
                        .child(
                            div()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().muted_foreground)
                                .child(format::compact_count(c.contributions)),
                        )
                }))
                .into_any_element(),
            Fetch::Loading => muted("Loading contributors…", cx),
            Fetch::Idle | Fetch::Empty => muted("No contributor data", cx),
        };
        v_flex()
            .gap_2()
            .child(section("Contributors", cx))
            .child(body)
            .into_any_element()
    }

    fn render_readme(&self, cx: &App) -> gpui::AnyElement {
        let body = match &self.readme {
            // Deliberately plain text: rendering Markdown here would mean a
            // second, differently-styled type system inside a dense inspector.
            // The full document is one click away on GitHub.
            Fetch::Ready(markdown) => div()
                .max_h(px(280.))
                .overflow_hidden()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(preview(markdown)))
                .into_any_element(),
            Fetch::Loading => muted("Loading README…", cx),
            Fetch::Idle | Fetch::Empty => muted("No README", cx),
        };
        v_flex()
            .gap_2()
            .child(section("README", cx))
            .child(body)
            .into_any_element()
    }
}

/// First ~40 lines with the heaviest Markdown punctuation stripped.
fn preview(markdown: &str) -> String {
    markdown
        .lines()
        .filter(|line| !line.trim_start().starts_with("<!--"))
        .take(40)
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn section(title: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(title)
}

fn muted(text: &'static str, cx: &App) -> gpui::AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

fn fact(label: &'static str, value: SharedString, cx: &App) -> impl IntoElement {
    h_flex()
        .justify_between()
        .text_sm()
        .child(div().text_color(cx.theme().muted_foreground).child(label))
        .child(
            div()
                .font_family(cx.theme().mono_font_family.clone())
                .child(value),
        )
}

impl Render for DetailSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `render_tags` needs `&mut Context` (it installs listeners); everything
        // else only reads. Resolve them into owned elements in that order so
        // the borrows never overlap.
        let tags = self.render_tags(cx);
        let languages = self.render_languages(cx);
        let header = self.render_header(cx);
        let facts = self.render_facts(cx);
        let contributors = self.render_contributors(cx);
        let readme = self.render_readme(cx);

        v_flex()
            .gap_5()
            .pb_6()
            .child(header)
            .child(Divider::horizontal())
            .child(facts)
            .when_some(languages, |this, languages| {
                this.child(Divider::horizontal()).child(languages)
            })
            .when_some(tags, |this, tags| {
                this.child(Divider::horizontal()).child(tags)
            })
            .child(Divider::horizontal())
            .child(contributors)
            .child(Divider::horizontal())
            .child(readme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readme_preview_is_bounded_and_drops_comments() {
        let markdown = format!("<!-- hidden -->\n{}", "line\n".repeat(100));
        let preview = preview(&markdown);
        assert_eq!(preview.lines().count(), 40);
        assert!(!preview.contains("hidden"));
    }
}
