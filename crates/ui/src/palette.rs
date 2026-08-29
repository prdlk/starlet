//! The command palette (`Cmd+K`).
//!
//! Built on `gpui-component`'s searchable `List` so keyboard navigation,
//! confirmation, and dismissal come from the component rather than from a
//! hand-rolled popup. The palette is the one surface in Starlet that carries a
//! shadow: it is the only thing that ever floats above the workspace.

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, WindowExt as _, h_flex,
    list::{List, ListDelegate, ListItem, ListState},
    v_flex,
};
use starlet_core::query::SortKey;

use crate::search::SearchView;
use crate::services::Session;
use crate::theme::Appearance;

/// One palette entry. Commands are data so the list, the keyboard, and the
/// toolbar buttons cannot drift apart.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Sort(SortKey),
    Filter {
        label: &'static str,
        query: &'static str,
    },
    Sync,
    Analyze,
    Settings,
    ToggleSidebar,
    ClearFilters,
    Appearance(Appearance),
    SignIn,
    SignOut,
}

impl Command {
    fn label(&self) -> SharedString {
        match self {
            Command::Sort(key) => {
                SharedString::from(format!("Sort by {}", key.label().to_lowercase()))
            }
            Command::Filter { label, .. } => SharedString::from(*label),
            Command::Sync => "Sync stars now".into(),
            Command::Analyze => "Analyze with AI…".into(),
            Command::Settings => "Settings…".into(),
            Command::ToggleSidebar => "Toggle filters".into(),
            Command::ClearFilters => "Clear filters".into(),
            Command::Appearance(a) => {
                SharedString::from(format!("Appearance: {}", a.label().to_lowercase()))
            }
            Command::SignIn => "Sign in to GitHub…".into(),
            Command::SignOut => "Sign out".into(),
        }
    }

    fn hint(&self) -> Option<SharedString> {
        use crate::actions::secondary_shortcut;
        match self {
            Command::Filter { query, .. } => Some(SharedString::from(*query)),
            Command::Sync => Some(secondary_shortcut("r").into()),
            Command::Settings => Some(secondary_shortcut(",").into()),
            Command::ToggleSidebar => Some(secondary_shortcut("b").into()),
            _ => None,
        }
    }

    fn group(&self) -> &'static str {
        match self {
            Command::Sort(_) => "Sort",
            Command::Filter { .. } => "Filter",
            Command::Appearance(_) => "Appearance",
            _ => "Actions",
        }
    }
}

fn all_commands(signed_in: bool) -> Vec<Command> {
    let mut commands: Vec<Command> = SortKey::ALL.iter().copied().map(Command::Sort).collect();
    commands.extend([
        Command::Filter {
            label: "Only archived repositories",
            query: "is:archived",
        },
        Command::Filter {
            label: "Hide forks",
            query: "-is:fork",
        },
        Command::Filter {
            label: "More than 1 000 stars",
            query: "stars:>1000",
        },
        Command::Filter {
            label: "Written in Rust",
            query: "lang:rust",
        },
        Command::ClearFilters,
        Command::Sync,
        Command::Analyze,
        Command::ToggleSidebar,
        Command::Settings,
    ]);
    commands.extend(Appearance::ALL.iter().copied().map(Command::Appearance));
    commands.push(if signed_in {
        Command::SignOut
    } else {
        Command::SignIn
    });
    commands
}

pub struct CommandDelegate {
    owner: WeakEntity<SearchView>,
    all: Vec<Command>,
    matched: Vec<Command>,
    selected: Option<IndexPath>,
}

impl CommandDelegate {
    fn new(owner: WeakEntity<SearchView>, signed_in: bool) -> Self {
        let all = all_commands(signed_in);
        Self {
            owner,
            matched: all.clone(),
            all,
            selected: Some(IndexPath::default()),
        }
    }
}

impl ListDelegate for CommandDelegate {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.matched.len()
    }

    fn perform_search(
        &mut self,
        query: &str,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let needle = query.trim().to_lowercase();
        self.matched = if needle.is_empty() {
            self.all.clone()
        } else {
            self.all
                .iter()
                .filter(|c| {
                    c.label().to_lowercase().contains(&needle)
                        || c.group().to_lowercase().contains(&needle)
                })
                .cloned()
                .collect()
        };
        self.selected = (!self.matched.is_empty()).then(IndexPath::default);
        cx.notify();
        Task::ready(())
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let command = self.matched.get(ix.row)?;
        let selected = self.selected == Some(ix);
        let hint = command.hint();
        Some(
            ListItem::new(("command", ix.row)).selected(selected).child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_4()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_baseline()
                            .child(
                                div()
                                    .text_xs()
                                    .w(px(52.))
                                    .flex_none()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(command.group()),
                            )
                            .child(command.label()),
                    )
                    .when_some(hint, |this, hint| {
                        this.child(
                            div()
                                .text_xs()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().muted_foreground)
                                .child(hint),
                        )
                    }),
            ),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
        cx.notify();
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(ix) = self.selected else { return };
        let Some(command) = self.matched.get(ix.row).cloned() else {
            return;
        };
        let owner = self.owner.clone();
        window.close_dialog(cx);
        run(command, owner, window, cx);
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        window.close_dialog(cx);
    }
}

