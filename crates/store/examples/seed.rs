//! Fill a Starlet database with synthetic stars.
//!
//! Used to exercise the interface and the performance targets without a GitHub
//! account. It writes through the real DAO, so FTS triggers and tag mirroring
//! behave exactly as they do in production.
//!
//! ```sh
//! cargo run -p starlet-store --example seed -- ~/.local/share/starlet/starlet.db 5000
//! ```

use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use starlet_core::model::{Contributor, Group, Repo, RepoTag, TagSource};
use starlet_store::Store;

const OWNERS: &[&str] = &[
    "rust-lang",
    "tokio-rs",
    "helix-editor",
    "zed-industries",
    "BurntSushi",
    "sharkdp",
    "clap-rs",
    "serde-rs",
    "hyperium",
    "launchbadge",
    "gfx-rs",
    "bevyengine",
    "emilk",
    "rustdesk",
    "starship",
    "nushell",
    "alacritty",
    "wez",
    "neovim",
    "vim",
    "junegunn",
    "cli",
    "golang",
    "kubernetes",
    "prometheus",
    "grafana",
    "influxdata",
    "hashicorp",
    "facebook",
    "vercel",
    "sveltejs",
    "vuejs",
    "denoland",
    "oven-sh",
    "microsoft",
];

const NOUNS: &[&str] = &[
    "engine",
    "kit",
    "core",
    "cli",
    "server",
    "client",
    "parser",
    "runtime",
    "shell",
    "editor",
    "index",
    "query",
    "store",
    "graph",
    "stream",
    "router",
    "watcher",
    "loader",
    "bridge",
    "daemon",
    "toolkit",
    "sandbox",
    "compiler",
    "linter",
    "formatter",
    "profiler",
];

const ADJECTIVES: &[&str] = &[
    "async",
    "fast",
    "tiny",
    "modern",
    "portable",
    "modal",
    "reactive",
    "embedded",
    "distributed",
    "typed",
    "zero-copy",
    "incremental",
    "declarative",
    "headless",
];

const LANGUAGES: &[&str] = &[
    "Rust",
    "Go",
    "TypeScript",
    "Python",
    "C",
    "C++",
    "Zig",
    "Shell",
    "Lua",
    "Elixir",
    "JavaScript",
    "Kotlin",
    "Swift",
    "Haskell",
    "OCaml",
    "Nix",
];

const TOPICS: &[&str] = &[
    "cli",
    "tui",
    "async",
    "database",
    "editor",
    "search",
    "web",
    "graphics",
    "parser",
    "networking",
    "devtools",
    "wasm",
    "testing",
    "observability",
    "security",
    "ai",
];

/// A deterministic 64-bit PRNG. Reproducible corpora make timing runs
/// comparable and keep screenshots stable between runs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .map(std::path::PathBuf::from)
        .unwrap_or(starlet_store::default_database_path()?);
    let count: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(5_000);

    let store = Store::open(&path).await?;
    let mut rng = Rng(0x5EED_57A2_1E7);
    let now = Utc::now();

    let mut repos = Vec::with_capacity(count);
    for id in 1..=count as i64 {
        let owner = rng.pick(OWNERS).to_string();
        let name = format!("{}-{}", rng.pick(ADJECTIVES), rng.pick(NOUNS));
        let full_name = format!("{owner}/{name}-{id}");
        let language = rng.pick(LANGUAGES).to_string();

        let mut languages = BTreeMap::new();
        languages.insert(language.clone(), 20_000 + rng.below(400_000) as i64);
        if rng.below(3) == 0 {
            languages.insert("Shell".to_string(), 500 + rng.below(4_000) as i64);
        }

        let topics: Vec<String> = (0..1 + rng.below(3))
            .map(|_| rng.pick(TOPICS).to_string())
            .collect();

        repos.push(Repo {
            id,
            node_id: format!("R_seed{id}"),
            name: format!("{name}-{id}"),
            owner,
            html_url: format!("https://github.com/{full_name}"),
            description: Some(format!(
                "A {} {} for {} workloads",
                rng.pick(ADJECTIVES),
                rng.pick(NOUNS),
                rng.pick(TOPICS)
            )),
            stargazers: (rng.below(200_000) as i64).pow(1) / (1 + rng.below(4) as i64),
            last_commit_at: Some(now - Duration::days(rng.below(900) as i64)),
            primary_language: Some(language),
            languages,
            contributors: if id % 7 == 0 {
                vec![Contributor {
                    login: "seed-bot".into(),
                    avatar_url: "https://avatars.githubusercontent.com/u/0".into(),
                    contributions: 100,
                }]
            } else {
                Vec::new()
            },
            starred_at: Some(now - Duration::hours(rng.below(30_000) as i64)),
            archived: rng.below(20) == 0,
            fork: rng.below(12) == 0,
            topics,
            updated_at: Some(now - Duration::days(rng.below(30) as i64)),
            synced_at: Some(now),
            full_name,
            tags: Vec::new(),
            groups: Vec::new(),
        });
    }

    let started = std::time::Instant::now();
    for chunk in repos.chunks(500) {
        store.upsert_repos(chunk).await?;
    }
    println!("inserted {count} repositories in {:?}", started.elapsed());

    // A handful of AI tags and groups so the sidebar and the muted-tag styling
    // have something real to render.
    for repo in repos.iter().take(count.min(600)) {
        let tags: Vec<RepoTag> = (0..2 + rng.below(3))
            .map(|_| RepoTag {
                name: rng.pick(TOPICS).to_string(),
                source: TagSource::Ai,
                confidence: 0.5 + (rng.below(50) as f32) / 100.0,
            })
            .collect();
        store.set_ai_tags(repo.id, &tags).await?;
    }

    let groups: Vec<Group> = ["Developer tools", "Data and storage", "Graphics", "Web"]
        .iter()
        .enumerate()
        .map(|(ix, name)| Group {
            name: name.to_string(),
            summary: format!("Cluster {} of the seeded corpus", ix + 1),
            source: TagSource::Ai,
            members: repos
                .iter()
                .skip(ix * 40)
                .step_by(4)
                .take(60)
                .map(|r| r.full_name.clone())
                .collect(),
        })
        .collect();
    store.replace_ai_groups(&groups).await?;

    store
        .set_state(starlet_store::KEY_INITIAL_SYNC_DONE, "1")
        .await?;
    println!("database ready at {}", path.display());
    Ok(())
}
