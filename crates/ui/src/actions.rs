//! Typed commands and their key bindings.
//!
//! Every command is an Action so the palette, the menus, and the keyboard all
//! dispatch the same thing. Bindings are registered once at startup and are
//! scoped by key context: `Starlet` is the workspace, `StarletResults` is the
//! table, and `gpui-component` owns `Input`, `Table`, `Dialog`, and `Sheet`.

use gpui::{App, KeyBinding, actions};

/// Key context for the workspace root. Global commands bind here.
pub const CONTEXT: &str = "Starlet";
/// Key context for the results table. Selection movement binds here and in the
/// input, so the arrow keys work while typing.
pub const RESULTS_CONTEXT: &str = "StarletResults";

actions!(
    starlet,
    [
        /// Move keyboard focus to the search input and select its contents.
        FocusSearch,
        /// Escape's cascade: close the sheet, then clear the query, then home.
        Dismiss,
        SelectNext,
        SelectPrev,
        SelectFirst,
        SelectLast,
        /// Open the highlighted repository in the browser.
        OpenInBrowser,
        /// Open the detail sheet for the highlighted repository.
        ShowDetail,
        CopyUrl,
        ToggleSidebar,
        ToggleCommandPalette,
        OpenSettings,
        SyncNow,
        Analyze,
        SignOut,
    ]
);

/// Register every binding. Called once from the app entry point, after
/// `gpui_component::init` so component contexts already exist.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        // Workspace-wide.
        //
        // `secondary-` is Cmd on macOS and Ctrl elsewhere, so the palette's
        // Cmd+K would collide with Ctrl+K selection movement on Linux and
        // Windows. Those platforms get the familiar Ctrl+Shift+P instead; see
        // `command_palette_shortcut`.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", ToggleCommandPalette, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-p", ToggleCommandPalette, Some(CONTEXT)),
        KeyBinding::new("secondary-b", ToggleSidebar, Some(CONTEXT)),
        KeyBinding::new("secondary-,", OpenSettings, Some(CONTEXT)),
        KeyBinding::new("secondary-r", SyncNow, Some(CONTEXT)),
        KeyBinding::new("secondary-f", FocusSearch, Some(CONTEXT)),
        KeyBinding::new("secondary-c", CopyUrl, Some(CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        // Selection movement works from the input as well as the table, so
        // arrows and the Emacs-style pair steer results without leaving the
        // query. `Input` already binds bare up/down for caret movement inside
        // a multi-line field; the search input is single-line, so there is no
        // conflict.
        KeyBinding::new("up", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("ctrl-k", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("ctrl-j", SelectNext, Some(CONTEXT)),
        KeyBinding::new("secondary-up", SelectFirst, Some(CONTEXT)),
        KeyBinding::new("secondary-down", SelectLast, Some(CONTEXT)),
        KeyBinding::new("enter", OpenInBrowser, Some(CONTEXT)),
        // Space is a literal character while the query has focus, so the
        // detail shortcut only exists where the table owns the keyboard.
        KeyBinding::new("space", ShowDetail, Some(RESULTS_CONTEXT)),
        KeyBinding::new("enter", OpenInBrowser, Some(RESULTS_CONTEXT)),
        KeyBinding::new("up", SelectPrev, Some(RESULTS_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(RESULTS_CONTEXT)),
    ]);
}

/// The label shown for the command palette, matching the binding registered
/// above for this platform.
pub const fn command_palette_shortcut() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘K"
    } else {
        "⌃⇧P"
    }
}
