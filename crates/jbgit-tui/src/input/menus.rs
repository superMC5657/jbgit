use jbgit_core::PaneId;

use crate::app::{Action, AppState, ContextMenuAction, ContextMenuItem, PopupKind};
use crate::graph::GraphRendererMode;
use crate::hit_test::HitTarget;
use crate::input_menus::{context_menu_for_target, keyboard_menu_target};
use crate::input_stage::stage_tree_context_menu_items;
use crate::log_view::PresentationToggle;

pub(crate) fn keyboard_context_menu(state: &mut AppState) -> Option<Vec<Action>> {
    let (target, anchor) = keyboard_menu_target(state)?;
    let mut actions = Vec::new();
    let items = build_menu_for(&target, state, &mut actions)?;
    if items.is_empty() {
        return None;
    }
    actions.push(Action::OpenContextMenu {
        anchor,
        items,
        depth: 0,
    });
    Some(actions)
}

/// Context-menu items for a right-clicked (or keyboard-focused) target.
pub(super) fn build_menu_for(
    target: &HitTarget,
    state: &mut AppState,
    actions: &mut Vec<Action>,
) -> Option<Vec<ContextMenuItem>> {
    if let Some(items) = context_menu_for_target(target, state, actions) {
        return Some(items);
    }
    let items = match target {
        HitTarget::BranchTreeNode { row: idx, .. } => {
            let idx = *idx;
            if state.branches.selected != idx {
                actions.push(Action::FocusPane(PaneId::LogBranchesTree));
                actions.push(Action::SelectBranchRow(idx));
            }
            let node = state
                .branches
                .rows
                .get(idx)
                .map(|r| &state.branches.model.nodes[r.node]);
            let branch_name = node
                .and_then(|n| n.ref_name.clone())
                .unwrap_or_else(|| "HEAD".to_string());
            let is_leaf = node.is_some_and(|n| n.is_leaf());
            let check = |on: bool| if on { "✓" } else { " " };
            let settings = &state.branches.settings;
            let mut items = match node.map(|n| (n.kind, n.ref_name.clone())) {
                Some((jbgit_core::BranchNodeKind::Leaf(kind), Some(name))) => {
                    let mut b = crate::menus::branch_menu_items(state, &name, kind);
                    if !b.is_empty() {
                        b.push(ContextMenuItem::separator());
                    }
                    b
                }
                Some((jbgit_core::BranchNodeKind::Leaf(jbgit_core::RefKind::Head), None)) => {
                    let mut b =
                        crate::menus::branch_menu_items(state, "", jbgit_core::RefKind::Head);
                    b.push(ContextMenuItem::separator());
                    b
                }
                _ => Vec::new(),
            };
            let i18n = &state.i18n;
            let km = &state.config.keymap;
            let nav_sc = km.primary_key_for(
                &[crate::config::KeymapScope::Branch],
                &crate::keymap::KeyAction::NavigateBranch,
            );
            let fav_sc = km.primary_key_for(
                &[crate::config::KeymapScope::Branch],
                &crate::keymap::KeyAction::ToggleFavorite,
            );
            let refresh_sc = km.primary_key_for(
                &[crate::config::KeymapScope::Global],
                &crate::keymap::KeyAction::Refresh,
            );
            items.push(ContextMenuItem::action(
                if is_leaf {
                    i18n.t_fmt("menu.branch.show_only_branch", &[("name", &branch_name)])
                } else {
                    i18n.t("menu.branch.show_only_branches").to_string()
                },
                Some('f'),
                Some("Enter"),
                ContextMenuAction::BranchesApplyFilter,
            ));
            if is_leaf {
                items.push(ContextMenuItem::action(
                    i18n.t("menu.branch.navigate_to_tip"),
                    Some('n'),
                    nav_sc.as_deref(),
                    ContextMenuAction::BranchesNavigate,
                ));
                items.push(ContextMenuItem::action(
                    if node.is_some_and(|n| n.is_favorite) {
                        i18n.t("menu.branch.unmark_favorite")
                    } else {
                        i18n.t("menu.branch.mark_favorite")
                    },
                    Some('m'),
                    fav_sc.as_deref(),
                    ContextMenuAction::BranchesToggleFavorite,
                ));
                items.push(ContextMenuItem::action(
                    i18n.t_fmt("menu.branch.log_branch", &[("name", &branch_name)]),
                    Some('l'),
                    None,
                    ContextMenuAction::RunReadOnlyProbe(vec![
                        "log".into(),
                        "-n".into(),
                        "10".into(),
                        "--oneline".into(),
                        branch_name,
                    ]),
                ));
            }
            items.push(ContextMenuItem::submenu(
                i18n.t("menu.branch.view_options"),
                Some('o'),
                vec![
                    ContextMenuItem::action(
                        format!(
                            "{} {}",
                            check(settings.options.group_by_directory),
                            i18n.t("menu.branch.opt_group_by_dir")
                        ),
                        Some('d'),
                        None,
                        ContextMenuAction::BranchesToggleGroupByDir,
                    ),
                    ContextMenuItem::action(
                        format!(
                            "{} {}",
                            check(settings.options.show_tags),
                            i18n.t("menu.branch.opt_show_tags")
                        ),
                        Some('t'),
                        None,
                        ContextMenuAction::BranchesToggleShowTags,
                    ),
                    ContextMenuItem::action(
                        format!(
                            "{} {}",
                            check(settings.navigate_on_select),
                            i18n.t("menu.branch.opt_nav_on_select")
                        ),
                        Some('n'),
                        None,
                        ContextMenuAction::BranchesToggleNavigateOnSelect,
                    ),
                ],
            ));
            items.push(ContextMenuItem::action(
                i18n.t("menu.branch.refresh_repo"),
                Some('r'),
                refresh_sc.as_deref(),
                ContextMenuAction::RefreshRepo,
            ));
            items
        }
        HitTarget::ChangesTreeNode { node_id, .. } => {
            let mut items = stage_tree_context_menu_items(state, node_id.0 as usize, actions);
            items.push(ContextMenuItem::submenu(
                state.i18n.t("menu.stage.git_probes"),
                Some('p'),
                probe_submenu_items(&state.i18n),
            ));
            items
        }
        HitTarget::ConsoleCommandRow { idx } => {
            if state.console_selected_idx != *idx {
                actions.push(Action::FocusPane(PaneId::ConsoleCommandList));
                actions.push(Action::SelectConsoleRow(*idx));
            }
            probe_submenu_items(&state.i18n)
        }
        _ => return None,
    };
    Some(items)
}

