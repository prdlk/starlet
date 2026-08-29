//! Interaction tests for the workspace.
//!
//! These drive the real view through a GPUI test window: the layout shift, the
//! keyboard path, and the Escape cascade are behaviour, not appearance, and a
//! screenshot cannot prove any of them.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, TestAppContext, VisualTestContext};
use starlet_core::model::Repo;
use starlet_store::Store;
use starlet_ui::SearchView;
use starlet_ui::actions::{Dismiss, SelectFirst, SelectLast, SelectNext, SelectPrev};
use starlet_ui::services::{Backend, Session};

fn repo(id: i64, full_name: &str, description: &str, stars: i64) -> Repo {
    let (owner, name) = full_name.split_once('/').unwrap();
    Repo {
        id,
        node_id: format!("n{id}"),
        full_name: full_name.into(),
        name: name.into(),
        owner: owner.into(),
        html_url: format!("https://github.com/{full_name}"),
        description: Some(description.into()),
        stargazers: stars,
        primary_language: Some("Rust".into()),
        starred_at: chrono::Utc::now().checked_sub_signed(chrono::Duration::days(id)),
        ..Default::default()
    }
}

/// Install the globals a workspace needs and seed a small corpus.
///
/// Leaves the signed-out welcome screen up, which is what a real launch does.
fn boot_signed_out(cx: &mut TestAppContext) -> (gpui::Entity<SearchView>, &mut VisualTestContext) {
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("io runtime"),
    );
    let store = runtime.block_on(async {
        let store = Store::open_in_memory().await.expect("open");
        store
            .upsert_repos(&[
                repo(
                    1,
                    "helix-editor/helix",
                    "A post-modern modal text editor",
                    39_000,
                ),
                repo(2, "sharkdp/bat", "A cat clone with wings", 48_000),
                repo(
                    3,
                    "BurntSushi/ripgrep",
                    "Recursively search directories",
                    47_000,
                ),
                repo(
                    4,
                    "zed-industries/zed",
                    "A high-performance code editor",
                    50_000,
                ),
            ])
            .await
            .expect("seed");
        store
    });

    cx.update(|cx| {
        gpui_component::init(cx);
        starlet_ui::init(starlet_ui::Appearance::Dark, cx);
        cx.set_global(Backend::new(store, runtime));
        cx.set_global(Session::signed_out());
    });

    // `gpui-component` requires `Root` at the first level of every window; its
    // overlay, dialog, and sheet layers all resolve through it.
    let captured: Rc<RefCell<Option<gpui::Entity<SearchView>>>> = Rc::new(RefCell::new(None));
    let slot = captured.clone();
    let (_root, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SearchView::new(window, cx));
        *slot.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().expect("workspace built");

    // The initial load runs on the I/O runtime; wait for it to land.
    settle(cx);
    (view, cx)
}

/// The same, then past the welcome screen onto the search field.
///
/// Most tests are about searching, not about signing in, so they start here.
fn boot(cx: &mut TestAppContext) -> (gpui::Entity<SearchView>, &mut VisualTestContext) {
    let (view, cx) = boot_signed_out(cx);
    cx.update(|window, cx| view.update(cx, |view, cx| view.dismiss_welcome(window, cx)));
    settle(cx);
    (view, cx)
}

/// Drain both executors. GPUI's test executor knows nothing about the Tokio
/// runtime the store runs on, so parking alone is not enough.
fn settle(cx: &mut VisualTestContext) {
    for _ in 0..100 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
        cx.run_until_parked();
    }
}

