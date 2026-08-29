//! The results table.
//!
//! The delegate holds an immutable snapshot of every repository plus a `Vec`
//! of indices describing the current ranking. A keystroke replaces the order
//! and nothing else, so re-ranking costs one small allocation and no row data
//! is copied. The snapshot is only rebuilt when a sync changes the data.

use std::sync::Arc;

use chrono::Utc;
use gpui::{
    App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Stateful, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, h_flex,
    table::{Column, ColumnSort, TableDelegate, TableState},
    v_flex,
};
use starlet_core::model::{Repo, TagSource};

use crate::format;

/// Stable column keys. Used for width persistence and for mapping a header
/// click onto a sort key.
pub const COL_NAME: &str = "name";
pub const COL_DESCRIPTION: &str = "description";
pub const COL_LANGUAGE: &str = "language";
pub const COL_STARS: &str = "stars";
pub const COL_LAST_COMMIT: &str = "last_commit";
pub const COL_TAGS: &str = "tags";

/// What the delegate reports upward when a header is clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortRequest {
    pub column: &'static str,
    pub sort: ColumnSort,
}

pub struct ResultsDelegate {
    /// Every repository, shared with the search view. Replaced, never mutated.
    repos: Arc<Vec<Arc<Repo>>>,
    /// Indices into `repos`, in rank order. This is the table's row space.
    order: Vec<usize>,
    columns: Vec<Column>,
    /// Set by `perform_sort`, drained by the search view on the next frame.
    pending_sort: Option<SortRequest>,
    /// Shown instead of the empty state while the first sync is running.
    loading: bool,
}

impl ResultsDelegate {
    pub fn new() -> Self {
        Self {
            repos: Arc::new(Vec::new()),
            order: Vec::new(),
            columns: default_columns(),
            pending_sort: None,
            loading: false,
        }
    }

    /// Swap in a new data snapshot. Called after a sync, not per keystroke.
    pub fn set_repos(&mut self, repos: Arc<Vec<Arc<Repo>>>) {
        self.repos = repos;
    }

    /// Swap in a new ranking. This is the per-keystroke path.
    pub fn set_order(&mut self, order: Vec<usize>) {
        self.order = order;
    }

    pub fn set_loading(&mut self, loading: bool) {
        self.loading = loading;
    }

    /// The repository at a table row, if the row still exists.
    pub fn repo_at(&self, row_ix: usize) -> Option<&Arc<Repo>> {
        self.repos.get(*self.order.get(row_ix)?)
    }

    pub fn take_sort_request(&mut self) -> Option<SortRequest> {
        self.pending_sort.take()
    }

    /// Column widths in declaration order, for persistence.
    pub fn column_widths(&self) -> Vec<f32> {
        self.columns.iter().map(|c| f32::from(c.width)).collect()
    }

    /// Restore persisted widths. Extra or missing entries are ignored so an
    /// older layout never breaks the table.
    pub fn set_column_widths(&mut self, widths: &[f32]) {
        for (column, width) in self.columns.iter_mut().zip(widths) {
            if *width >= 40.0 && *width <= 1200.0 {
                column.width = px(*width);
            }
        }
    }

    /// Mirror the active sort onto the header indicators.
    pub fn set_sort_indicator(&mut self, column: &str, sort: ColumnSort) {
        for c in &mut self.columns {
            if c.sort.is_none() {
                continue;
            }
            c.sort = Some(if c.key == column {
                sort
            } else {
                ColumnSort::Default
            });
        }
    }
}

impl Default for ResultsDelegate {
    fn default() -> Self {
        Self::new()
    }
}

fn default_columns() -> Vec<Column> {
    vec![
        // Fixed so the identity column stays visible while the rest scrolls.
        Column::new(COL_NAME, "Name")
            .width(px(260.))
            .fixed_left()
            .movable(false)
            .sortable(),
        Column::new(COL_DESCRIPTION, "Description").width(px(380.)),
        Column::new(COL_LANGUAGE, "Language").width(px(128.)),
        Column::new(COL_STARS, "Stars")
            .width(px(80.))
            .text_right()
            .sortable(),
        Column::new(COL_LAST_COMMIT, "Last commit")
            .width(px(108.))
            .text_right()
            .sortable(),
        Column::new(COL_TAGS, "Tags").width(px(240.)),
    ]
}

