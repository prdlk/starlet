//! Starlet: a local-first search engine for your GitHub stars.
//!
//! This binary does startup and nothing else: open the database, start the I/O
//! runtime, install the theme and key bindings, open one window. Every decision
//! past that point belongs to `starlet-ui`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext as _, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point,
    px, size,
};
use gpui_component::Root;
use starlet_store::Store;
use starlet_ui::{Backend, Session, settings};

/// Comfortable default for a dense table plus the filter sidebar.
const DEFAULT_SIZE: (f32, f32) = (1180.0, 760.0);
/// Below this the table stops being readable, so the window refuses to shrink.
const MIN_SIZE: (f32, f32) = (720.0, 480.0);

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("STARLET_LOG")
                .unwrap_or_else(|_| "starlet=info,warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // One multi-threaded runtime for SQLite and HTTP. GPUI owns the main
    // thread; this owns everything that blocks.
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("starlet-io")
            .build()
            .context("start the I/O runtime")?,
    );

    // `STARLET_DB` points the app at another file. Useful for a scratch
    // corpus and for running two builds side by side.
    let database = match std::env::var_os("STARLET_DB") {
        Some(path) => std::path::PathBuf::from(path),
        None => starlet_store::default_database_path().context("resolve the database path")?,
    };
    let store = runtime
        .block_on(Store::open(&database))
        .with_context(|| format!("open {}", database.display()))?;

    // The persisted appearance decides the first painted frame, so it is read
    // before the window exists rather than applied as a flash afterwards.
    let appearance = runtime.block_on(async {
        settings::parse_appearance(
            store
                .get_state(settings::KEY_APPEARANCE)
                .await
                .ok()
                .flatten()
                .as_deref(),
        )
    });

    Application::new()
        .with_assets(starlet_ui::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            starlet_ui::init(appearance, cx);

            cx.set_global(Backend::new(store, runtime));
            cx.set_global(Session::restore());
            cx.activate(true);

            let options = window_options(cx);
            cx.open_window(options, |window, cx| {
                let workspace = starlet_ui::workspace(window, cx);
                cx.new(|cx| Root::new(workspace, window, cx))
            })
            .expect("open the main window");
        });

    Ok(())
}

fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Starlet".into()),
            appears_transparent: false,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
        app_id: Some("dev.starlet.Starlet".into()),
        ..Default::default()
    }
}
