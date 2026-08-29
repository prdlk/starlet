//! The workspace: search input, results table, and everything hung off them.
//!
//! # Layout
//!
//! There is one screen with two states. With an empty query the input sits in
//! the vertical centre of an otherwise empty canvas; as soon as there is a
//! query the input rises to the top and the table takes the remaining space.
//! The transition is a 120 ms ease-out on a single flexible spacer, so the
//! input moves continuously instead of jumping between two layouts.
//!
//! # Search
//!
//! Ranking runs in two stages. Stage one is synchronous on the keystroke:
//! parse, filter, fuzzy-rank, render. Stage two asks SQLite for BM25 relevance
//! and re-ranks when it answers, guarded by a revision counter so a slow
//! answer can never overwrite a newer query. Neither stage touches the network.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, AppContext as _, ClipboardItem, Context, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Task, Window, div, ease_out_quint,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Root, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    table::{ColumnSort, Table, TableDelegate as _, TableEvent, TableState},
    v_flex,
};
use starlet_core::model::Repo;
use starlet_core::query::{self, Query, SortKey};
use starlet_core::rank::Ranker;
use starlet_store::{KEY_COLUMN_WIDTHS, Store};
use starlet_sync::{SyncEngine, SyncEvent, SyncMode, SyncPhase};

use crate::actions::{
    self, Analyze, CopyUrl, CycleFocus, Dismiss, FocusSearch, OpenInBrowser, OpenSettings,
    SelectFirst, SelectLast, SelectNext, SelectPrev, ShowDetail, SignOut, SyncNow,
    ToggleCommandPalette, ToggleSidebar,
};
use crate::detail::DetailSheet;
use crate::filters::{FacetFilters, FilterPanel};
use crate::palette::CommandPalette;
use crate::results::{COL_LAST_COMMIT, COL_NAME, COL_STARS, ResultsDelegate, SortRequest};
use crate::services::{Backend, Session};
use crate::settings::SettingsDialog;
use crate::signin::SignInFlow;

/// How long the input takes to travel between the centred and docked layouts.
const LAYOUT_SHIFT: Duration = Duration::from_millis(120);
/// Background refresh cadence while the window is focused.
const SYNC_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Upper bound on FTS rows pulled back per keystroke. Beyond this the tail
/// cannot affect the visible ranking.
const FTS_LIMIT: i64 = 2_000;

/// What the status line is currently saying.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncStatus {
    Idle,
    Running {
        phase: SyncPhase,
        done: usize,
        total: Option<usize>,
    },
    Failed(String),
}

pub struct SearchView {
    focus_handle: FocusHandle,
    input: Entity<InputState>,
    table: Entity<TableState<ResultsDelegate>>,
    filters: Entity<FilterPanel>,

    /// Every repository, newest snapshot. Shared with the table delegate.
    repos: Vec<Arc<Repo>>,
    ranker: Ranker,
    query: Query,
    /// Incremented on every query change; stale async results are discarded by
    /// comparing against it.
    revision: u64,
    /// BM25 relevance for the current query, once SQLite has answered.
    fts: HashMap<i64, f32>,
    /// Sort chosen by clicking a column header. A `sort:` prefix in the query
    /// takes precedence over it.
    header_sort: Option<SortKey>,

    sidebar_open: bool,
    /// `flex_grow` the layout spacer animates away from, so a transition
    /// interrupted halfway still starts where the eye last saw it.
    hero_grow_from: f32,
    /// Bumped on every layout-state change so the animation replays.
    layout_generation: usize,

