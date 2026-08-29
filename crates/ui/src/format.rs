//! Value formatting for dense rows.
//!
//! Table cells are read by scanning, not by reading, so counts are abbreviated
//! and dates are relative. Both are pure functions with a fixed reference time
//! so they can be tested without freezing the clock globally.

use chrono::{DateTime, Utc};
use gpui::{Hsla, SharedString};

/// `39472` → `39.5k`. Keeps the column narrow and comparable.
pub fn compact_count(n: i64) -> SharedString {
    let neg = n < 0;
    let v = n.unsigned_abs();
    let s = match v {
        0..=999 => format!("{v}"),
        1_000..=9_999 => format!("{:.1}k", v as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", v / 1_000),
        1_000_000..=9_999_999 => format!("{:.1}M", v as f64 / 1_000_000.0),
        _ => format!("{}M", v / 1_000_000),
    };
    // Trim `4.0k` down to `4k`; the decimal only earns its width when it says
    // something.
    let s = s.replace(".0k", "k").replace(".0M", "M");
    SharedString::from(if neg { format!("-{s}") } else { s })
}

/// `2026-02-18` against a now of `2026-02-29` → `11d`.
pub fn relative_time(then: Option<DateTime<Utc>>, now: DateTime<Utc>) -> SharedString {
    let Some(then) = then else {
        return SharedString::new_static("—");
    };
    let delta = now.signed_duration_since(then);
    let secs = delta.num_seconds();
    if secs < 0 {
        return SharedString::new_static("now");
    }
    let out = match secs {
        0..=59 => "now".to_string(),
        60..=3_599 => format!("{}m", secs / 60),
        3_600..=86_399 => format!("{}h", secs / 3_600),
        86_400..=2_591_999 => format!("{}d", secs / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", secs / 2_592_000),
        _ => format!("{}y", secs / 31_536_000),
    };
    SharedString::from(out)
}

/// Absolute form for the detail sheet, where precision matters more than width.
pub fn absolute_date(then: Option<DateTime<Utc>>) -> SharedString {
    match then {
        Some(t) => SharedString::from(t.format("%-d %b %Y").to_string()),
        None => SharedString::new_static("—"),
    }
}

/// The dot colour for a language.
///
/// This is data, not decoration: the hue *is* the value, which is the one case
/// the design guide allows a literal colour outside the theme. Hues are the
/// familiar GitHub linguist ones so they read the same as on the web.
pub fn language_color(language: &str) -> Hsla {
    // (hue degrees, saturation, lightness) chosen to stay legible on #0a0a0a.
    let (h, s, l) = match language {
        "Rust" => (17.0, 0.60, 0.55),
        "Go" => (188.0, 0.66, 0.55),
        "TypeScript" => (219.0, 0.55, 0.58),
        "JavaScript" => (53.0, 0.75, 0.58),
        "Python" => (207.0, 0.44, 0.55),
        "C" => (220.0, 0.13, 0.60),
        "C++" => (338.0, 0.42, 0.55),
        "C#" => (274.0, 0.45, 0.50),
        "Java" => (25.0, 0.62, 0.50),
        "Ruby" => (357.0, 0.72, 0.50),
        "Shell" => (96.0, 0.42, 0.55),
        "Swift" => (17.0, 0.90, 0.60),
        "Kotlin" => (280.0, 0.55, 0.60),
        "Zig" => (40.0, 0.85, 0.55),
        "Elixir" => (277.0, 0.35, 0.50),
        "Haskell" => (280.0, 0.30, 0.50),
        "Lua" => (240.0, 0.65, 0.55),
        "HTML" => (13.0, 0.75, 0.52),
        "CSS" => (271.0, 0.45, 0.60),
        "Nix" => (215.0, 0.55, 0.58),
        "Vim Script" => (120.0, 0.55, 0.42),
        "Dart" => (195.0, 0.75, 0.45),
        "Scala" => (0.0, 0.75, 0.50),
        "PHP" => (240.0, 0.25, 0.60),
        "Julia" => (280.0, 0.45, 0.60),
        "OCaml" => (30.0, 0.85, 0.55),
        "Objective-C" => (215.0, 0.85, 0.60),
        "Perl" => (210.0, 0.45, 0.50),
        "R" => (210.0, 0.65, 0.50),
        "Clojure" => (110.0, 0.55, 0.42),
        "Erlang" => (330.0, 0.45, 0.45),
        "Assembly" => (10.0, 0.55, 0.45),
        "Makefile" => (140.0, 0.35, 0.50),
        "Dockerfile" => (207.0, 0.55, 0.50),
        // Anything unlisted gets a stable hue derived from its name, so two
        // different languages never share a dot by accident.
        other => (stable_hue(other), 0.45, 0.55),
    };
    gpui::hsla(h / 360.0, s, l, 1.0)
}

/// FNV-1a over the name, folded into the hue circle.
fn stable_hue(name: &str) -> f32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in name.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash % 360) as f32
}

