//! The embedded asset source: Lucide icons and the Geist type family.

use std::borrow::Cow;

use gpui::{App, AssetSource, SharedString};

include!(concat!(env!("OUT_DIR"), "/embedded_assets.rs"));

/// Serves `assets/` out of the binary.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let path = path.trim_start_matches('/');
        Ok(EMBEDDED
            .iter()
            .find(|(key, _)| *key == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let prefix = path.trim_start_matches('/');
        Ok(EMBEDDED
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .map(|(key, _)| SharedString::from(*key))
            .collect())
    }
}

/// Family name of the embedded UI typeface.
pub const UI_FONT: &str = "Geist";
/// Family name of the embedded monospace typeface.
pub const MONO_FONT: &str = "Geist Mono";

/// Register the bundled fonts with the text system.
///
/// If registration fails the app still runs: `theme::install` only claims the
/// Geist families when this succeeded, so the theme falls back to the platform
/// UI font instead of rendering with a missing family.
pub fn load_fonts(cx: &App) -> gpui::Result<()> {
    let fonts: Vec<Cow<'static, [u8]>> = EMBEDDED
        .iter()
        .filter(|(key, _)| key.starts_with("fonts/") && key.ends_with(".ttf"))
        .map(|(_, bytes)| Cow::Borrowed(*bytes))
        .collect();
    cx.text_system().add_fonts(fonts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_gpui_component_asks_for_is_present() {
        // The published `gpui-component` crate does not ship its SVGs; the
        // application must. A missing file renders as an empty box at runtime,
        // so check the whole set here instead.
        let icons: Vec<&str> = EMBEDDED
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| k.starts_with("icons/"))
            .collect();
        assert!(icons.len() > 60, "only {} icons embedded", icons.len());
        for required in [
            "icons/search.svg",
            "icons/settings.svg",
            "icons/star.svg",
            "icons/github.svg",
            "icons/external-link.svg",
            "icons/panel-left.svg",
            "icons/close.svg",
        ] {
            assert!(icons.contains(&required), "{required} is not embedded");
        }
    }

    #[test]
    fn fonts_are_embedded() {
        let fonts: Vec<&str> = EMBEDDED
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| k.starts_with("fonts/"))
            .collect();
        assert!(fonts.contains(&"fonts/Geist-Regular.ttf"));
        assert!(fonts.contains(&"fonts/GeistMono-Regular.ttf"));
    }

    #[test]
    fn loading_is_path_normalised() {
        assert!(Assets.load("icons/search.svg").unwrap().is_some());
        assert!(Assets.load("/icons/search.svg").unwrap().is_some());
        assert!(Assets.load("icons/not-a-real-icon.svg").unwrap().is_none());
    }
}
