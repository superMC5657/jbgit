use std::sync::Arc;
use std::time::Instant;

use jbgit_core::graph::SortType;
use jbgit_core::{
    BranchFilter, BranchNodeKind, ChangeRowKind, DetailsKey, FileHistoryKey, PaneId, PathFilter,
    RefKind, RefreshScope, WorkspaceTab,
};

use crate::log_view::{DETAILS_DEBOUNCE, FileHistoryState, PathRequest, PresentationToggle};

use super::{
    Action, AppState, ContainingBranchesJob, Effect, ModalLayer, PopupKind, ScrollPane,
    initial_menu_selection,
};

impl AppState {
    pub fn dispatch(&mut self, action: Action) -> Vec<Effect> {
        let mut effects = Vec::new();
        self.needs_redraw = true;
        self.ensure_graph_cache();
        let snap = self.repo_snapshot.clone();
        let width = self.terminal_area.width;

        match action {
            Action::Quit => {
                self.should_quit = true;
                effects.push(Effect::Quit);
            }
            Action::SwitchTab(tab) => {
                self.active_tab = tab;
                self.focused_pane = tab.default_pane();
                self.log_filter.editing_text = false;
                self.branches.searching = false;
                if tab != WorkspaceTab::Log {
                    self.details.diff_zoomed = false;
                }
                if tab == WorkspaceTab::Commit {
                    effects.push(Effect::RequestCommitPrecheck);
                }
            }
            Action::CycleFocus { reverse } => {
                let ring = self.effective_focus_ring();
                if let Some(pos) = ring.iter().position(|&p| p == self.focused_pane) {
                    let next = if reverse {
                        if pos == 0 { ring.len() - 1 } else { pos - 1 }
                    } else {
                        (pos + 1) % ring.len()
                    };
                    self.focused_pane = ring[next];
                } else if let Some(&first) = ring.first() {
                    self.focused_pane = first;
                }
                self.log_filter.editing_text = false;
                if self.focused_pane != PaneId::LogBranchesTree {
                    self.branches.searching = false;
                }
            }
            Action::FocusPane(pane) => {
                self.focused_pane = pane;
                self.log_filter.editing_text = false;
                if self.focused_pane != PaneId::LogBranchesTree {
                    self.branches.searching = false;
                }
            }
            Action::ToggleGlyphMode => {
                self.glyph_mode = self.glyph_mode.toggle();
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.glyph_mode", &[("mode", self.glyph_mode.label())]),
                );
            }
            Action::ToggleCompactRefs => {
                return self.dispatch(Action::TogglePresentation(PresentationToggle::CompactRefs));
            }
            Action::ToggleTagNames => {
                return self.dispatch(Action::TogglePresentation(PresentationToggle::ShowTagNames));
            }
            Action::RefreshAll => {
                self.loading_snapshot = true;
                effects.push(Effect::RefreshRepoAndStage);
            }
            Action::OpenPopup {
                kind,
                initial_query,
            } => {
                self.modal_stack.push(ModalLayer::FloatingPopup {
                    kind,
                    query: initial_query,
                    selected: 0,
                });
            }
            Action::OpenContextMenu {
                anchor,
                items,
                depth,
            } => {
                if !items.is_empty() {
                    let selected = initial_menu_selection(&items);
                    self.menu_hover = None;
                    self.modal_stack.push(ModalLayer::ContextMenu {
                        anchor,
                        items,
                        selected,
                        depth,
                        parent: None,
                    });
                }
            }
            Action::PopModal => {
                self.menu_hover = None;
                match self.modal_stack.pop() {
                    Some(ModalLayer::FloatingPopup {
                        kind: PopupKind::ThreeWayInspector,
                        ..
                    }) => {
                        self.stage_workbench.three_way_range_idx = None;
                    }
                    Some(ModalLayer::FloatingPopup {
                        kind: PopupKind::ConflictMerge,
                        ..
                    }) => {
                        self.conflict_merge = None;
                    }
                    Some(ModalLayer::FloatingPopup {
                        kind: PopupKind::FileHistory,
                        ..
                    }) => {
                        self.file_history = None;
                    }
                    _ => {}
                }
            }
            Action::PopContextMenus => {
                self.menu_hover = None;
                while matches!(
                    self.modal_stack.last(),
                    Some(ModalLayer::ContextMenu { .. })
                ) {
                    self.modal_stack.pop();
                }
            }
            Action::ConfirmCriticalDialog => {
                if let Some(ModalLayer::CriticalDialog { confirm_effect, .. }) =
                    self.modal_stack.pop()
                {
                    if let Some(eff) = confirm_effect {
                        if let Effect::DiscardTargets(targets) = &eff {
                            for t in targets {
                                self.stage_workbench.invalidate_diff_cache_for_path(&t.path);
                            }
                            self.stage_workbench.pending_select_immediate = true;
                        }
                        effects.push(eff);
                    } else {
                        self.loading_snapshot = true;
                        effects.push(Effect::RefreshRepoAndStage);
                    }
                }
            }
            Action::ResetSplitter(id) => {
                self.splitters.reset(id);
            }
            Action::SetGraphRenderer(mode) => {
                self.graph_renderer.set_mode(mode);
            }
            Action::CycleGraphRenderer => {
                let next = self.graph_renderer.mode().next();
                return self.dispatch(Action::SetGraphRenderer(next));
            }
            Action::TogglePresentation(t) => {
                self.toggle_presentation(t);
            }
            Action::FlushPendingDetails => {
                self.flush_pending_at(Instant::now(), &mut effects);
            }
            Action::SelectConsoleRow(idx) => {
                if !self.console_entries.is_empty() {
                    self.console_selected_idx = idx.min(self.console_entries.len() - 1);
                    self.console_output_scroll = 0;
                }
            }
            Action::ScrollConsoleList(delta) => {
                let len = self.console_entries.len();
                if len > 0 {
                    let next = (self.console_selected_idx as isize + delta)
                        .clamp(0, (len - 1) as isize) as usize;
                    if next != self.console_selected_idx {
                        self.console_selected_idx = next;
                        self.console_output_scroll = 0;
                    }
                }
            }
            Action::ScrollConsoleOutput(delta) => {
                self.scroll_pane(ScrollPane::ConsoleOutput, delta);
            }
            Action::RunCliProbe(args) => {
                effects.push(Effect::RunCli {
                    args,
                    stdin_payload: None,
                    is_read_only: true,
                    refresh_after: RefreshScope::None,
                });
            }
            Action::Resize | Action::TickSpinner => {
                if matches!(action, Action::TickSpinner) {
                    self.spinner_frame = self.spinner_frame.wrapping_add(1);
                }
            }
            other => {
                if !self.dispatch_branches_action(&other, snap.as_ref(), width)
                    && !self.dispatch_filter_action(&other)
                    && !self.dispatch_log_action(&other, snap.as_ref())
                    && !self.dispatch_details_action(&other, snap.as_ref(), &mut effects)
                    && !self.dispatch_file_history_action(&other, &mut effects)
                    && !self.dispatch_git_action(&other, &mut effects)
                {
                    let _ = self.dispatch_stage_action(&other, &mut effects);
                }
            }
        }

