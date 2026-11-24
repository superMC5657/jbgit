mod context_menu;
mod popups;
mod tooltip;
mod which_key;

use ratatui::Frame;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

use crate::app::{AppState, ModalLayer, RenderContext};
use crate::hit_test::HitTarget;

pub use context_menu::{context_menu_rect, menu_item_row_y, mnemonic_spans};
pub use tooltip::render_hover_tooltip;
pub use which_key::render_which_key_popup;

pub fn render_modals(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    ctx: &mut RenderContext<'_>,
) {
    let top_layer2_idx = state.modal_stack.iter().rposition(|l| l.z_level() == 20);
    for (idx, layer) in state.modal_stack.iter().enumerate() {
        let z = layer.z_level();
        if z == 20 && top_layer2_idx.is_some_and(|top| idx < top) {
            let mut scratch_hits = crate::hit_test::HitTestRegistry::default();
            let mut sub_ctx = ctx.rebind_hits(&mut scratch_hits);
            render_modal_layer(frame, screen, state, layer, &mut sub_ctx, z, false);
        } else {
            render_modal_layer(frame, screen, state, layer, ctx, z, true);
        }
    }
}

fn render_modal_layer(
    frame: &mut Frame<'_>,
    screen: Rect,
    state: &AppState,
    layer: &ModalLayer,
    ctx: &mut RenderContext<'_>,
    z: u8,
    register_backdrop: bool,
) {
    // Register full-screen backdrop so lower-layer widgets cannot receive clicks
    // (cascading submenus share their root menu's backdrop so the parent
    // menu stays hoverable while a child is open).
    if register_backdrop && !matches!(layer, ModalLayer::ContextMenu { depth, .. } if *depth > 0) {
        ctx.hit_registry
            .register(screen, z, HitTarget::ModalBackdrop { layer_z: z });
    }

    match layer {
        ModalLayer::BranchesPopup { query, selected } => {
            crate::branches_popup::render_branches_popup(
                frame, screen, state, ctx, query, *selected, z,
            );
        }
        ModalLayer::ShortcutsHelp { .. }
        | ModalLayer::PalettePreview
        | ModalLayer::FilterEditor { .. }
        | ModalLayer::GoToRevision { .. }
        | ModalLayer::ChooseParent { .. }
        | ModalLayer::ChooseChild { .. }
        | ModalLayer::CommitHistory { .. } => {
            let kind = layer.popup_kind().expect("popup kind");
            let (query, selected) = layer.popup_query_and_selected().unwrap_or(("", 0));
            popups::render_floating_popup(frame, screen, state, ctx, kind, query, selected, z);
        }
        ModalLayer::CommitMessage {
            oid,
            subject,
            message,
            scroll,
        } => {
            popups::render_commit_message_modal(
                frame,
                screen,
                state,
                ctx,
                *oid,
                subject,
                message,
                scroll.offset(),
                z,
            );
        }
        ModalLayer::Reflog(rf) => {
            crate::views::reflog::render_reflog_modal(frame, screen, state, rf, ctx, z);
        }
        ModalLayer::FileHistory(fh) => {
            crate::views::file_history::render_file_history_modal(frame, screen, state, fh, ctx, z);
        }
        ModalLayer::Annotate(ann) => {
            crate::views::annotate::render_annotate_modal(frame, screen, state, ann, ctx, z);
        }
        ModalLayer::PushDialog(pd) => {
            crate::views::push_dialog::render_push_dialog_modal(frame, screen, state, pd, ctx, z);
        }
        ModalLayer::Remotes(rs) => {
            crate::remotes_view::render_remotes_modal(frame, screen, state, rs, ctx, z);
        }
        ModalLayer::RepoSwitcher(rs) => {
            crate::repo_switcher::render_repo_switcher_modal(frame, screen, state, rs, ctx, z);
        }
        ModalLayer::Worktrees(ws) => {
            crate::worktrees_view::render_worktrees_modal(frame, screen, state, ws, ctx, z);
        }
        ModalLayer::ApplyPatch(s) => {
            crate::patch_view::render_apply_patch_modal(frame, screen, s, ctx, z);
        }
        ModalLayer::TreeBrowser(s) => {
            crate::tree_browser::render_tree_browser(frame, screen, s, ctx, z);
        }
        ModalLayer::ConflictResolver(_) => {
            crate::views::conflict_merge::render_conflict_merge_modal(
                frame, screen, state, ctx, z,
            );
        }
        ModalLayer::ConflictQueue(cq) => {
            crate::views::conflict_queue::render_conflict_queue_modal(
                frame, screen, state, cq, ctx, z,
            );
        }
        ModalLayer::ThreeWayInspector { range_idx } => {
            popups::render_three_way_inspector_modal(frame, screen, state, ctx, *range_idx, z);
        }
        ModalLayer::HookError { output } => {
            popups::render_hook_error_modal(frame, screen, ctx, output, z);
        }
        ModalLayer::ContextMenu {
            anchor,
            items,
            selected,
            depth,
            parent,
        } => context_menu::render_context_menu(
            frame, screen, ctx, *anchor, *parent, items, *selected, *depth, z,
        ),
        ModalLayer::Dialog(d) => {
            crate::views::dialogs::render_dialog(frame, screen, state, ctx, d, z);
        }
    }
}

pub(super) fn truncate_display_width(text: &str, max_w: usize) -> String {
    if text.width() <= max_w {
        return text.to_string();
    }
    if max_w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    let limit = max_w.saturating_sub(1);
    for ch in text.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > limit {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Darkens the cells under a 1-cell drop shadow (right column + bottom row).
pub fn draw_shadow(frame: &mut Frame<'_>, screen: Rect, rect: Rect) {
    let shade = ratatui::style::Color::Rgb(14, 14, 16);
    let dim = ratatui::style::Color::Rgb(70, 72, 76);
    let buf = frame.buffer_mut();
    let sx = rect.right();
    if sx < screen.right() {
        for y in rect.y.saturating_add(1)..rect.bottom().saturating_add(1).min(screen.bottom()) {
            if let Some(cell) = buf.cell_mut((sx, y)) {
                cell.set_bg(shade).set_fg(dim);
            }
        }
    }
    let sy = rect.bottom();
    if sy < screen.bottom() {
        for x in rect.x.saturating_add(1)..rect.right().min(screen.right()) {
            if let Some(cell) = buf.cell_mut((x, sy)) {
                cell.set_bg(shade).set_fg(dim);
            }
        }
    }
}