fn probe_submenu_items(i18n: &crate::i18n::I18n) -> Vec<ContextMenuItem> {
    vec![
        ContextMenuItem::action(
            i18n.t("menu.stage.probe_status"),
            Some('s'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec![
                "status".into(),
                "--short".into(),
                "--branch".into(),
            ]),
        ),
        ContextMenuItem::action(
            i18n.t("menu.stage.probe_branch"),
            Some('b'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec!["branch".into(), "-vv".into()]),
        ),
        ContextMenuItem::action(
            i18n.t("menu.stage.probe_log"),
            Some('l'),
            None,
            ContextMenuAction::RunReadOnlyProbe(vec![
                "log".into(),
                "-n".into(),
                "10".into(),
                "--oneline".into(),
            ]),
        ),
    ]
}

pub(crate) fn resolve_menu_action(act: ContextMenuAction) -> Vec<Action> {
    match act {
        ContextMenuAction::Run(actions) => actions,
        ContextMenuAction::RefreshRepo => vec![Action::RefreshAll],
        ContextMenuAction::RunReadOnlyProbe(args) => vec![Action::RunCliProbe(args)],
        ContextMenuAction::TogglePresentation(t) => vec![Action::TogglePresentation(t)],
        ContextMenuAction::SetGraphRenderer(mode) => vec![Action::SetGraphRenderer(mode)],
        ContextMenuAction::BranchesApplyFilter => vec![Action::BranchesApplyFilter],
        ContextMenuAction::BranchesNavigate => vec![Action::BranchesNavigate],
        ContextMenuAction::BranchesToggleFavorite => vec![Action::BranchesToggleFavorite],
        ContextMenuAction::BranchesToggleGroupByDir => vec![Action::BranchesToggleGroupByDir],
        ContextMenuAction::BranchesToggleShowTags => vec![Action::BranchesToggleShowTags],
        ContextMenuAction::BranchesToggleNavigateOnSelect => {
            vec![Action::BranchesToggleNavigateOnSelect]
        }
        ContextMenuAction::OpenGoTo => vec![Action::OpenPopup {
            kind: PopupKind::GoToRevision,
            initial_query: String::new(),
        }],
        ContextMenuAction::StageSelected => vec![Action::StageActionSelected],
        ContextMenuAction::UnstageSelected => vec![Action::UnstageActionSelected],
        ContextMenuAction::StageAllTracked => vec![Action::StageAllTracked],
        ContextMenuAction::DiscardSelected => vec![Action::DiscardPromptSelected],
        ContextMenuAction::StageToggleGroupByDir => vec![Action::StageToggleGroupByDir],
        ContextMenuAction::StageToggleShowIgnored => vec![Action::StageToggleShowIgnored],
        ContextMenuAction::StageExpandAll(expand) => vec![Action::StageExpandAll(expand)],
        ContextMenuAction::OpenExternalEditorFile(p) => {
            vec![Action::OpenExternalEditorFile(Some(p))]
        }
    }
}