        self.reconcile_all_requests(&mut effects);
        effects
    }

    pub(crate) fn reconcile_all_requests(&mut self, effects: &mut Vec<Effect>) {
        let now = Instant::now();
        self.reconcile_log_requests(now, effects);
        self.reconcile_file_history_requests(now, effects);
        self.reconcile_stage_requests(now, effects);
        self.stash.reconcile_requests(
            self.repo_snapshot.as_ref(),
            self.active_tab == WorkspaceTab::Stash,
            effects,
        );
        self.reconcile_stash_syntax(effects);
    }

    fn toggle_presentation(&mut self, t: PresentationToggle) {
        let p = &mut self.presentation;
        let o = &mut self.log_graph.options;
        let (label_key, on) = match t {
            PresentationToggle::CompactRefs => {
                self.compact_refs = !self.compact_refs;
                ("presentation.compact_refs", self.compact_refs)
            }
            PresentationToggle::ShowTagNames => {
                self.show_tag_names = !self.show_tag_names;
                ("presentation.show_tag_names", self.show_tag_names)
            }
            PresentationToggle::ShowLongEdges => {
                o.show_long_edges = !o.show_long_edges;
                ("presentation.show_long_edges", o.show_long_edges)
            }
            PresentationToggle::IntelliSort => {
                o.sort_type = match o.sort_type {
                    SortType::IntelliSort => SortType::Topo,
                    SortType::Topo => SortType::IntelliSort,
                };
                (
                    "presentation.intelli_sort",
                    o.sort_type == SortType::IntelliSort,
                )
            }
            PresentationToggle::FirstParentOnly => {
                o.first_parent_only = !o.first_parent_only;
                ("presentation.first_parent_only", o.first_parent_only)
            }
            PresentationToggle::HighlightCurrentBranch => {
                p.highlight_current_branch = !p.highlight_current_branch;
                (
                    "presentation.highlight_current_branch",
                    p.highlight_current_branch,
                )
            }
            PresentationToggle::HighlightMerges => {
                p.highlight_merges = !p.highlight_merges;
                ("presentation.highlight_merges", p.highlight_merges)
            }
            PresentationToggle::ShowHashColumn => {
                p.show_hash_column = !p.show_hash_column;
                ("presentation.show_hash_column", p.show_hash_column)
            }
            PresentationToggle::RelativeDates => {
                p.relative_dates = !p.relative_dates;
                ("presentation.relative_dates", p.relative_dates)
            }
            PresentationToggle::UseAuthorDate => {
                p.use_author_date = !p.use_author_date;
                ("presentation.use_author_date", p.use_author_date)
            }
        };
        let label = self.i18n.t(label_key);
        let state = self.i18n.on_off(on);
        self.status_notice = Some(
            self.i18n
                .t_fmt("toast.presentation_toggle", &[("label", label), ("state", state)]),
        );
        self.ensure_graph_cache();
    }

    fn dispatch_branches_action(
        &mut self,
        action: &Action,
        snap: Option<&Arc<jbgit_core::RepoSnapshot>>,
        width: u16,
    ) -> bool {
        match action {
            Action::ToggleBranchesPane => {
                self.branches.toggle_collapsed(width);
                if self.branches.is_collapsed(width) {
                    if self.focused_pane == PaneId::LogBranchesTree {
                        self.focused_pane = PaneId::LogCommitTable;
                    }
                } else {
                    self.focused_pane = PaneId::LogBranchesTree;
                }
            }
            Action::SelectBranchRow(idx) => {
                if !self.branches.rows.is_empty() {
                    self.branches.selected = (*idx).min(self.branches.rows.len() - 1);
                    if self.branches.settings.navigate_on_select {
                        self.navigate_to_branch_node();
                    }
                }
            }
            Action::ScrollBranches(delta) => {
                self.branches.move_selection(*delta);
                if self.branches.settings.navigate_on_select {
                    self.navigate_to_branch_node();
                }
            }
            Action::BranchesExpand(expand) => {
                let expand = *expand;
                if let Some(row) = self.branches.selected_row() {
                    let node = &self.branches.model.nodes[row.node];
                    match row.expanded {
                        Some(is_open) if is_open != expand => {
                            let key = node.key.clone();
                            self.branches.settings.expansion.set(&key, expand);
                            self.branches.save_settings();
                            self.branches.refresh_rows();
                        }
                        Some(true) if expand => self.branches.move_selection(1),
                        _ if !expand => {
                            if let Some(parent) = node.parent {
                                let key = self.branches.model.nodes[parent].key.clone();
                                self.branches.select_key(&key);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Action::BranchesToggleRow(idx) => {
                if let Some(row) = self.branches.rows.get(*idx).copied()
                    && row.expanded.is_some()
                {
                    let key = self.branches.model.nodes[row.node].key.clone();
                    self.branches.settings.expansion.toggle(&key);
                    self.branches.save_settings();
                    self.branches.refresh_rows();
                    self.branches.select_key(&key);
                }
            }
            Action::BranchesToggleMulti(idx) => {
                if let Some(row) = self.branches.rows.get(*idx).copied() {
                    let key = self.branches.model.nodes[row.node].key.clone();
                    if self.branches.multi.is_empty()
                        && let Some(cur) = self.branches.selected_node()
                        && cur.key != key
                    {
                        let cur_key = cur.key.clone();
                        self.branches.toggle_multi(&cur_key);
                    }
                    self.branches.toggle_multi(&key);
                    self.branches.selected = *idx;
                }
            }
            Action::BranchesApplyFilter => {
                let names = self.branches.filter_ref_names();
                if !names.is_empty() {
                    self.log_filter.spec.branch = BranchFilter::from_names(names);
                    self.branches.multi.clear();
                    let label = self.log_filter.spec.branch.label();
                    self.status_notice = Some(
                        self.i18n
                            .t_fmt("toast.branch_filter_set", &[("label", &label)]),
                    );
                    self.on_filter_changed();
                }
            }
            Action::BranchesNavigate => self.navigate_to_branch_node(),
            Action::BranchesToggleFavorite => {
                if let Some(node) = self.branches.selected_node().cloned()
                    && let BranchNodeKind::Leaf(kind) = node.kind
                    && kind != RefKind::Tag
                {
                    let (full, short) = if kind == RefKind::Head {
                        match snap.and_then(|s| s.head_branch.clone()) {
                            Some(b) => (format!("refs/heads/{b}"), b),
                            None => return true,
                        }
                    } else {
                        let name = node.ref_name.clone().unwrap_or_default();
                        let short = if kind == RefKind::RemoteBranch {
                            name.split_once('/')
                                .map_or(name.clone(), |(_, s)| s.to_string())
                        } else {
                            name
                        };
                        (node.key.clone(), short)
                    };
                    let on = self.branches.settings.favorites.toggle(&full, &short);
                    self.branches.save_settings();
                    self.branches.sync_model(snap, true);
                    self.branches.select_key(&node.key);
                    self.status_notice = Some(self.i18n.t_fmt(
                        if on {
                            "toast.favorite_added"
                        } else {
                            "toast.favorite_removed"
                        },
                        &[("name", &short)],
                    ));
                }
            }
            Action::BranchesSearchStart => {
                self.focused_pane = PaneId::LogBranchesTree;
                self.branches.searching = true;
            }
            Action::BranchesSearchDone => {
                self.branches.searching = false;
            }
            Action::BranchesSearchClear => {
                self.branches.query.clear();
                self.branches.refresh_rows();
            }
            Action::BranchesSearchInput(ch) => {
                self.branches.query.push(*ch);
                self.branches.refresh_rows();
                if let Some(pos) = self.branches.rows.iter().position(|r| r.expanded.is_none()) {
                    self.branches.selected = pos;
                }
            }
            Action::BranchesSearchBackspace => {
                self.branches.query.pop();
                self.branches.refresh_rows();
            }
            Action::BranchesEscape => {
                self.branches.searching = false;
                if !self.branches.query.is_empty() {
                    let key = self.branches.selected_node().map(|n| n.key.clone());
                    self.branches.query.clear();
                    self.branches.refresh_rows();
                    if let Some(k) = key {
                        self.branches.select_key(&k);
                    }
                } else if !self.branches.multi.is_empty() {
                    self.branches.multi.clear();
                } else if !self.log_filter.spec.branch.is_empty() {
                    self.log_filter.spec.branch = BranchFilter::default();
                    self.status_notice =
                        Some(self.i18n.t("toast.branch_filter_cleared").to_string());
                    self.on_filter_changed();
                }
            }
            Action::BranchesToggleGroupByDir => {
                let o = &mut self.branches.settings.options;
                o.group_by_directory = !o.group_by_directory;
                self.branches.save_settings();
                self.branches.sync_model(snap, true);
            }
            Action::BranchesToggleShowTags => {
                let o = &mut self.branches.settings.options;
                o.show_tags = !o.show_tags;
                self.branches.save_settings();
                self.branches.sync_model(snap, true);
            }
            Action::BranchesToggleNavigateOnSelect => {
                let s = &mut self.branches.settings;
                s.navigate_on_select = !s.navigate_on_select;
                self.branches.save_settings();
                let st = self.i18n.on_off(self.branches.settings.navigate_on_select);
                self.status_notice = Some(self.i18n.t_fmt("toast.nav_on_select", &[("state", st)]));
            }
            _ => return false,
        }
        true
    }

    fn dispatch_filter_action(&mut self, action: &Action) -> bool {
        match action {
            Action::FocusFilterText => {
                self.focused_pane = PaneId::LogCommitTable;
                self.log_filter.editing_text = true;
            }
            Action::FilterTextInput(ch) => {
                self.log_filter.push_text_char(*ch, Instant::now());
            }
            Action::FilterTextBackspace => {
                self.log_filter.pop_text_char(Instant::now());
            }
            Action::FilterTextDone => {
                if self.log_filter.finish_text_edit() {
                    self.on_filter_changed();
                }
            }
            Action::FilterTextClear => {
                self.log_filter.spec.text.query.clear();
                self.on_filter_changed();
            }
            Action::ToggleFilterRegex => {
                self.log_filter.spec.text.regex = !self.log_filter.spec.text.regex;
                self.on_filter_changed();
            }
            Action::ToggleFilterCase => {
                self.log_filter.spec.text.match_case = !self.log_filter.spec.text.match_case;
                self.on_filter_changed();
            }
            Action::ApplyFilterField { field, expr } => {
                if let Err(err) = field.apply_i18n(
                    &mut self.log_filter.spec,
                    expr,
                    chrono::Local::now().timestamp(),
                    &self.i18n,
                ) {
                    self.status_notice = Some(err);
                    return true;
                }
                self.on_filter_changed();
            }
            Action::ClearFilterField(field) => {
                field.clear(&mut self.log_filter.spec);
                self.on_filter_changed();
            }
            Action::FilterLogByPath(path) => {
                self.active_tab = WorkspaceTab::Log;
                self.log_filter.spec.paths = PathFilter::single(path);
                self.log_filter.path_matches = None;
                self.log_filter.path_requested = None;
                self.on_filter_changed();
                if let Some(p) = self.log_filter.spec.paths.paths.first() {
                    self.status_notice =
                        Some(self.i18n.t_fmt("toast.path_filter_set", &[("path", p)]));
                }
            }
            Action::ClearAllFilters => {
                self.log_filter.spec = Default::default();
                self.on_filter_changed();
            }
            _ => return false,
        }
        true
    }

    fn dispatch_log_action(
        &mut self,
        action: &Action,
        snap: Option<&Arc<jbgit_core::RepoSnapshot>>,
    ) -> bool {
        match action {
            Action::SelectLogRow(idx) => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                self.log_graph.select_row(*idx);
            }
            Action::ExtendLogSelectionTo(idx) => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                self.log_graph.extend_selection_to(*idx);
            }
            Action::ScrollLog(delta) => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                self.log_graph.scroll_by(*delta, false);
            }
            Action::ExtendLogSelection(delta) => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                self.log_graph.scroll_by(*delta, true);
            }
            Action::LogPage { half, down } => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                let vh = self.log_graph.last_viewport_h.max(2);
                let step = if *half { vh / 2 } else { vh.saturating_sub(1) }.max(1) as isize;
                self.log_graph
                    .scroll_by(if *down { step } else { -step }, false);
            }
            Action::LogEdge { bottom } => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                let len = self.log_graph.visible_count();
                self.log_graph.jump_to(
                    snap,
                    if *bottom { len.saturating_sub(1) } else { 0 },
                    false,
                );
            }
            Action::JumpToLogRow { row, center } => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                self.log_graph.jump_to(snap, *row, *center);
            }
            Action::GoToParent(idx) => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                match self.log_graph.parent_row(snap, *idx) {
                    Some(row) => self.log_graph.jump_to(snap, row, false),
                    None => {
                        self.status_notice = Some(if *idx == 0 {
                            self.i18n.t("toast.no_visible_parent").to_string()
                        } else {
                            let n_s = (*idx + 1).to_string();
                            self.i18n.t_fmt("toast.no_visible_parent_n", &[("n", &n_s)])
                        });
                    }
                }
            }
            Action::GoToChild => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                match self.log_graph.child_row() {
                    Some(row) => self.log_graph.jump_to(snap, row, false),
                    None => {
                        self.status_notice = Some(self.i18n.t("toast.no_visible_child").to_string());
                    }
                }
            }
            Action::GoToRevision(expr) => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                let expr = expr.trim().to_string();
                match snap.and_then(|s| s.resolve_revision(&expr)) {
                    Some(oid) => {
                        let short = oid.short();
                        if self.log_graph.jump_to_oid(snap, &oid) {
                            self.focused_pane = PaneId::LogCommitTable;
                            self.status_notice = Some(
                                self.i18n
                                    .t_fmt("toast.goto_ok", &[("expr", &expr), ("short", &short)]),
                            );
                        } else {
                            self.status_notice = Some(
                                self.i18n
                                    .t_fmt("toast.goto_hidden", &[("expr", &expr), ("short", &short)]),
                            );
                        }
                    }
                    None => {
                        self.status_notice =
                            Some(self.i18n.t_fmt("toast.goto_not_found", &[("expr", &expr)]));
                    }
                }
            }
            Action::JumpToCommit(oid) => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                if self.log_graph.jump_to_oid(snap, oid) {
                    self.focused_pane = PaneId::LogCommitTable;
                } else {
                    let short = oid.short();
                    self.status_notice = Some(
                        self.i18n
                            .t_fmt("toast.commit_not_visible", &[("short", &short)]),
                    );
                }
            }
            Action::NavLogBack => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                self.log_graph.nav_back(snap);
            }
            Action::NavLogForward => {
                self.details.compare_override = None;
                self.details.pending_select_path = None;
                self.log_graph.nav_forward(snap);
            }
            _ => return false,
        }
        true
    }

    fn dispatch_details_action(
        &mut self,
        action: &Action,
        snap: Option<&Arc<jbgit_core::RepoSnapshot>>,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::ChangesMove(delta) => {
                let len = self.details.change_rows().len();
                if len > 0 {
                    self.details.changes_selected = (self.details.changes_selected as isize
                        + *delta)
                        .clamp(0, len as isize - 1)
                        as usize;
                    self.details.diff_scroll = 0;
                }
            }
            Action::ChangesSelectRow(idx) => {
                let len = self.details.change_rows().len();
                if len > 0 {
                    self.details.changes_selected = (*idx).min(len - 1);
                    self.details.diff_scroll = 0;
                }
            }
            Action::ChangesExpand(expand) => {
                let rows = self.details.change_rows();
                if let Some(row) = rows.get(self.details.changes_selected)
                    && matches!(row.kind, ChangeRowKind::Dir { .. })
                {
                    if *expand {
                        self.details.changes_collapsed.remove(&row.key);
                    } else {
                        self.details.changes_collapsed.insert(row.key.clone());
                    }
                }
            }
            Action::ChangesToggleGroupByDir => {
                self.details.changes_group_by_dir = !self.details.changes_group_by_dir;
                self.details.changes_selected = 0;
            }
            Action::ChangesExpandAll(expand) => {
                if *expand {
                    self.details.changes_collapsed.clear();
                } else if let Some(bundle) = self.details.current_bundle() {
                    let all = jbgit_core::build_change_rows(
                        &bundle.changes,
                        true,
                        &rustc_hash::FxHashSet::default(),
                    );
                    self.details.changes_collapsed = all
                        .into_iter()
                        .filter(|r| r.depth == 0 && matches!(r.kind, ChangeRowKind::Dir { .. }))
                        .map(|r| r.key)
                        .collect();
                    self.details.changes_selected = 0;
                }
            }
            Action::SwitchMergeParent => {
                let oids = self.log_graph.selected_oids(snap);
                let parents = match oids.as_slice() {
                    [one] => self
                        .log_graph
                        .commit_at(snap, self.log_graph.selected_idx)
                        .filter(|c| c.oid == *one)
                        .map_or(0, |c| c.parents.len()),
                    _ => 0,
                };
                if parents >= 2 {
                    self.details.merge_parent = (self.details.merge_parent + 1) % parents;
                    let cur_s = (self.details.merge_parent + 1).to_string();
                    let tot_s = parents.to_string();
                    self.status_notice = Some(self.i18n.t_fmt(
                        "toast.merge_parent_switched",
                        &[("cur", &cur_s), ("total", &tot_s)],
                    ));
                } else {
                    self.status_notice = Some(self.i18n.t("toast.not_merge_commit").to_string());
                }
            }
            Action::ToggleDiffPreview => {
                self.details.diff_preview = !self.details.diff_preview;
                self.details.diff_scroll = 0;
            }
            Action::DiffCycleDock => {
                self.details.diff_dock = self.details.diff_dock.next();
                let dock_lbl = self.i18n.diff_dock_label(self.details.diff_dock);
                self.status_notice =
                    Some(self.i18n.t_fmt("toast.diff_dock_set", &[("dock", dock_lbl)]));
            }
            Action::ZoomDiff(on) => {
                self.details.diff_zoomed = *on && self.details.selected_change().is_some();
                self.details.diff_scroll = 0;
            }
            Action::DiffNextFile(delta) => {
                self.details.select_next_file(*delta);
            }
            Action::DiffJumpHunk(delta) => {
                self.details.jump_diff_hunk(*delta);
            }
            Action::DiffToggleSideBySide => {
                self.details.diff_side_by_side = !self.details.diff_side_by_side;
            }
            Action::DiffCycleContext => {
                self.details.diff_context = self.details.diff_context.next();
                let ctx_lbl = self.i18n.diff_context_label(self.details.diff_context);
                self.status_notice = Some(
                    self.i18n
                        .t_fmt("toast.diff_context_set", &[("ctx", ctx_lbl)]),
                );
            }
            Action::ScrollDiff(delta) => {
                self.scroll_pane(ScrollPane::LogDiff, *delta);
            }
            Action::ScrollDetails(delta) => {
                self.scroll_pane(ScrollPane::LogDetails, *delta);
            }
            Action::CopyText(text) => {
                self.status_notice = Some(self.i18n.t_fmt("toast.copied_text", &[("text", text)]));
                effects.push(Effect::CopyToClipboard(text.clone()));
            }
            _ => return false,
        }
        true
    }

    /// Derives background requests from the current Log selection:
    /// debounced details (LRU hit → 0 ms), containing refs, diff preview,
    /// large-graph rebuilds and path-filter matches.
    pub fn reconcile_log_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        self.ensure_graph_cache();
        let snap = self.repo_snapshot.clone();
        if self.uses_async_graph() {
            let path_matches = self
                .log_filter
                .path_matches
                .as_ref()
                .filter(|pm| {
                    snap.as_ref().is_some_and(|s| {
                        pm.paths == self.log_filter.spec.paths.paths
                            && Arc::ptr_eq(&pm.snapshot, s)
                    })
                })
                .map(|pm| Arc::clone(&pm.oids));
            if let Some(job) = self.log_graph.take_async_rebuild_job(
                snap.as_ref(),
                &self.branch_colors,
                &self.log_filter.spec,
                path_matches,
            ) {
                effects.push(Effect::RebuildLogGraph(Box::new(job)));
            }
        }

        if let Some(s) = &snap
            && self.log_filter.path_matches_pending(Some(s))
        {
            let paths = self.log_filter.spec.paths.paths.clone();
            let already = self
                .log_filter
                .path_requested
                .as_ref()
                .is_some_and(|req| req.paths == paths && Arc::ptr_eq(&req.snapshot, s));
            if !already {
                self.log_filter.path_requested = Some(PathRequest {
                    paths: paths.clone(),
                    snapshot: Arc::clone(s),
                });
                effects.push(Effect::RequestPathMatches(paths, Arc::clone(s)));
            }
        }

        let desired = if let Some(cmp) = &self.details.compare_override {
            Some(cmp.clone())
        } else {
            let oids = self.log_graph.selected_oids(snap.as_ref());
            if self.details.current_key.as_ref().map(|k| &k.oids) != Some(&oids) {
                self.details.merge_parent = 0;
            }
            let parent_index = if oids.len() == 1 {
                self.details.merge_parent
            } else {
                0
            };
            (!oids.is_empty()).then_some(DetailsKey::commits(oids, parent_index))
        };
        if desired != self.details.current_key {
            self.details.current_key = desired.clone();
            self.details.changes_selected = 0;
            self.details.details_scroll = 0;
            self.details.diff_scroll = 0;
            self.details.pending = match desired {
                Some(k) if self.details.cache.get(&k).is_some() => None,
                Some(k) if self.details.compare_override.is_some() => {
                    effects.push(Effect::RequestCommitDetails(k));
                    None
                }
                Some(k) => Some((k, now + DETAILS_DEBOUNCE)),
                None => None,
            };
            if self.details.current_bundle().is_none() {
                self.details.diff_zoomed = false;
            }
        }

        if let Some((path, found)) = self.details.try_select_pending_path()
            && !found
        {
            self.status_notice = Some(
                self.i18n
                    .t_fmt("toast.compare_local_no_diff", &[("path", &path)]),
            );
        }

        if let Some(key) = &self.details.current_key
            && let [oid] = key.oids.as_slice()
            && let (Some(s), Some(cached)) = (&snap, &self.log_graph.cached)
            && let Some(node) = cached.permanent.linear.node_of_oid(oid)
        {
            let oid = *oid;
            if let Some(token) = self.details.request_containing_branches(oid) {
                effects.push(Effect::ComputeContainingBranches(ContainingBranchesJob {
                    oid,
                    node,
                    token,
                    graph: Arc::clone(&cached.permanent),
                    snapshot: Arc::clone(s),
                }));
            }
        }

        if self.details.diff_visible()
            && let Some(key) = self.details.desired_diff_key()
        {
            if let Some(cached) = self.details.diff_cache.get(&key).cloned() {
                if self.details.diff_syntax.get(&key).is_none() {
                    self.schedule_file_syntax(cached, effects);
                }
            } else if self.details.requested_diff.as_ref() != Some(&key) {
                if self.file_syntax.cancel.is_some() {
                    self.file_syntax.invalidate();
                }
                self.details.requested_diff = Some(key.clone());
                effects.push(Effect::RequestFileDiff(key));
            }
        }
    }

    fn close_file_history(&mut self) {
        self.file_history = None;
        if matches!(
            self.modal_stack.last(),
            Some(ModalLayer::FloatingPopup {
                kind: PopupKind::FileHistory,
                ..
            })
        ) {
            self.modal_stack.pop();
        }
    }

    pub(crate) fn jump_file_history_to_log(&mut self, effects: &mut Vec<Effect>) {
        let target_oid = self
            .file_history
            .as_ref()
            .and_then(|fh| fh.selected_entry())
            .map(|e| e.oid);
        if let Some(oid) = target_oid {
            self.close_file_history();
            self.active_tab = WorkspaceTab::Log;
            effects.extend(self.dispatch(Action::JumpToCommit(oid)));
        }
    }

    fn dispatch_file_history_action(
        &mut self,
        action: &Action,
        effects: &mut Vec<Effect>,
    ) -> bool {
        match action {
            Action::OpenFileHistory {
                path,
                start_oid,
                follow_renames,
            } => {
                let norm = path.replace('\\', "/");
                if norm.is_empty() {
                    return true;
                }
                let key = FileHistoryKey {
                    path: norm,
                    start_oid: *start_oid,
                    follow_renames: *follow_renames,
                };
                let mut fh = FileHistoryState::new(key.clone(), self.details.diff_context);
                fh.diff_side_by_side = self.details.diff_side_by_side;
                self.file_history = Some(fh);
                self.modal_stack.retain(|l| {
                    !matches!(
                        l,
                        ModalLayer::ContextMenu { .. }
                            | ModalLayer::FloatingPopup {
                                kind: PopupKind::FileHistory,
                                ..
                            }
                    )
                });
                self.modal_stack.push(ModalLayer::FloatingPopup {
                    kind: PopupKind::FileHistory,
                    query: String::new(),
                    selected: 0,
                });
                effects.push(Effect::RequestFileHistory(key));
            }
            Action::FileHistoryMove { delta, extend } => {
                if let Some(fh) = &mut self.file_history {
                    fh.move_selection(*delta, *extend);
                }
            }
            Action::FileHistorySelect { idx, extend } => {
                if let Some(fh) = &mut self.file_history {
                    fh.select_row(*idx, *extend);
                }
            }
            Action::FileHistoryToggleFollow => {
                if let Some(fh) = &mut self.file_history {
                    effects.push(Effect::RequestFileHistory(fh.toggle_follow()));
                }
            }
            Action::FileHistoryToggleCompareLocal => {
                if let Some(fh) = &mut self.file_history {
                    fh.toggle_compare_local();
                }
            }
            Action::FileHistoryToggleSideBySide => {
                if let Some(fh) = &mut self.file_history {
                    fh.toggle_side_by_side();
                }
            }
            Action::FileHistoryCycleContext => {
                if let Some(fh) = &mut self.file_history {
                    fh.cycle_context();
                }
            }
            Action::FileHistoryToggleZoom => {
                if let Some(fh) = &mut self.file_history {
                    fh.toggle_zoom();
                }
            }
            Action::FileHistorySetFocus(focus) => {
                if let Some(fh) = &mut self.file_history {
                    fh.set_focus(*focus);
                }
            }
            Action::FileHistoryToggleFocus => {
                if let Some(fh) = &mut self.file_history {
                    fh.toggle_focus();
                }
            }
            Action::FileHistoryScrollDiff(delta) => {
                self.scroll_pane(ScrollPane::FileHistoryDiff, *delta);
            }
            Action::FileHistoryJumpToLog => self.jump_file_history_to_log(effects),
            Action::FileHistoryEscape => {
                if self.file_history.as_ref().is_some_and(|fh| fh.diff_zoomed) {
                    if let Some(fh) = &mut self.file_history {
                        fh.toggle_zoom();
                    }
                } else {
                    self.close_file_history();
                }
            }
            _ => return false,
        }
        true
    }

    /// Schedules debounced diff fetches (0 ms on LRU cache hit) for `PopupKind::FileHistory`.
    pub fn reconcile_file_history_requests(&mut self, now: Instant, effects: &mut Vec<Effect>) {
        let desired_key = match &mut self.file_history {
            Some(fh) => match fh.desired_diff_key() {
                Some(k) => k,
                None => {
                    fh.pending_diff = None;
                    return;
                }
            },
            None => return,
        };
        if let Some(cached) = self.details.diff_cache.get(&desired_key).cloned() {
            if let Some(fh) = &mut self.file_history {
                fh.pending_diff = None;
            }
            if self.details.diff_syntax.get(&desired_key).is_none() {
                self.schedule_file_syntax(cached, effects);
            }
        } else if let Some(fh) = &mut self.file_history
            && fh.requested_diff.as_ref() != Some(&desired_key)
            && fh.pending_diff.as_ref().map(|(k, _)| k) != Some(&desired_key)
        {
            fh.pending_diff = Some((desired_key, now + DETAILS_DEBOUNCE));
        }
    }
}