    sync: SyncStatus,
    /// Held so the work is cancelled when the view goes away.
    _sync_task: Option<Task<()>>,
    _schedule_task: Option<Task<()>>,
    _fts_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SearchView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search your stars")
                .clean_on_escape()
        });
        let table = cx.new(|cx| {
            TableState::new(ResultsDelegate::new(), window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .col_movable(false)
                .loop_selection(false)
        });
        let filters = cx.new(FilterPanel::new);

        let mut subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input_event),
            cx.subscribe_in(&table, window, Self::on_table_event),
            cx.subscribe_in(&filters, window, Self::on_filter_event),
        ];
        // The avatar and the empty state both depend on sign-in state.
        subscriptions.push(cx.observe_global::<Session>(|_, cx| cx.notify()));

        let mut this = Self {
            focus_handle: cx.focus_handle(),
            input,
            table,
            filters,
            repos: Vec::new(),
            ranker: Ranker::new(),
            query: Query::default(),
            revision: 0,
            fts: HashMap::new(),
            header_sort: None,
            sidebar_open: false,
            hero_grow_from: 1.0,
            layout_generation: 0,
            sync: SyncStatus::Idle,
            _sync_task: None,
            _schedule_task: None,
            _fts_task: None,
            _subscriptions: subscriptions,
        };

        this.load_from_store(cx);
        this.restore_column_widths(cx);
        this.schedule_background_sync(window, cx);
        // Focus has to be requested after the window exists, not while its
        // root view is still being constructed: a focus set during `new` is
        // discarded when the window installs its initial focus.
        cx.defer_in(window, |this, window, cx| {
            this.input.update(cx, |state, cx| state.focus(window, cx));
        });
        this
    }

    // ---------------------------------------------------------------- data

    /// Read the whole mirror into memory. 5 000 repositories is a few
    /// megabytes, and holding them is what makes search allocation-free.
    fn load_from_store(&mut self, cx: &mut Context<Self>) {
        let store = Backend::global(cx).store();
        let rx = Backend::global(cx).spawn(async move { store.load_repos().await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(repos)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.replace_repos(repos, cx);
            });
        })
        .detach();
    }

    fn replace_repos(&mut self, repos: Vec<Repo>, cx: &mut Context<Self>) {
        self.repos = repos.into_iter().map(Arc::new).collect();
        self.publish_snapshot(cx);
        self.rerank(Selection::Keep, cx);
        self.filters.update(cx, |panel, cx| panel.reload(cx));
    }

    /// Merge rows a sync produced, then re-rank. Existing repositories are
    /// replaced in place so the snapshot keeps a stable order.
    fn merge_repos(&mut self, updated: Vec<Repo>, cx: &mut Context<Self>) {
        let mut by_id: HashMap<i64, usize> = self
            .repos
            .iter()
            .enumerate()
            .map(|(ix, r)| (r.id, ix))
            .collect();
        for repo in updated {
            match by_id.get(&repo.id) {
                Some(&ix) => self.repos[ix] = Arc::new(repo),
                None => {
                    by_id.insert(repo.id, self.repos.len());
                    self.repos.push(Arc::new(repo));
                }
            }
        }
        self.publish_snapshot(cx);
        self.rerank(Selection::Keep, cx);
    }

    fn remove_repos(&mut self, ids: &[i64], cx: &mut Context<Self>) {
        self.repos.retain(|r| !ids.contains(&r.id));
        self.publish_snapshot(cx);
        self.rerank(Selection::Keep, cx);
    }

    /// Hand the table a fresh snapshot. Cloning a `Vec<Arc<Repo>>` copies
    /// pointers, not repositories.
    fn publish_snapshot(&mut self, cx: &mut Context<Self>) {
        let snapshot = Arc::new(self.repos.clone());
        self.table.update(cx, |state, _| {
            state.delegate_mut().set_repos(snapshot);
        });
    }

    // -------------------------------------------------------------- search

    fn on_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => self.on_query_changed(cx),
            // Enter belongs to the input while it has focus, so the "open the
            // highlighted repository" command arrives as an input event rather
            // than a key binding.
            InputEvent::PressEnter { .. } => self.open_selected(window, cx),
            _ => {}
        }
    }

    fn on_query_changed(&mut self, cx: &mut Context<Self>) {
        let raw = self.input.read(cx).value();
        let was_home = self.is_home();
        self.query = query::parse(&raw);
        self.revision = self.revision.wrapping_add(1);
        self.fts.clear();

        if was_home != self.is_home() {
            self.hero_grow_from = if was_home { 1.0 } else { 0.0 };
            self.layout_generation += 1;
        }

        self.rerank(Selection::Reset, cx);
        self.request_fts(cx);
    }

    /// Stage two: BM25 from SQLite, applied only if the query has not moved on.
    fn request_fts(&mut self, cx: &mut Context<Self>) {
        if !self.query.has_text() {
            self._fts_task = None;
            return;
        }
        let terms = self.query.terms.clone();
        let revision = self.revision;
        let store: Store = Backend::global(cx).store();
        let rx =
            Backend::global(cx).spawn(async move { store.search_fts(&terms, FTS_LIMIT).await });

        self._fts_task = Some(cx.spawn(async move |this, cx| {
            let Ok(Ok(hits)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                this.fts = hits;
                this.rerank(Selection::Keep, cx);
            });
        }));
    }
}

