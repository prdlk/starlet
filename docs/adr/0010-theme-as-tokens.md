# 10. The palette is a theme file, and the app carries its own assets

Status: accepted

## Context

The visual target is Vercel/shadcn: background `#0a0a0a`, surface `#111111`,
border `#262626`, text `#fafafa`, muted `#a1a1aa`, one accent, 6 px radius, no
shadows. `gpui-component` reads every colour from a global `Theme` whose
configuration is a JSON document.

Separately, the published `gpui-component` crate does not ship the SVG files
its `IconName` variants reference.

## Decision

**One theme file.** `crates/ui/src/theme/starlet.json` defines a dark theme and
a light theme as `ThemeConfig` documents. At startup they replace the stock
configs on the global `Theme`. No application view names a colour; they read
`cx.theme()`.

**A build script embeds `assets/`.** `crates/ui/build.rs` walks the directory
and emits a `&[(&str, &[u8])]` of `include_bytes!` entries. The 86 Lucide icons
`gpui-component 0.5.1` references are vendored from its own repository at the
matching tag, and the Geist Sans and Geist Mono TTFs are vendored from
`vercel/geist-font`.

**One audited exception to the no-literal-colours rule.** Language dots and the
language bar compute their hue from the language name, using the familiar
linguist hues, with an FNV-derived hue for anything unlisted. The colour *is*
the data there, which is the case the design guide permits.

## Consequences

* Switching to light, or to any future theme, is a data change. Both themes are
  defined, so the "match system" setting is real rather than aspirational.
* A missing icon renders as an empty box at runtime, so a test asserts the set
  is embedded and names the ones the interface actually uses.
* If the bundled fonts fail to register, `theme::install` does not claim the
  Geist families and the platform UI font is used instead. The app never asks
  for a family that is not there.
* Radius comes from the theme (`6`), and `shadow` is `false`, so the only
  elevated surface is the command palette.