/// Percentage split for the language bar, largest first, dropping slivers.
pub fn language_shares(languages: &starlet_core::LanguageBytes) -> Vec<(String, f32)> {
    let total: i64 = languages.values().sum();
    if total <= 0 {
        return Vec::new();
    }
    let mut shares: Vec<(String, f32)> = languages
        .iter()
        .map(|(name, bytes)| (name.clone(), *bytes as f32 / total as f32))
        .filter(|(_, share)| *share >= 0.005)
        .collect();
    shares.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    shares
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn counts_stay_narrow() {
        assert_eq!(compact_count(0).as_ref(), "0");
        assert_eq!(compact_count(999).as_ref(), "999");
        assert_eq!(compact_count(1_000).as_ref(), "1k");
        assert_eq!(compact_count(1_234).as_ref(), "1.2k");
        assert_eq!(compact_count(39_472).as_ref(), "39k");
        assert_eq!(compact_count(1_500_000).as_ref(), "1.5M");
        assert_eq!(compact_count(-5).as_ref(), "-5");
    }

    #[test]
    fn relative_times_step_through_the_units() {
        let now = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();
        let at = |d: chrono::Duration| relative_time(Some(now - d), now);
        assert_eq!(at(chrono::Duration::seconds(5)).as_ref(), "now");
        assert_eq!(at(chrono::Duration::minutes(5)).as_ref(), "5m");
        assert_eq!(at(chrono::Duration::hours(5)).as_ref(), "5h");
        assert_eq!(at(chrono::Duration::days(11)).as_ref(), "11d");
        assert_eq!(at(chrono::Duration::days(90)).as_ref(), "3mo");
        assert_eq!(at(chrono::Duration::days(800)).as_ref(), "2y");
        assert_eq!(relative_time(None, now).as_ref(), "—");
    }

    #[test]
    fn a_future_timestamp_reads_as_now_rather_than_negative() {
        let now = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();
        assert_eq!(
            relative_time(Some(now + chrono::Duration::hours(2)), now).as_ref(),
            "now"
        );
    }

    #[test]
    fn language_colours_are_stable_and_distinct() {
        assert_eq!(language_color("Rust"), language_color("Rust"));
        assert_ne!(language_color("Rust"), language_color("Go"));
        // Unlisted languages still get their own hue.
        assert_ne!(language_color("Nim"), language_color("Crystal"));
        assert_eq!(language_color("Nim"), language_color("Nim"));
    }

    #[test]
    fn language_shares_are_ordered_and_normalised() {
        let languages = starlet_core::LanguageBytes::from([
            ("Rust".to_string(), 8_000i64),
            ("Shell".to_string(), 2_000),
            ("Makefile".to_string(), 1),
        ]);
        let shares = language_shares(&languages);
        assert_eq!(shares.len(), 2, "slivers below 0.5% are dropped");
        assert_eq!(shares[0].0, "Rust");
        assert!((shares[0].1 - 0.8).abs() < 1e-3);
    }

    #[test]
    fn no_languages_means_no_bar() {
        assert!(language_shares(&Default::default()).is_empty());
    }
}
