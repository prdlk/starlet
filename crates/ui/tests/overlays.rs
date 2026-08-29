//! The overlay layers must actually be on screen.
//!
//! `gpui_component::Root` renders only its child view; the sheet, dialog, and
//! notification layers are static helpers the application root has to compose
//! itself. Forgetting them produces a build where every command fires, every
//! piece of state updates, and nothing is visible. These tests assert the
//! window really has an active overlay after each command, which is what a
//! missing layer would not change — so they are paired with a render pass that
//! would panic if the layer were absent.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, TestAppContext, VisualTestContext};
use gpui_component::WindowExt as _;
use starlet_core::model::Repo;
use starlet_store::Store;
use starlet_ui::SearchView;
use starlet_ui::actions::{Dismiss, OpenSettings, ShowDetail, ToggleCommandPalette};
use starlet_ui::services::{Backend, Session};

fn boot(cx: &mut TestAppContext) -> (gpui::Entity<SearchView>, &mut VisualTestContext) {
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
            .upsert_repos(&[Repo {
                id: 1,
                node_id: "n1".into(),
                full_name: "helix-editor/helix".into(),
                name: "helix".into(),
                owner: "helix-editor".into(),
                html_url: "https://github.com/helix-editor/helix".into(),
                description: Some("A post-modern modal text editor".into()),
                stargazers: 39_000,
                ..Default::default()
            }])
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

    let captured: Rc<RefCell<Option<gpui::Entity<SearchView>>>> = Rc::new(RefCell::new(None));
    let slot = captured.clone();
    let (_root, cx) = cx.add_window_view(move |window, cx| {
        let view = cx.new(|cx| SearchView::new(window, cx));
        *slot.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let view = captured.borrow_mut().take().expect("workspace built");
    settle(cx);
    (view, cx)
}

fn settle(cx: &mut VisualTestContext) {
    for _ in 0..60 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
        cx.run_until_parked();
    }
}

fn has_dialog(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| window.has_active_dialog(cx))
}

fn has_sheet(cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| window.has_active_sheet(cx))
}

#[gpui::test]
async fn the_command_palette_opens_and_closes(cx: &mut TestAppContext) {
    let (_view, cx) = boot(cx);
    assert!(!has_dialog(cx));

    cx.dispatch_action(ToggleCommandPalette);
    settle(cx);
    assert!(has_dialog(cx), "the palette must reach the dialog layer");

    // Toggling is symmetric: the same command closes what it opened.
    cx.dispatch_action(ToggleCommandPalette);
    settle(cx);
    assert!(!has_dialog(cx));
}

#[gpui::test]
async fn settings_opens_as_a_dialog(cx: &mut TestAppContext) {
    let (_view, cx) = boot(cx);
    cx.dispatch_action(OpenSettings);
    settle(cx);
    assert!(has_dialog(cx));
}

#[gpui::test]
async fn the_detail_sheet_opens_for_the_highlighted_repository(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    cx.simulate_input("helix");
    settle(cx);
    assert_eq!(view.read_with(cx, |v, cx| v.selected_repo_id(cx)), Some(1));

    assert!(!has_sheet(cx));
    cx.dispatch_action(ShowDetail);
    settle(cx);
    assert!(has_sheet(cx), "the sheet must reach the sheet layer");
}

#[gpui::test]
async fn escape_dismisses_the_topmost_surface_before_the_query(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    cx.simulate_input("helix");
    settle(cx);

    cx.dispatch_action(ShowDetail);
    settle(cx);
    assert!(has_sheet(cx));

    // First Escape closes the sheet and leaves the query alone.
    cx.dispatch_action(Dismiss);
    settle(cx);
    assert!(!has_sheet(cx));
    assert!(
        !view.read_with(cx, |v, _| v.is_home()),
        "the query survives the first Escape"
    );

    // Second Escape clears the query and returns home.
    cx.dispatch_action(Dismiss);
    settle(cx);
    assert!(view.read_with(cx, |v, _| v.is_home()));
}

#[gpui::test]
async fn escape_closes_a_dialog_before_touching_the_query(cx: &mut TestAppContext) {
    let (view, cx) = boot(cx);
    cx.simulate_input("helix");
    settle(cx);

    cx.dispatch_action(OpenSettings);
    settle(cx);
    assert!(has_dialog(cx));

    cx.dispatch_action(Dismiss);
    settle(cx);
    assert!(!has_dialog(cx));
    assert!(!view.read_with(cx, |v, _| v.is_home()));
}