/// `[⚙ Presentation]` menu (`Vcs.Log.PresentationSettings`).
pub fn presentation_menu_items(state: &AppState) -> Vec<ContextMenuItem> {
    use jbgit_core::graph::SortType;
    let check = |on: bool| if on { "✓" } else { " " };
    let p = &state.presentation;
    let o = &state.log_graph.options;
    let i18n = &state.i18n;
    let item =
        |on: bool, key: &str, mnemonic: char, shortcut: Option<&str>, t: PresentationToggle| {
            ContextMenuItem::action(
                format!("{} {}", check(on), i18n.t(key)),
                Some(mnemonic),
                shortcut,
                ContextMenuAction::TogglePresentation(t),
            )
        };
    vec![
        item(
            state.compact_refs,
            "presentation.compact_refs",
            'c',
            None,
            PresentationToggle::CompactRefs,
        ),
        item(
            state.show_tag_names,
            "presentation.show_tag_names",
            't',
            None,
            PresentationToggle::ShowTagNames,
        ),
        item(
            o.show_long_edges,
            "presentation.show_long_edges",
            'l',
            None,
            PresentationToggle::ShowLongEdges,
        ),
        item(
            o.sort_type == SortType::IntelliSort,
            "presentation.intelli_sort",
            'i',
            None,
            PresentationToggle::IntelliSort,
        ),
        item(
            o.first_parent_only,
            "presentation.first_parent_only",
            'f',
            None,
            PresentationToggle::FirstParentOnly,
        ),
        item(
            p.highlight_current_branch,
            "presentation.highlight_current_branch",
            'b',
            None,
            PresentationToggle::HighlightCurrentBranch,
        ),
        item(
            p.highlight_merges,
            "presentation.highlight_merges",
            'm',
            None,
            PresentationToggle::HighlightMerges,
        ),
        item(
            p.show_hash_column,
            "presentation.show_hash_column",
            'h',
            None,
            PresentationToggle::ShowHashColumn,
        ),
        item(
            p.relative_dates,
            "presentation.relative_dates",
            'r',
            None,
            PresentationToggle::RelativeDates,
        ),
        item(
            p.use_author_date,
            "presentation.use_author_date",
            'u',
            None,
            PresentationToggle::UseAuthorDate,
        ),
        ContextMenuItem::separator(),
        ContextMenuItem::submenu(
            i18n.t_fmt(
                "presentation.graph_renderer",
                &[("mode", i18n.graph_renderer_label(state.graph_renderer.mode()))],
            ),
            Some('g'),
            GraphRendererMode::ALL
                .into_iter()
                .map(|mode| {
                    let on = state.graph_renderer.mode() == mode;
                    ContextMenuItem::action(
                        format!(
                            "{} {}",
                            if on { "●" } else { "○" },
                            i18n.graph_renderer_label(mode)
                        ),
                        mode.label().chars().next().map(|c| c.to_ascii_lowercase()),
                        (mode == state.graph_renderer.mode().next()).then_some("F8"),
                        ContextMenuAction::SetGraphRenderer(mode),
                    )
                })
                .collect(),
        ),
    ]
}
