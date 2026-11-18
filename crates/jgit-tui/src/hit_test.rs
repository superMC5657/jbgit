use jgit_core::{
    BranchTreeNodeId, ChangeNodeId, ModalButton, Oid, PaneId, SplitAxis, SplitterId, TreeId,
    WorkspaceTab,
};
use ratatui::layout::Rect;

use crate::log_view::FilterField;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchesToolbarButton {
    /// `[«]` collapse the Branches pane.
    Collapse,
    /// `[⌖]` navigate the log to the selected branch.
    Locate,
    /// `[⊞]` toggle Group by Directory.
    GroupByDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesToolbarButton {
    GroupByDir,
    ExpandAll,
    CollapseAll,
    DiffPreview,
    SwitchParent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffToolbarButton {
    SideBySide,
    Context,
    Dock,
    Zoom,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphHitKind {
    Node {
        commit_oid: Oid,
    },
    Arrow {
        up_row: usize,
        down_row: usize,
        target_row: usize,
        target_oid: Oid,
        is_down: bool,
    },
    Edge {
        up_row: usize,
        down_row: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    ToolTab(WorkspaceTab),
    BranchWidgetPill,
    HeaderRefreshButton,
    HeaderGlyphButton,
    HeaderHelpButton,
    Splitter {
        id: SplitterId,
        axis: SplitAxis,
    },
    Pane(PaneId),
    BranchTreeNode {
        pane: PaneId,
        node_id: BranchTreeNodeId,
        row: usize,
    },
    /// `▼` / `▶` expander of a Branches tree folder row (row index).
    BranchTreeToggle {
        row: usize,
    },
    BranchesToolbar(BranchesToolbarButton),
    /// 2-column `»` stripe shown while the Branches pane is collapsed.
    BranchesExpandStripe,
    FilterTextInput,
    FilterRegexToggle,
    FilterCaseToggle,
    FilterChip(FilterField),
    FilterClear(FilterField),
    FilterPresentation,
    LogChangeRow {
        idx: usize,
    },
    ChangesToolbar(ChangesToolbarButton),
    DetailsHash {
        oid: Oid,
    },
    DetailsParent {
        oid: Oid,
    },
    DetailsArea,
    DiffToolbar(DiffToolbarButton),
    DiffArea,
    GraphPrintElement {
        visible_row: usize,
        element_idx: usize,
        commit_oid: Oid,
        kind: GraphHitKind,
    },
    CommitRefPill {
        visible_row: usize,
        ref_name: String,
        is_detached_head: bool,
    },
    /// Trailing `+N` pill summarising refs that did not fit the row.
    CommitRefOverflow {
        visible_row: usize,
        hidden_refs: Vec<String>,
    },
    CommitTableRow {
        visible_row: usize,
        commit_oid: Oid,
    },
    ChangesTreeNode {
        tree_id: TreeId,
        node_id: ChangeNodeId,
        on_hover_action_icon: bool,
    },
    ConsoleCommandRow {
        idx: usize,
    },
    ConsoleOutputArea,
    MenuItem {
        menu_depth: u8,
        item_idx: usize,
    },
    ModalBackdrop {
        layer_z: u8,
    },
    ModalButton(ModalButton),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitRegion {
    pub rect: Rect,
    pub z_index: u8,
    pub target: HitTarget,
}

impl HitRegion {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.rect.width > 0
            && self.rect.height > 0
            && col >= self.rect.x
            && col < self.rect.x.saturating_add(self.rect.width)
            && row >= self.rect.y
            && row < self.rect.y.saturating_add(self.rect.height)
    }
}

/// Immediate-mode spatial hit-test registry populated during every render pass.
/// Resolves hits by descending `z_index`, breaking ties in favor of later-registered
/// (child/foreground) regions.
#[derive(Debug, Default)]
pub struct HitTestRegistry {
    regions: Vec<HitRegion>,
}

impl HitTestRegistry {
    pub fn new() -> Self {
        Self {
            regions: Vec::with_capacity(128),
        }
    }

    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn register(&mut self, rect: Rect, z_index: u8, target: HitTarget) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        self.regions.push(HitRegion {
            rect,
            z_index,
            target,
        });
    }

    /// Returns the topmost region containing `(col, row)`:
    /// highest `z_index` wins; ties resolve to the last-registered region.
    pub fn hit_test(&self, col: u16, row: u16) -> Option<&HitRegion> {
        let mut best: Option<(usize, &HitRegion)> = None;
        for (order, region) in self.regions.iter().enumerate() {
            if !region.contains(col, row) {
                continue;
            }
            match best {
                None => best = Some((order, region)),
                Some((best_order, best_reg)) => {
                    if region.z_index > best_reg.z_index
                        || (region.z_index == best_reg.z_index && order > best_order)
                    {
                        best = Some((order, region));
                    }
                }
            }
        }
        best.map(|(_, reg)| reg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_index_and_registration_order_occlusion() {
        let mut reg = HitTestRegistry::new();

        // Layer 1: background pane + underlying table row
        reg.register(
            Rect::new(0, 0, 80, 24),
            1,
            HitTarget::Pane(PaneId::LogCommitTable),
        );
        reg.register(
            Rect::new(2, 5, 60, 1),
            1,
            HitTarget::CommitTableRow {
                visible_row: 3,
                commit_oid: Oid::ZERO,
            },
        );

        // Same z_index: child row overrides parent pane
        let hit_row = reg.hit_test(10, 5).unwrap();
        assert_eq!(
            hit_row.target,
            HitTarget::CommitTableRow {
                visible_row: 3,
                commit_oid: Oid::ZERO,
            }
        );

        // Layer 3: modal backdrop + menu item occludes underlying Layer 1 row even if registered later
        reg.register(
            Rect::new(0, 0, 80, 24),
            30,
            HitTarget::ModalBackdrop { layer_z: 30 },
        );
        reg.register(
            Rect::new(8, 4, 20, 1),
            31,
            HitTarget::MenuItem {
                menu_depth: 0,
                item_idx: 0,
            },
        );

        // Even if another Layer 1 item is registered after, Layer 3 still occludes it
        reg.register(
            Rect::new(2, 5, 60, 1),
            1,
            HitTarget::CommitTableRow {
                visible_row: 4,
                commit_oid: Oid::ZERO,
            },
        );

        let occluded = reg.hit_test(10, 4).unwrap();
        assert_eq!(
            occluded.target,
            HitTarget::MenuItem {
                menu_depth: 0,
                item_idx: 0
            }
        );

        let backdrop_hit = reg.hit_test(10, 5).unwrap();
        assert_eq!(
            backdrop_hit.target,
            HitTarget::ModalBackdrop { layer_z: 30 }
        );
    }
}
