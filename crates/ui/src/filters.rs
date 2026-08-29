//! The collapsible filter sidebar: groups and tags as facets.
//!
//! Facets are a second, orthogonal filter to the query text. They live here
//! rather than being rewritten into the input because a facet is a persistent
//! selection the user toggles, and folding it into the query string would make
//! every toggle also destroy whatever they had typed.

use std::collections::BTreeSet;

use gpui::{
    App, Context, EventEmitter, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IconName, Side,
    sidebar::{Sidebar, SidebarGroup, SidebarMenu},
    v_flex,
};
use starlet_core::model::Repo;
use starlet_store::{GroupFacet, TagFacet};

use crate::services::Backend;

/// The facets currently switched on. Empty means "no facet constraint".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FacetFilters {
    pub tags: BTreeSet<String>,
    pub groups: BTreeSet<String>,
}

impl FacetFilters {
    /// Selections within one facet are ORed, and the two facets are ANDed:
    /// picking two tags widens, picking a tag and a group narrows. That is what
    /// people expect from faceted browsing and it is the only combination that
    /// keeps every click meaningful.
    pub fn matches(&self, repo: &Repo) -> bool {
        let tags_ok = self.tags.is_empty() || self.tags.iter().any(|t| repo.has_tag(t));
        let groups_ok = self.groups.is_empty() || self.groups.iter().any(|g| repo.in_group(g));
        tags_ok && groups_ok
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.groups.is_empty()
    }
}

pub struct FilterPanel {
    active: FacetFilters,
    tags: Vec<TagFacet>,
    groups: Vec<GroupFacet>,
}

impl EventEmitter<FacetFilters> for FilterPanel {}

impl FilterPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            active: FacetFilters::default(),
            tags: Vec::new(),
            groups: Vec::new(),
        };
        this.reload(cx);
        this
    }

    pub fn active(&self) -> &FacetFilters {
        &self.active
    }

    /// Re-read the facet counts. Called after a sync or an analysis run.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let store = Backend::global(cx).store();
        let rx = Backend::global(cx).spawn(async move {
            let tags = store.tag_facets().await.unwrap_or_default();
            let groups = store.group_facets().await.unwrap_or_default();
            (tags, groups)
        });
        cx.spawn(async move |this, cx| {
            let Ok((tags, groups)) = rx.await else { return };
            let _ = this.update(cx, |this, cx| {
                // Keep at most the head of a long tail: a sidebar with 800
                // one-use topics is not a filter, it is a wall.
                this.tags = merge_by_name(tags).into_iter().take(40).collect();
                this.groups = groups;
                this.prune_missing_selections();
                cx.notify();
            });
        })
        .detach();
    }

    /// Drop selections whose facet no longer exists, so a stale filter cannot
    /// silently empty the result list after an unstar or a re-analysis.
    fn prune_missing_selections(&mut self) {
        let known_tags: BTreeSet<&str> = self.tags.iter().map(|t| t.name.as_str()).collect();
        let known_groups: BTreeSet<&str> = self.groups.iter().map(|g| g.name.as_str()).collect();
        self.active.tags.retain(|t| known_tags.contains(t.as_str()));
        self.active
            .groups
            .retain(|g| known_groups.contains(g.as_str()));
    }

    fn toggle_tag(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.active.tags.remove(name) {
            self.active.tags.insert(name.to_string());
        }
        self.publish(cx);
    }

    fn toggle_group(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.active.groups.remove(name) {
            self.active.groups.insert(name.to_string());
        }
        self.publish(cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.active.is_empty() {
            return;
        }
        self.active = FacetFilters::default();
        self.publish(cx);
    }

    fn publish(&mut self, cx: &mut Context<Self>) {
        cx.emit(self.active.clone());
        cx.notify();
    }
}