/// Execute a command. Everything routes through the workspace entity so the
/// palette never becomes a second source of truth.
fn run<T: 'static>(
    command: Command,
    owner: WeakEntity<SearchView>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    match command {
        Command::Sort(key) => {
            let prefix = format!("sort:{}", sort_token(key));
            owner
                .update(cx, |view, cx| {
                    let current = view.current_query(cx);
                    let rest: Vec<&str> = current
                        .split_whitespace()
                        .filter(|t| !t.to_lowercase().starts_with("sort:"))
                        .collect();
                    let next = if rest.is_empty() {
                        prefix
                    } else {
                        format!("{} {prefix}", rest.join(" "))
                    };
                    view.set_query(&next, window, cx);
                })
                .ok();
        }
        Command::Filter { query, .. } => {
            owner
                .update(cx, |view, cx| {
                    let current = view.current_query(cx);
                    let next = if current.trim().is_empty() {
                        query.to_string()
                    } else {
                        format!("{} {query}", current.trim())
                    };
                    view.set_query(&next, window, cx);
                })
                .ok();
        }
        Command::ClearFilters => {
            owner.update(cx, |view, cx| view.clear_filters(cx)).ok();
        }
        Command::Sync => window.dispatch_action(Box::new(crate::actions::SyncNow), cx),
        Command::Analyze => window.dispatch_action(Box::new(crate::actions::Analyze), cx),
        Command::Settings => window.dispatch_action(Box::new(crate::actions::OpenSettings), cx),
        Command::ToggleSidebar => {
            window.dispatch_action(Box::new(crate::actions::ToggleSidebar), cx)
        }
        Command::Appearance(appearance) => {
            crate::settings::set_appearance(appearance, window, cx);
        }
        Command::SignIn => crate::signin::SignInFlow::start(window, cx),
        Command::SignOut => window.dispatch_action(Box::new(crate::actions::SignOut), cx),
    }
}

fn sort_token(key: SortKey) -> &'static str {
    match key {
        SortKey::Relevance => "relevance",
        SortKey::Stars => "stars",
        SortKey::Name => "name",
        SortKey::Recent => "recent",
        SortKey::Starred => "starred",
    }
}

pub struct CommandPalette {
    list: Entity<ListState<CommandDelegate>>,
}

impl CommandPalette {
    /// Open the palette, or close it if it is already the topmost surface.
    pub fn toggle(owner: Entity<SearchView>, window: &mut Window, cx: &mut App) {
        if window.has_active_dialog(cx) {
            window.close_dialog(cx);
            return;
        }
        let palette = cx.new(|cx| CommandPalette::new(owner.downgrade(), window, cx));
        let content = palette.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .w(px(560.))
                .close_button(false)
                .child(content.clone())
        });
    }

    fn new(owner: WeakEntity<SearchView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let signed_in = Session::is_signed_in(cx);
        let list = cx.new(|cx| {
            ListState::new(CommandDelegate::new(owner, signed_in), window, cx).searchable(true)
        });
        list.update(cx, |state, cx| state.focus(window, cx));
        Self { list }
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .h(px(420.))
            .child(List::new(&self.list).search_placeholder("Run a command…"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sort_key_is_offered_and_tokenises() {
        let commands = all_commands(true);
        for key in SortKey::ALL {
            assert!(
                commands.contains(&Command::Sort(key)),
                "{key:?} missing from the palette"
            );
            let token = sort_token(key);
            let parsed = starlet_core::query::parse(&format!("sort:{token}"));
            assert_eq!(parsed.sort, Some(key), "sort:{token} must round-trip");
        }
    }

    #[test]
    fn the_account_command_follows_the_session() {
        assert!(all_commands(true).contains(&Command::SignOut));
        assert!(all_commands(false).contains(&Command::SignIn));
        assert!(!all_commands(true).contains(&Command::SignIn));
    }

    #[test]
    fn every_filter_shortcut_parses_into_a_clause() {
        for command in all_commands(true) {
            let Command::Filter { query, label } = command else {
                continue;
            };
            let parsed = starlet_core::query::parse(query);
            assert!(
                !parsed.clauses.is_empty(),
                "{label} ({query}) produced no filter"
            );
            assert!(
                parsed.terms.is_empty(),
                "{label} ({query}) leaked free text"
            );
        }
    }
}