/// What a re-rank should do with the current highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    /// The query changed: the old highlight is meaningless, start at the top.
    Reset,
    /// The data or the ranking refined under a stable query: follow the same
    /// repository so the highlight does not jump while the user reads.
    Keep,
}

impl SearchView {
    /// Filter, rank, and hand the resulting order to the table.
    fn rerank(&mut self, selection: Selection, cx: &mut Context<Self>) {
        let facets = self.filters.read(cx).active().clone();
        let mut candidate_ix = Vec::with_capacity(self.repos.len());
        let mut candidates = Vec::with_capacity(self.repos.len());
        for (ix, repo) in self.repos.iter().enumerate() {
            if self.query.matches(repo) && facets.matches(repo) {
                candidate_ix.push(ix);
                candidates.push(repo.clone());
            }
        }

        let mut query = self.query.clone();
        // A `sort:` prefix is an explicit instruction and outranks the header.
        if query.sort.is_none() {
            query.sort = self.header_sort;
        }
        let scored = self.ranker.rank(&query, &candidates, &self.fts);
        let order: Vec<usize> = scored.iter().map(|s| candidate_ix[s.ix]).collect();

        let previous = match selection {
            Selection::Keep => self.selected_repo_id(cx),
            Selection::Reset => None,
        };
        self.table.update(cx, |state, cx| {
            state.delegate_mut().set_order(order);
            state.refresh(cx);
        });
        self.restore_selection(previous, cx);
        cx.notify();
    }

    /// Keep the highlight on the same repository across a re-rank; fall back to
    /// the first row so Enter always has a target.
    fn restore_selection(&mut self, previous: Option<i64>, cx: &mut Context<Self>) {
        self.table.update(cx, |state, cx| {
            let rows = state.delegate().rows_count(cx);
            if rows == 0 {
                state.clear_selection(cx);
                return;
            }
            let row = previous
                .and_then(|id| {
                    (0..rows).find(|ix| state.delegate().repo_at(*ix).is_some_and(|r| r.id == id))
                })
                .unwrap_or(0);
            state.set_selected_row(row, cx);
        });
    }

    /// The highlighted repository's id, if any.
    pub fn selected_repo_id(&self, cx: &App) -> Option<i64> {
        let state = self.table.read(cx);
        let row = state.selected_row()?;
        state.delegate().repo_at(row).map(|r| r.id)
    }

    fn selected_repo(&self, cx: &App) -> Option<Arc<Repo>> {
        let state = self.table.read(cx);
        let row = state.selected_row()?;
        state.delegate().repo_at(row).cloned()
    }

    /// True while the query is empty and the input sits centred.
    pub fn is_home(&self) -> bool {
        self.query.is_empty()
    }

