//! Starlet's palette, expressed as a `gpui-component` theme.
//!
//! Every colour in the product lives in `theme/starlet.json`. Views read
//! `cx.theme()` and never name a hex value, so the whole surface follows the
//! theme — including the light variant, which exists so the OS-appearance path
//! is real rather than a claim.

use std::rc::Rc;

use gpui::{App, Window};
use gpui_component::{Theme, ThemeMode, ThemeSet};

use crate::assets;

const STARLET_THEME: &str = include_str!("theme/starlet.json");

/// How the interface picks between light and dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    /// Dark, always. The product default.
    #[default]
    Dark,
    Light,
    /// Track the OS.
    System,
}

impl Appearance {
    pub fn label(self) -> &'static str {
        match self {
            Appearance::Dark => "Dark",
            Appearance::Light => "Light",
            Appearance::System => "Match system",
        }
    }

    pub const ALL: [Appearance; 3] = [Appearance::Dark, Appearance::Light, Appearance::System];
}

/// Install the Starlet themes and select `appearance`.
///
/// Must run after `gpui_component::init`, which creates the global `Theme` this
/// replaces the configs on.
pub fn install(appearance: Appearance, fonts_loaded: bool, cx: &mut App) {
    let set: ThemeSet = match serde_json::from_str(STARLET_THEME) {
        Ok(set) => set,
        // The theme is compiled in, so a failure here is a build-time mistake
        // that has escaped the test below. Keep the stock theme and say so.
        Err(err) => {
            tracing::error!("starlet theme is malformed, falling back to defaults: {err}");
            return;
        }
    };

    let theme = Theme::global_mut(cx);
    for config in set.themes {
        if config.mode.is_dark() {
            theme.dark_theme = Rc::new(config);
        } else {
            theme.light_theme = Rc::new(config);
        }
    }

    apply(appearance, None, cx);

    if fonts_loaded {
        let theme = Theme::global_mut(cx);
        theme.font_family = assets::UI_FONT.into();
        theme.mono_font_family = assets::MONO_FONT.into();
    }
}

/// Switch appearance. `Theme::change` reprojects the scrollbar and resize
/// handles owned by the base layer, so this is the only correct entry point.
pub fn apply(appearance: Appearance, window: Option<&mut Window>, cx: &mut App) {
    match appearance {
        Appearance::Dark => Theme::change(ThemeMode::Dark, window, cx),
        Appearance::Light => Theme::change(ThemeMode::Light, window, cx),
        Appearance::System => Theme::sync_system_appearance(window, cx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::Colorize as _;

    #[test]
    fn the_bundled_theme_parses_and_covers_both_modes() {
        let set: ThemeSet = serde_json::from_str(STARLET_THEME).expect("theme must parse");
        assert_eq!(set.themes.len(), 2);
        assert!(set.themes.iter().any(|t| t.mode.is_dark()));
        assert!(set.themes.iter().any(|t| !t.mode.is_dark()));
    }

    #[test]
    fn the_dark_theme_uses_the_specified_surface_colours() {
        let set: ThemeSet = serde_json::from_str(STARLET_THEME).unwrap();
        let dark = set.themes.iter().find(|t| t.mode.is_dark()).unwrap();
        // Round-tripping through Hsla loses nothing at these values, and this
        // is the only place a literal is allowed to appear.
        for (actual, expected) in [
            (&dark.colors.background, "#0a0a0a"),
            (&dark.colors.foreground, "#fafafa"),
            (&dark.colors.border, "#262626"),
            (&dark.colors.muted_foreground, "#a1a1aa"),
        ] {
            let parsed = gpui::Hsla::parse_hex(actual.as_ref().expect("colour set")).unwrap();
            assert_eq!(
                parsed.to_hex().to_lowercase()[..7],
                expected[..7].to_lowercase()
            );
        }
        assert_eq!(dark.radius, Some(6));
        assert_eq!(dark.shadow, Some(false));
    }
}