/// Park until `predicate` holds, or fail with `what`.
fn wait_for(
    cx: &mut VisualTestContext,
    what: &str,
    mut predicate: impl FnMut(&mut VisualTestContext) -> bool,
) {
    for _ in 0..200 {
        cx.run_until_parked();
        if predicate(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

#[gpui::test]
async fn the_workspace_opens_at_home_with_every_repository_loaded(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    view.read_with(cx, |view, cx| {
        assert!(view.is_home(), "an empty query means the centred layout");
        assert_eq!(view.repos().len(), 4);
        // Home still ranks everything: the table exists, it is simply not shown.
        assert_eq!(view.result_count(cx), 4);
    });
}

#[gpui::test]
async fn a_signed_out_launch_opens_on_the_sign_in_screen(cx: &mut TestAppContext) {
    let (view, cx) = boot_signed_out(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    view.read_with(cx, |view, cx| {
        assert!(
            view.shows_welcome(cx),
            "signing in is the first decision, not searching"
        );
        assert!(view.is_home());
    });
}

#[gpui::test]
async fn the_sign_in_screen_can_be_dismissed_to_search_the_local_mirror(cx: &mut TestAppContext) {
    let (view, cx) = boot_signed_out(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.update(|window, cx| view.update(cx, |view, cx| view.dismiss_welcome(window, cx)));
    settle(cx);
    assert!(!view.read_with(cx, |v, cx| v.shows_welcome(cx)));

    // The query field is live again.
    cx.simulate_input("helix");
    settle(cx);
    view.read_with(cx, |view, cx| {
        assert!(!view.is_home());
        assert_eq!(view.result_count(cx), 1);
    });
}

#[gpui::test]
async fn signing_in_replaces_the_welcome_screen(cx: &mut TestAppContext) {
    let (view, cx) = boot_signed_out(cx);
    assert!(view.read_with(cx, |v, cx| v.shows_welcome(cx)));

    cx.update(|_, cx| {
        cx.set_global(Session {
            status: starlet_ui::AuthStatus::SignedIn,
            github: None,
            viewer: None,
        })
    });
    settle(cx);
    assert!(!view.read_with(cx, |v, cx| v.shows_welcome(cx)));
}

#[gpui::test]
async fn signing_out_brings_the_welcome_screen_back(cx: &mut TestAppContext) {
    let (view, cx) = boot_signed_out(cx);
    cx.update(|window, cx| view.update(cx, |view, cx| view.dismiss_welcome(window, cx)));
    settle(cx);
    assert!(!view.read_with(cx, |v, cx| v.shows_welcome(cx)));

    // A dismissal is for one session, not forever: after a sign-out the user
    // is back at the same decision they started with.
    cx.update(|_, cx| cx.set_global(Session::signed_out()));
    settle(cx);
    assert!(view.read_with(cx, |v, cx| v.shows_welcome(cx)));
}

#[gpui::test]
async fn typing_shifts_the_layout_and_narrows_the_results(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("helix");
    settle(cx);

    view.read_with(cx, |view, cx| {
        assert!(
            !view.is_home(),
            "a query docks the input and shows the table"
        );
        assert_eq!(view.result_count(cx), 1);
        assert_eq!(view.selected_repo_id(cx), Some(1));
    });
}

#[gpui::test]
async fn a_prefix_filters_without_matching_as_text(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("stars:>47500");
    settle(cx);

    view.read_with(cx, |view, cx| {
        // bat (48k) and zed (50k), not ripgrep (47k) or helix (39k).
        assert_eq!(view.result_count(cx), 2);
    });
}

#[gpui::test]
async fn a_description_only_match_is_found_through_the_full_text_index(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    // "wings" appears only in bat's description, nowhere in any name.
    cx.simulate_input("wings");
    wait_for(cx, "the FTS stage", |cx| {
        view.read_with(cx, |v, cx| v.result_count(cx) == 1)
    });

    view.read_with(cx, |view, cx| {
        assert_eq!(view.selected_repo_id(cx), Some(2));
    });
}

#[gpui::test]
async fn the_arrow_keys_move_the_highlight_and_stop_at_the_ends(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("e");
    settle(cx);
    let rows = view.read_with(cx, |view, cx| view.result_count(cx));
    assert!(rows >= 3, "expected several matches for 'e', got {rows}");

    let first = view.read_with(cx, |view, cx| view.selected_repo_id(cx));
    cx.dispatch_action(SelectNext);
    settle(cx);
    let second = view.read_with(cx, |view, cx| view.selected_repo_id(cx));
    assert_ne!(first, second, "SelectNext must move the highlight");

    cx.dispatch_action(SelectPrev);
    settle(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.selected_repo_id(cx)),
        first
    );

    // The selection clamps rather than wrapping: at the top, Prev is a no-op.
    cx.dispatch_action(SelectPrev);
    settle(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.selected_repo_id(cx)),
        first
    );

    cx.dispatch_action(SelectLast);
    settle(cx);
    let last = view.read_with(cx, |view, cx| view.selected_repo_id(cx));
    cx.dispatch_action(SelectNext);
    settle(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.selected_repo_id(cx)),
        last,
        "SelectNext at the bottom must not wrap"
    );

    cx.dispatch_action(SelectFirst);
    settle(cx);
    assert_eq!(
        view.read_with(cx, |view, cx| view.selected_repo_id(cx)),
        first
    );
}

#[gpui::test]
async fn a_new_query_resets_the_highlight_to_the_top(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("e");
    settle(cx);
    cx.dispatch_action(SelectLast);
    settle(cx);
    let moved = view.read_with(cx, |view, cx| view.selected_repo_id(cx));

    cx.simulate_input("d");
    settle(cx);
    let after = view.read_with(cx, |view, cx| view.selected_repo_id(cx));
    assert_ne!(
        after, moved,
        "a keystroke must not carry a stale highlight down the list"
    );
    assert!(after.is_some(), "there is always a target for Enter");
}

#[gpui::test]
async fn escape_clears_the_query_and_returns_home(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("helix");
    settle(cx);
    assert!(!view.read_with(cx, |view, _| view.is_home()));

    cx.dispatch_action(Dismiss);
    settle(cx);
    assert!(
        view.read_with(cx, |view, _| view.is_home()),
        "Escape with no overlay open clears the query"
    );
}

#[gpui::test]
async fn copying_writes_the_highlighted_repository_url(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("ripgrep");
    settle(cx);
    cx.dispatch_action(starlet_ui::actions::CopyUrl);
    settle(cx);

    let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(
        clipboard.as_deref(),
        Some("https://github.com/BurntSushi/ripgrep")
    );
    let _ = view;
}

#[gpui::test]
async fn tab_hands_the_keyboard_to_the_table_and_back(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("editor");
    settle(cx);
    assert!(
        !cx.update(|window, cx| view.read(cx).is_table_focused(window, cx)),
        "typing leaves focus in the query"
    );

    cx.dispatch_action(starlet_ui::actions::CycleFocus);
    settle(cx);
    assert!(
        cx.update(|window, cx| view.read(cx).is_table_focused(window, cx)),
        "Tab moves focus to the results, which is what makes Space mean 'open the sheet'"
    );

    cx.dispatch_action(starlet_ui::actions::CycleFocus);
    settle(cx);
    assert!(
        !cx.update(|window, cx| view.read(cx).is_table_focused(window, cx)),
        "Tab again returns to the query"
    );
}

#[gpui::test]
async fn tab_does_nothing_when_there_is_nothing_to_focus(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("no-such-repository-anywhere");
    settle(cx);
    assert_eq!(view.read_with(cx, |v, cx| v.result_count(cx)), 0);

    cx.dispatch_action(starlet_ui::actions::CycleFocus);
    settle(cx);
    assert!(
        !cx.update(|window, cx| view.read(cx).is_table_focused(window, cx)),
        "focus must not move into an empty table"
    );
}

#[gpui::test]
async fn the_filters_come_into_view_with_the_results(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    assert!(
        !view.read_with(cx, |v, _| v.is_sidebar_open()),
        "an empty canvas has nothing to filter"
    );

    cx.simulate_input("helix");
    settle(cx);
    assert!(
        view.read_with(cx, |v, _| v.is_sidebar_open()),
        "filters follow the results into view without being asked for"
    );

    // Clearing the query takes them away again.
    cx.dispatch_action(Dismiss);
    settle(cx);
    assert!(!view.read_with(cx, |v, _| v.is_sidebar_open()));
}

#[gpui::test]
async fn an_explicit_toggle_outranks_the_query(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    wait_for(cx, "the store to load", |cx| {
        view.read_with(cx, |v, _| !v.repos().is_empty())
    });

    cx.simulate_input("helix");
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.is_sidebar_open()));

    // Someone who closes the panel has expressed a preference; the next
    // keystroke must not reopen it.
    cx.dispatch_action(starlet_ui::actions::ToggleSidebar);
    settle(cx);
    assert!(!view.read_with(cx, |v, _| v.is_sidebar_open()));

    cx.simulate_input("-editor");
    settle(cx);
    assert!(
        !view.read_with(cx, |v, _| v.is_sidebar_open()),
        "the query stops deciding once the user has decided"
    );
}

#[gpui::test]
async fn the_sidebar_toggles(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    cx.dispatch_action(starlet_ui::actions::ToggleSidebar);
    settle(cx);
    // The panel is only rendered when open; the flag is the observable state.
    assert!(view.read_with(cx, |view, _| view.is_sidebar_open()));
    cx.dispatch_action(starlet_ui::actions::ToggleSidebar);
    settle(cx);
    assert!(!view.read_with(cx, |view, _| view.is_sidebar_open()));
}