    // ------------------------------------------------------------- commands

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.table.update(cx, |state, cx| {
            let rows = state.delegate().rows_count(cx);
            if rows == 0 {
                return;
            }
            let current = state.selected_row().unwrap_or(0) as isize;
            let next = (current + delta).clamp(0, rows as isize - 1) as usize;
            state.set_selected_row(next, cx);
            state.scroll_to_row(next, cx);
        });
    }

    fn select_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        self.table.update(cx, |state, cx| {
            let rows = state.delegate().rows_count(cx);
            if rows == 0 {
                return;
            }
            let row = if last { rows - 1 } else { 0 };
            state.set_selected_row(row, cx);
            state.scroll_to_row(row, cx);
        });
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.selected_repo(cx) else {
            return;
        };
        let url = repo.html_url.clone();
        // Opening a browser can block for a noticeable moment on Linux.
        Backend::global(cx).spawn_blocking(move || {
            if let Err(err) = open::that_detached(&url) {
                tracing::warn!("could not open {url}: {err}");
            }
        });
        let _ = window;
    }

    fn copy_url(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.selected_repo(cx) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(repo.html_url.clone()));
    }

    fn show_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.selected_repo(cx) else {
            return;
        };
        DetailSheet::open(repo, window, cx);
    }

    // ----------------------------------------------------------------- sync

    /// Run once at launch and every 15 minutes while the window is focused.
    ///
    /// The cadence is checked rather than driven by focus events so a window
    /// that regains focus after hours does not queue up a burst of syncs.
    fn schedule_background_sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if Session::is_signed_in(cx) {
            self.start_sync(None, window, cx);
        }
        self._schedule_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SYNC_INTERVAL).await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if matches!(this.sync, SyncStatus::Running { .. }) {
                            return;
                        }
                        if Session::is_signed_in(cx) {
                            this.start_sync_headless(cx);
                        }
                    })
                    .is_ok();
                if !keep_going {
                    break;
                }
            }
        }));
    }

    fn start_sync(&mut self, mode: Option<SyncMode>, _: &mut Window, cx: &mut Context<Self>) {
        self.start_sync_inner(mode, cx);
    }

    fn start_sync_headless(&mut self, cx: &mut Context<Self>) {
        self.start_sync_inner(None, cx);
    }

    /// Kick off a sync and stream its events into the view.
    fn start_sync_inner(&mut self, mode: Option<SyncMode>, cx: &mut Context<Self>) {
        let Some(github) = Session::client(cx) else {
            self.sync = SyncStatus::Failed("Sign in to sync your stars".into());
            cx.notify();
            return;
        };
        if matches!(self.sync, SyncStatus::Running { .. }) {
            return;
        }

        let store = Backend::global(cx).store();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let store_for_mode = store.clone();
        Backend::global(cx).spawn(async move {
            let mode = match mode {
                Some(mode) => mode,
                None if SyncEngine::needs_full_sync(&store_for_mode).await => SyncMode::Full,
                None => SyncMode::Incremental,
            };
            let engine = SyncEngine::new(github, store);
            let _ = engine.run(mode, &tx).await;
        });

        self.sync = SyncStatus::Running {
            phase: SyncPhase::Stars,
            done: 0,
            total: None,
        };
        cx.notify();

        self._sync_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let alive = this
                    .update(cx, |this, cx| this.apply_sync_event(event, cx))
                    .is_ok();
                if !alive {
                    break;
                }
            }
        }));
    }

    fn apply_sync_event(&mut self, event: SyncEvent, cx: &mut Context<Self>) {
        match event {
            SyncEvent::Started(_) => {
                self.sync = SyncStatus::Running {
                    phase: SyncPhase::Stars,
                    done: 0,
                    total: None,
                };
                self.table.update(cx, |state, _| {
                    state.delegate_mut().set_loading(self.repos.is_empty());
                });
            }
            SyncEvent::Progress { phase, done, total } => {
                self.sync = SyncStatus::Running { phase, done, total };
            }
            SyncEvent::Upserted(repos) => {
                self.merge_repos(repos, cx);
                self.table
                    .update(cx, |state, _| state.delegate_mut().set_loading(false));
            }
            SyncEvent::Removed(ids) => self.remove_repos(&ids, cx),
            SyncEvent::Finished(_) => {
                self.sync = SyncStatus::Idle;
                self.table
                    .update(cx, |state, _| state.delegate_mut().set_loading(false));
                self.filters.update(cx, |panel, cx| panel.reload(cx));
            }
            SyncEvent::Failed(message) => {
                self.sync = SyncStatus::Failed(message);
                self.table
                    .update(cx, |state, _| state.delegate_mut().set_loading(false));
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------ persistence

    fn restore_column_widths(&mut self, cx: &mut Context<Self>) {
        let store = Backend::global(cx).store();
        let rx = Backend::global(cx).spawn(async move { store.get_state(KEY_COLUMN_WIDTHS).await });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(raw))) = rx.await else { return };
            let Ok(widths) = serde_json::from_str::<Vec<f32>>(&raw) else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.table.update(cx, |state, cx| {
                    state.delegate_mut().set_column_widths(&widths);
                    state.refresh(cx);
                });
            });
        })
        .detach();
    }

    fn persist_column_widths(&self, cx: &mut Context<Self>) {
        let widths = self.table.read(cx).delegate().column_widths();
        let Ok(json) = serde_json::to_string(&widths) else {
            return;
        };
        let store = Backend::global(cx).store();
        Backend::global(cx).spawn(async move {
            let _ = store.set_state(KEY_COLUMN_WIDTHS, &json).await;
        });
    }

    // ---------------------------------------------------------------- events

    fn on_table_event(
        &mut self,
        _: &Entity<TableState<ResultsDelegate>>,
        event: &TableEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TableEvent::DoubleClickedRow(_) => self.open_selected(window, cx),
            TableEvent::ColumnWidthsChanged(_) => self.persist_column_widths(cx),
            TableEvent::SelectRow(_) => cx.notify(),
            _ => {}
        }
        if let Some(request) = self
            .table
            .update(cx, |state, _| state.delegate_mut().take_sort_request())
        {
            self.apply_sort_request(request, cx);
        }
    }

    fn apply_sort_request(&mut self, request: SortRequest, cx: &mut Context<Self>) {
        self.header_sort = match (request.column, request.sort) {
            (_, ColumnSort::Default) => None,
            (COL_NAME, _) => Some(SortKey::Name),
            (COL_STARS, _) => Some(SortKey::Stars),
            (COL_LAST_COMMIT, _) => Some(SortKey::Recent),
            _ => None,
        };
        self.rerank(Selection::Reset, cx);
    }

    fn on_filter_event(
        &mut self,
        _: &Entity<FilterPanel>,
        _: &FacetFilters,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rerank(Selection::Reset, cx);
    }

    // --------------------------------------------------------------- actions

    /// Tab moves between the query and the results.
    ///
    /// Focus is what decides whether Space types a character or opens the
    /// detail sheet, so this is the only way to reach that command from the
    /// keyboard.
    fn cycle_focus(&mut self, _: &CycleFocus, window: &mut Window, cx: &mut Context<Self>) {
        let table = self.table.focus_handle(cx);
        let input = self.input.focus_handle(cx);
        if table.contains_focused(window, cx) {
            window.focus(&input);
        } else if self.result_count(cx) > 0 {
            window.focus(&table);
        }
    }

    /// True while the results table owns the keyboard.
    pub fn is_table_focused(&self, window: &Window, cx: &App) -> bool {
        self.table.focus_handle(cx).contains_focused(window, cx)
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_sheet(cx) {
            window.close_sheet(cx);
            return;
        }
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
            return;
        }
        if !self.input.read(cx).value().is_empty() {
            self.input
                .update(cx, |state, cx| state.set_value("", window, cx));
            return;
        }
        self.input.update(cx, |state, cx| state.focus(window, cx));
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    fn toggle_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        CommandPalette::toggle(cx.entity(), window, cx);
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        SettingsDialog::open(window, cx);
    }

    fn sync_now(&mut self, _: &SyncNow, window: &mut Window, cx: &mut Context<Self>) {
        if Session::is_signed_in(cx) {
            self.start_sync(None, window, cx);
        } else {
            SignInFlow::start(window, cx);
        }
    }

    fn analyze(&mut self, _: &Analyze, window: &mut Window, cx: &mut Context<Self>) {
        crate::analyze::AnalyzeDialog::open(cx.entity(), window, cx);
    }

    fn sign_out(&mut self, _: &SignOut, _: &mut Window, cx: &mut Context<Self>) {
        SignInFlow::sign_out(cx);
    }

    /// Replace the query text from a command, keeping the caret at the end.
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = SharedString::from(text.to_owned());
        self.input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.focus(window, cx);
        });
    }

    /// Drop every sidebar facet selection.
    pub fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.filters.update(cx, |panel, cx| panel.clear(cx));
        self.rerank(Selection::Reset, cx);
    }

    pub fn current_query(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    /// Whether the filter sidebar is showing.
    pub fn is_sidebar_open(&self) -> bool {
        self.sidebar_open
    }

    /// How many rows the table is currently showing.
    pub fn result_count(&self, cx: &App) -> usize {
        self.table.read(cx).delegate().rows_count(cx)
    }

    /// Every repository currently in memory. The analysis dialog needs them.
    pub fn repos(&self) -> &[Arc<Repo>] {
        &self.repos
    }

    pub fn refresh_after_tag_change(&mut self, cx: &mut Context<Self>) {
        self.load_from_store(cx);
    }

    // ---------------------------------------------------------------- render

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let signed_in = Session::is_signed_in(cx);
        let login = Session::global(cx)
            .viewer
            .as_ref()
            .map(|v| SharedString::from(v.login.clone()));

        h_flex()
            .h_10()
            .w_full()
            .flex_none()
            .px_4()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("toggle-sidebar")
                            .ghost()
                            .xsmall()
                            .icon(if self.sidebar_open {
                                IconName::PanelLeftClose
                            } else {
                                IconName::PanelLeftOpen
                            })
                            .tooltip("Filters")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_sidebar(&ToggleSidebar, window, cx)
                            })),
                    )
                    .child(self.render_sync_status(cx)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("command-palette")
                            .ghost()
                            .xsmall()
                            .label(actions::command_palette_shortcut())
                            .tooltip("Commands")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_palette(&ToggleCommandPalette, window, cx)
                            })),
                    )
                    .child(
                        Button::new("settings")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Settings)
                            .tooltip("Settings")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings(&OpenSettings, window, cx)
                            })),
                    )
                    .when(signed_in, |this| {
                        this.child(
                            Button::new("account")
                                .ghost()
                                .xsmall()
                                .icon(IconName::CircleUser)
                                .when_some(login, |b, login| b.label(login))
                                .tooltip("Sign out")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.sign_out(&SignOut, window, cx)
                                })),
                        )
                    })
                    .when(!signed_in, |this| {
                        this.child(
                            Button::new("sign-in")
                                .primary()
                                .xsmall()
                                .label("Sign in")
                                .on_click(|_, window, cx| SignInFlow::start(window, cx)),
                        )
                    }),
            )
    }

    fn render_sync_status(&self, cx: &App) -> impl IntoElement {
        let (text, muted) = match &self.sync {
            SyncStatus::Idle => (
                SharedString::from(format!("{} repositories", self.repos.len())),
                true,
            ),
            SyncStatus::Running { phase, done, total } => {
                let progress = match total {
                    Some(total) => format!("{done}/{total}"),
                    None => format!("{done}"),
                };
                (
                    SharedString::from(format!("{} {progress}", phase.label())),
                    true,
                )
            }
            SyncStatus::Failed(message) => (SharedString::from(message.clone()), false),
        };
        div()
            .text_xs()
            .text_color(if muted {
                cx.theme().muted_foreground
            } else {
                cx.theme().danger
            })
            .child(text)
    }

    fn render_search_field(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let home = self.is_home();
        div()
            .w_full()
            .when(home, |this| this.max_w(px(560.)))
            .child(
                Input::new(&self.input)
                    .when(home, |input| input.large())
                    .prefix(
                        div()
                            .pl_2()
                            .text_color(cx.theme().muted_foreground)
                            .child(Icon::new(IconName::Search).small()),
                    )
                    .cleanable(true),
            )
    }

    fn render_home_hint(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child("Type to search. Try lang:rust, stars:>1000, tag:cli")
            .child(SharedString::from(format!(
                "{} repositories indexed",
                self.repos.len()
            )))
    }

    fn render_results(&self) -> impl IntoElement {
        h_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .gap_0()
            .when(self.sidebar_open, |this| this.child(self.filters.clone()))
            .child(
                div()
                    // Space and Enter mean something different here than they
                    // do in the query field, so the table owns its own context.
                    .key_context(actions::RESULTS_CONTEXT)
                    .flex_1()
                    .min_w_0()
                    .size_full()
                    .child(
                        Table::new(&self.table)
                            .small()
                            .bordered(false)
                            .stripe(false),
                    ),
            )
    }
}

