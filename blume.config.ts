import { defineConfig } from "blume";

/**
 * Starlet's developer documentation.
 *
 * The site is published to GitHub Pages as a *project* site, so it is served
 * from `https://prdlk.github.io/starlet` — `deployment.site` is the origin and
 * `deployment.base` is the subdirectory. Both are needed: GitHub Pages exposes
 * no environment variable Blume could detect the origin from.
 */
export default defineConfig({
  title: "Starlet",
  description:
    "Developer documentation for Starlet — a local-first desktop search engine for your GitHub stars, built in Rust on GPUI and SQLite.",

  // Single-path SVG filled with `currentColor`, so the mark follows the theme.
  logo: {
    image: "/logo.svg",
    text: "Starlet",
  },

  github: {
    owner: "prdlk",
    repo: "starlet",
    branch: "main",
  },

  // The app bundles Geist and Geist Mono; the docs use the same faces so the
  // screenshots and the prose around them belong to one typographic family.
  theme: {
    accent: "teal",
    radius: "md",
    mode: "dark",
    fonts: {
      display: "geist",
      body: "geist",
      mono: "geist-mono",
    },
  },

  // Groups are collapsible: the crate reference and the ADR log are long, and
  // a flat sidebar would bury the section the reader is actually in.
  navigation: {
    sidebar: {
      display: "group",
    },
  },

  // Derived from git history, which is why the Pages workflow checks out with
  // `fetch-depth: 0` — a shallow clone would date every page to the last commit.
  lastModified: true,

  search: {
    provider: "orama",
  },

  ai: {
    llmsTxt: true,
  },

  seo: {
    og: { enabled: true },
    sitemap: true,
    robots: true,
    structuredData: true,
  },

  deployment: {
    output: "static",
    site: "https://prdlk.github.io",
    base: "/starlet",
  },
});
