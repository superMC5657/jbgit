//! Shared index/position navigation helpers with explicit wrap-around control.

/// Steps `current` by `delta` within `0..len`, wrapping when `wrap` is true
/// and clamping to `0..len` otherwise.
pub fn step_index(current: usize, len: usize, delta: isize, wrap: bool) -> usize {
    if len == 0 {
        return 0;
    }
    let cur = current.min(len - 1) as isize;
    let next = cur.saturating_add(delta);
    if wrap {
        next.rem_euclid(len as isize) as usize
    } else {
        next.clamp(0, len as isize - 1) as usize
    }
}

/// Steps `from` across selectable entries in `items` by `delta.unsigned_abs()`
/// hops in the direction of `delta`, either wrapping or stopping at the ends.
pub fn step_selectable<T>(
    items: &[T],
    from: usize,
    delta: isize,
    wrap: bool,
    is_selectable: impl Fn(&T) -> bool,
) -> usize {
    let n = items.len();
    if n == 0 {
        return 0;
    }
    let mut idx = from.min(n - 1);
    if delta == 0 {
        return idx;
    }
    let forward = delta > 0;
    for _ in 0..delta.unsigned_abs() {
        let next = if forward {
            ((idx + 1)..n)
                .find(|&i| is_selectable(&items[i]))
                .or_else(|| {
                    wrap.then(|| (0..=idx).find(|&i| is_selectable(&items[i])))
                        .flatten()
                })
        } else {
            (0..idx)
                .rev()
                .find(|&i| is_selectable(&items[i]))
                .or_else(|| {
                    wrap.then(|| (idx..n).rev().find(|&i| is_selectable(&items[i])))
                        .flatten()
                })
        };
        let Some(next) = next else {
            break;
        };
        idx = next;
    }
    idx
}

/// Steps to the next or previous position in sorted `positions`, clamping at ends.
/// When `current` is not in `positions` (e.g. a directory header row), picks the
/// nearest position in direction `delta`, clamping to the last/first position.
pub fn step_sorted_position(positions: &[usize], current: usize, delta: isize) -> Option<usize> {
    if positions.is_empty() {
        return None;
    }
    let next_idx = match positions.iter().position(|&p| p == current) {
        Some(i) => step_index(i, positions.len(), delta, false),
        None if delta >= 0 => positions
            .iter()
            .position(|&p| p > current)
            .unwrap_or(positions.len() - 1),
        None => positions.iter().rposition(|&p| p < current).unwrap_or(0),
    };
    Some(positions[next_idx])
}

/// Returns the strictly next (`delta >= 0`) or strictly previous (`delta < 0`)
/// sorted anchor offset relative to `current`, or `None` at the boundary.
pub fn jump_sorted_anchor(anchors: &[usize], current: usize, delta: isize) -> Option<usize> {
    if delta >= 0 {
        anchors.iter().copied().find(|&a| a > current)
    } else {
        anchors.iter().copied().rev().find(|&a| a < current)
    }
}