impl Render for FilterPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let groups: Vec<_> = self.groups.clone();
        let tags: Vec<_> = self.tags.clone();

        let group_menu = SidebarMenu::new().children(groups.into_iter().map(|group| {
            let active = self.active.groups.contains(&group.name);
            let name = group.name.clone();
            let count = group.count;
            gpui_component::sidebar::SidebarMenuItem::new(SharedString::from(group.name.clone()))
                .icon(IconName::Folder)
                .active(active)
                .suffix(count_badge(count, cx))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&name, cx)))
        }));

        let tag_menu = SidebarMenu::new().children(tags.into_iter().map(|tag| {
            let active = self.active.tags.contains(&tag.name);
            let name = tag.name.clone();
            let count = tag.count;
            gpui_component::sidebar::SidebarMenuItem::new(SharedString::from(tag.name.clone()))
                .active(active)
                .suffix(count_badge(count, cx))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_tag(&name, cx)))
        }));

        let empty = self.groups.is_empty() && self.tags.is_empty();

        Sidebar::new(Side::Left)
            .w(px(232.))
            .children(vec![
                SidebarGroup::new("Groups").child(group_menu),
                SidebarGroup::new("Tags").child(tag_menu),
            ])
            .when(empty, |sidebar| {
                sidebar.footer(
                    v_flex()
                        .p_3()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Run Analyze to group and tag your stars."),
                )
            })
    }
}

/// Collapse facets that differ only by source.
///
/// The same word can exist as a GitHub topic and as an AI tag; the filter
/// matches by name regardless of source, so showing "cli 588" beside
/// "cli 114" would offer two controls that do the same thing. Counts are
/// summed and the strongest source wins the label.
fn merge_by_name(tags: Vec<TagFacet>) -> Vec<TagFacet> {
    let mut merged: Vec<TagFacet> = Vec::with_capacity(tags.len());
    for tag in tags {
        match merged
            .iter_mut()
            .find(|t| t.name.eq_ignore_ascii_case(&tag.name))
        {
            Some(existing) => {
                existing.count += tag.count;
                existing.source = existing.source.max(tag.source);
            }
            None => merged.push(tag),
        }
    }
    // Only offer a facet that actually partitions the corpus.
    merged.retain(|t| t.count > 1);
    merged.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    merged
}

fn count_badge(count: i64, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_family(cx.theme().mono_font_family.clone())
        .text_color(cx.theme().muted_foreground)
        .child(SharedString::from(count.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(tags: &[&str], groups: &[&str]) -> Repo {
        Repo {
            tags: tags
                .iter()
                .map(|t| starlet_core::model::RepoTag {
                    name: t.to_string(),
                    source: starlet_core::model::TagSource::Ai,
                    confidence: 1.0,
                })
                .collect(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn facets_merge_across_sources_and_drop_singletons() {
        use starlet_core::model::TagSource;
        let merged = merge_by_name(vec![
            TagFacet {
                name: "cli".into(),
                source: TagSource::Github,
                count: 588,
            },
            TagFacet {
                name: "CLI".into(),
                source: TagSource::Ai,
                count: 114,
            },
            TagFacet {
                name: "wasm".into(),
                source: TagSource::Ai,
                count: 110,
            },
            TagFacet {
                name: "once".into(),
                source: TagSource::Github,
                count: 1,
            },
        ]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].name, "cli");
        assert_eq!(merged[0].count, 702, "counts sum across sources");
        assert_eq!(merged[0].source, TagSource::User.min(TagSource::Ai));
        assert_eq!(merged[1].name, "wasm");
    }

    #[test]
    fn no_selection_matches_everything() {
        assert!(FacetFilters::default().matches(&repo(&[], &[])));
    }

    #[test]
    fn tags_or_and_facets_and() {
        let filters = FacetFilters {
            tags: ["cli", "tui"].iter().map(|s| s.to_string()).collect(),
            groups: ["Editors".to_string()].into_iter().collect(),
        };
        assert!(filters.matches(&repo(&["cli"], &["Editors"])));
        assert!(filters.matches(&repo(&["tui"], &["Editors"])));
        assert!(
            !filters.matches(&repo(&["cli"], &["Databases"])),
            "group must also match"
        );
        assert!(
            !filters.matches(&repo(&["web"], &["Editors"])),
            "tag must also match"
        );
    }
}