impl TableDelegate for ResultsDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.order.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> &Column {
        &self.columns[col_ix]
    }

    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(column) = self.columns.get(col_ix) else {
            return;
        };
        let key = match column.key.as_ref() {
            COL_NAME => COL_NAME,
            COL_STARS => COL_STARS,
            COL_LAST_COMMIT => COL_LAST_COMMIT,
            _ => return,
        };
        // The delegate does not own the ranking, so it records the intent and
        // lets the search view re-rank through the same path a query uses.
        self.pending_sort = Some(SortRequest { column: key, sort });
        self.set_sort_indicator(key, sort);
        cx.notify();
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        h_flex()
            .size_full()
            .when(matches!(column.align, gpui::TextAlign::Right), |this| {
                this.justify_end()
            })
            .text_xs()
            .text_color(cx.theme().table_head_foreground)
            .child(column.name.clone())
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        // Identity comes from the repository, not the row position, so a
        // re-rank does not reset hover or selection state onto a new repo.
        let id = self.repo_at(row_ix).map(|r| r.id).unwrap_or(-1);
        div().id(("repo-row", id as u64))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(repo) = self.repo_at(row_ix) else {
            return div().into_any_element();
        };
        let key = self.columns[col_ix].key.clone();

        match key.as_ref() {
            COL_NAME => name_cell(repo, cx),
            COL_DESCRIPTION => div()
                .w_full()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(
                    repo.description.clone().unwrap_or_default(),
                ))
                .into_any_element(),
            COL_LANGUAGE => language_cell(repo, cx),
            COL_STARS => h_flex()
                .w_full()
                .justify_end()
                .font_family(cx.theme().mono_font_family.clone())
                .child(format::compact_count(repo.stargazers))
                .into_any_element(),
            COL_LAST_COMMIT => h_flex()
                .w_full()
                .justify_end()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(cx.theme().muted_foreground)
                .child(format::relative_time(repo.last_commit_at, Utc::now()))
                .into_any_element(),
            COL_TAGS => tags_cell(repo, cx),
            _ => div().into_any_element(),
        }
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(IconName::Search).size_5())
            .child(div().text_sm().child("No repositories match this search"))
            .into_any_element()
    }
}

fn name_cell(repo: &Arc<Repo>, cx: &App) -> gpui::AnyElement {
    h_flex()
        .w_full()
        .gap_1p5()
        .min_w_0()
        .child(
            div()
                .flex_shrink()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(format!("{}/", repo.owner))),
        )
        .child(
            div()
                .flex_none()
                .truncate()
                .text_color(cx.theme().foreground)
                .child(SharedString::from(repo.name.clone())),
        )
        .when(repo.archived, |this| {
            this.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("archived"),
            )
        })
        .into_any_element()
}

fn language_cell(repo: &Arc<Repo>, cx: &App) -> gpui::AnyElement {
    let Some(language) = repo.primary_language.clone() else {
        return div()
            .text_color(cx.theme().muted_foreground)
            .child("—")
            .into_any_element();
    };
    h_flex()
        .gap_1p5()
        .items_center()
        .child(language_dot(&language))
        .child(
            div()
                .truncate()
                .text_color(cx.theme().muted_foreground)
                .child(SharedString::from(language)),
        )
        .into_any_element()
}

/// The one place a colour bypasses the theme, because the hue is the datum.
pub fn language_dot(language: &str) -> Div {
    div()
        .flex_none()
        .size(px(8.))
        .rounded_full()
        .bg(format::language_color(language))
}

fn tags_cell(repo: &Arc<Repo>, cx: &App) -> gpui::AnyElement {
    if repo.tags.is_empty() {
        return div().into_any_element();
    }
    h_flex()
        .gap_1()
        .overflow_hidden()
        .children(repo.tags.iter().take(4).map(|tag| {
            // AI suggestions read quieter than tags the user or GitHub own.
            let muted = tag.source == TagSource::Ai;
            div()
                .flex_none()
                .px_1p5()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(if muted {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                })
                .child(SharedString::from(tag.name.clone()))
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(id: i64, full_name: &str) -> Arc<Repo> {
        let (owner, name) = full_name.split_once('/').unwrap();
        Arc::new(Repo {
            id,
            full_name: full_name.into(),
            owner: owner.into(),
            name: name.into(),
            ..Default::default()
        })
    }

    #[test]
    fn the_order_vector_is_the_row_space() {
        let mut delegate = ResultsDelegate::new();
        delegate.set_repos(Arc::new(vec![
            repo(1, "a/one"),
            repo(2, "b/two"),
            repo(3, "c/three"),
        ]));
        delegate.set_order(vec![2, 0]);

        assert_eq!(delegate.repo_at(0).unwrap().id, 3);
        assert_eq!(delegate.repo_at(1).unwrap().id, 1);
        assert!(delegate.repo_at(2).is_none());
    }

    #[test]
    fn column_widths_round_trip_and_reject_nonsense() {
        let mut delegate = ResultsDelegate::new();
        let original = delegate.column_widths();
        delegate.set_column_widths(&[320.0, 0.0, f32::NAN, 5000.0]);
        let widths = delegate.column_widths();

        assert_eq!(widths[0], 320.0, "a sane width applies");
        assert_eq!(widths[1], original[1], "zero is rejected");
        assert_eq!(widths[2], original[2], "NaN is rejected");
        assert_eq!(widths[3], original[3], "an absurd width is rejected");
    }

    #[test]
    fn a_short_persisted_layout_leaves_the_rest_alone() {
        let mut delegate = ResultsDelegate::new();
        let original = delegate.column_widths();
        delegate.set_column_widths(&[300.0]);
        assert_eq!(delegate.column_widths()[1..], original[1..]);
    }

    #[test]
    fn the_sort_indicator_is_exclusive() {
        let mut delegate = ResultsDelegate::new();
        delegate.set_sort_indicator(COL_STARS, ColumnSort::Descending);
        let sorted: Vec<_> = delegate
            .columns
            .iter()
            .filter(|c| matches!(c.sort, Some(ColumnSort::Ascending | ColumnSort::Descending)))
            .map(|c| c.key.clone())
            .collect();
        assert_eq!(sorted, [COL_STARS]);
    }
}
