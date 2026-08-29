//! Starlet's interface.
//!
//! The crate is organised by capability rather than by implementation role:
//! `search` owns the workspace, `results` the table, `detail` the sheet, and so
//! on. `services` holds the only two globals — the I/O backend and the sign-in
//! session — and everything else is a GPUI entity owned by the view that needs
//! it.

pub mod actions;
pub mod analyze;
pub mod assets;
pub mod detail;
pub mod filters;
pub mod format;
pub mod palette;
pub mod results;
pub mod search;
pub mod services;
pub mod settings;
pub mod signin;
pub mod theme;

pub use assets::Assets;
pub use search::SearchView;
pub use services::{AuthStatus, Backend, Session};
pub use theme::Appearance;

use gpui::{App, AppContext as _, Window};

/// Register everything the interface needs, in dependency order.
///
/// `gpui_component::init` must already have run: it creates the theme global
/// this replaces the palette on, and the key contexts these bindings extend.
pub fn init(appearance: Appearance, cx: &mut App) {
    let fonts_loaded = match assets::load_fonts(cx) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("bundled fonts unavailable, falling back to the system UI font: {err}");
            false
        }
    };
    theme::install(appearance, fonts_loaded, cx);
    actions::bind_keys(cx);
}

/// Build the workspace view for a window.
pub fn workspace(window: &mut Window, cx: &mut App) -> gpui::Entity<SearchView> {
    cx.new(|cx| SearchView::new(window, cx))
}