impl Focusable for SearchView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Emitted when a palette or dialog wants the workspace to reload its data.
pub struct RepoDataChanged;

impl EventEmitter<RepoDataChanged> for SearchView {}

impl Render for SearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let home = self.is_home();
        let target_grow = if home { 1.0 } else { 0.0 };
        let from = self.hero_grow_from;
        let generation = self.layout_generation;

        v_flex()
            .id("starlet")
            .key_context(actions::CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::cycle_focus))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::sync_now))
            .on_action(cx.listener(Self::analyze))
            .on_action(cx.listener(Self::sign_out))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select_edge(false, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select_edge(true, cx)))
            .on_action(
                cx.listener(|this, _: &OpenInBrowser, window, cx| this.open_selected(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowDetail, window, cx| this.show_detail(window, cx)))
            .on_action(cx.listener(|this, _: &CopyUrl, _, cx| this.copy_url(cx)))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .text_sm()
            .child(self.render_toolbar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_center()
                    .px_4()
                    // The spacer above the input is the whole layout shift: it
                    // owns all the free vertical space at home and none of it
                    // once results are showing.
                    .child(div().w_full().flex_shrink().with_animation(
                        ("layout-shift", generation),
                        Animation::new(LAYOUT_SHIFT).with_easing(ease_out_quint()),
                        move |mut el, delta| {
                            // `Styled::flex_grow` is a fixed 1.0; the
                            // refinement is the only way to set a
                            // fractional grow, which is what makes the
                            // input travel instead of jump.
                            el.style().flex_grow = Some(from + (target_grow - from) * delta);
                            el
                        },
                    ))
                    .child(
                        v_flex()
                            .w_full()
                            .flex_none()
                            .items_center()
                            .gap_3()
                            .py_3()
                            .child(self.render_search_field(cx))
                            .when(home, |this| this.child(self.render_home_hint(cx))),
                    )
                    .when(!home, |this| this.child(self.render_results()))
                    .when(home, |this| {
                        this.child(div().w_full().flex_grow().flex_shrink())
                    }),
            )
            // `Root` renders only its child view; the overlay layers are static
            // helpers the application root has to compose itself. Order is
            // stacking order: a dialog sits above a sheet, and a notification
            // above both.
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}
