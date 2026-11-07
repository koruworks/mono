// SPDX-License-Identifier: MPL-2.0

//! **Where a dragged task lands, as numbers** — the list page's manual order.
//!
//! A task's place in its list is one integer on the task (`koru.rank`,
//! smaller first). A list page ordered by hand shows the ranked tasks first,
//! by rank, then the ones nobody has ranked, by due (the store's
//! [`TaskSort::Manual`](koru_store_client::TaskSort)). A drop gives the moved
//! tasks ranks that put them where they were dropped, writing as few tasks as
//! it can:
//!
//! - **between two neighbours**, when the gap between their ranks has room —
//!   the common drag, which writes only the tasks that moved;
//! - **otherwise every loaded row of the list is renumbered** in its new
//!   order, [`GAP`] apart — the first drag on a list nobody has ordered, a
//!   drop among unranked tasks, or a gap that has run out. Only rows whose
//!   rank changes are written.
//!
//! A neighbour with no rank below the drop is not a reason to renumber: the
//! unranked always sort after every ranked task, so a rank just past the one
//! above the drop already puts the moved tasks above it.

/// The distance between two neighbours when a list is numbered afresh: room
/// for twenty halvings before the next renumbering.
pub const GAP: i64 = 1 << 20;

/// The new order of a page after moving `moving` (indices into `ranks`, in
/// page order) to the gap before row `gap` (`0..=ranks.len()`, counted in the
/// page as it was).
pub fn reordered(len: usize, moving: &[usize], gap: usize) -> Vec<usize> {
    let rest: Vec<usize> = (0..len).filter(|i| !moving.contains(i)).collect();
    let at = rest.iter().filter(|&&i| i < gap).count();
    let mut order = rest[..at].to_vec();
    order.extend_from_slice(moving);
    order.extend_from_slice(&rest[at..]);
    order
}

/// The ranks to write for a drop: `(row, rank)` for every row whose rank
/// changes, rows named by their index in `ranks`. Empty when the drop moves
/// nothing.
pub fn assign(ranks: &[Option<i64>], moving: &[usize], gap: usize) -> Vec<(usize, i64)> {
    if moving.is_empty() {
        return Vec::new();
    }
    let order = reordered(ranks.len(), moving, gap);
    if order.iter().copied().eq(0..ranks.len()) {
        return Vec::new();
    }
    let at = order
        .iter()
        .position(|i| *i == moving[0])
        .expect("the moved rows are in the new order");
    let above = at.checked_sub(1).map(|i| ranks[order[i]]);
    let below = order.get(at + moving.len()).map(|&i| ranks[i]);
    let k = moving.len() as i64;
    let placed: Option<Vec<i64>> = match (above, below) {
        // Between two ranked neighbours: evenly, when there is room.
        (Some(Some(lo)), Some(Some(hi))) => {
            let step = (hi - lo) / (k + 1);
            (step >= 1).then(|| (1..=k).map(|j| lo + step * j).collect())
        }
        // Under a ranked row, with nothing ranked below: just past it.
        (Some(Some(lo)), None | Some(None)) => Some((1..=k).map(|j| lo + GAP * j).collect()),
        // At the top, over a ranked row.
        (None, Some(Some(hi))) => Some((0..k).map(|j| hi - GAP * (k - j)).collect()),
        // At the top of a page nobody has ranked: the moved rows are the
        // first ranked ones, and every other row follows by its due.
        (None, None | Some(None)) => Some((1..=k).map(|j| GAP * j).collect()),
        // Under a row with no rank: there is no number that says *here*.
        (Some(None), _) => None,
    };
    let changes: Vec<(usize, i64)> = match placed {
        Some(placed) => moving.iter().copied().zip(placed).collect(),
        None => order
            .iter()
            .enumerate()
            .map(|(j, &i)| (i, GAP * (j as i64 + 1)))
            .collect(),
    };
    changes
        .into_iter()
        .filter(|(i, rank)| ranks[*i] != Some(*rank))
        .collect()
}

/// The rank a task typed into a list ordered by hand takes: above every row
/// the page holds.
pub fn top(ranks: &[Option<i64>]) -> i64 {
    ranks.iter().flatten().min().map_or(0, |min| min - GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply `changes` and read the order a manual sort would show: ranked by
    /// rank, then the unranked in their old order.
    fn shown(ranks: &[Option<i64>], changes: &[(usize, i64)]) -> Vec<usize> {
        let mut after = ranks.to_vec();
        for &(i, rank) in changes {
            after[i] = Some(rank);
        }
        let mut order: Vec<usize> = (0..ranks.len()).collect();
        order.sort_by_key(|&i| (after[i].is_none(), after[i].unwrap_or(0)));
        order
    }

    #[test]
    fn a_drop_between_ranked_neighbours_writes_only_the_moved_row() {
        let ranks = [Some(GAP), Some(2 * GAP), Some(3 * GAP), Some(4 * GAP)];
        // Row 3 to the gap before row 1.
        let changes = assign(&ranks, &[3], 1);
        assert_eq!(changes.len(), 1);
        assert_eq!(shown(&ranks, &changes), [0, 3, 1, 2]);
    }

    #[test]
    fn the_first_drag_on_an_unranked_list_freezes_its_order() {
        let ranks = [None, None, None, None];
        // Row 0 to the end.
        let changes = assign(&ranks, &[0], 4);
        assert_eq!(changes.len(), 4, "every row is numbered once");
        assert_eq!(shown(&ranks, &changes), [1, 2, 3, 0]);
    }

    #[test]
    fn to_the_top_of_an_unranked_list_ranks_only_the_moved_rows() {
        let ranks = [None, None, None];
        let changes = assign(&ranks, &[2], 0);
        assert_eq!(changes, [(2, GAP)]);
        assert_eq!(shown(&ranks, &changes), [2, 0, 1]);
    }

    #[test]
    fn several_rows_move_together_keeping_their_order() {
        let ranks = [
            Some(GAP),
            Some(2 * GAP),
            Some(3 * GAP),
            Some(4 * GAP),
            Some(5 * GAP),
        ];
        // Rows 0 and 3 to the gap before row 2.
        let changes = assign(&ranks, &[0, 3], 2);
        assert_eq!(changes.len(), 2);
        assert_eq!(shown(&ranks, &changes), [1, 0, 3, 2, 4]);
    }

    #[test]
    fn a_gap_that_has_run_out_renumbers() {
        let ranks = [Some(10), Some(11), Some(12)];
        let changes = assign(&ranks, &[2], 1);
        assert_eq!(shown(&ranks, &changes), [0, 2, 1]);
        assert!(changes.len() > 1);
    }

    #[test]
    fn a_drop_into_the_unranked_tail_renumbers() {
        let ranks = [Some(GAP), Some(2 * GAP), None, None];
        // Row 0 to between the two unranked rows.
        let changes = assign(&ranks, &[0], 3);
        assert_eq!(shown(&ranks, &changes), [1, 2, 0, 3]);
    }

    #[test]
    fn a_drop_where_it_already_was_writes_nothing() {
        let ranks = [Some(GAP), Some(2 * GAP)];
        assert!(assign(&ranks, &[1], 1).is_empty());
        assert!(assign(&ranks, &[1], 2).is_empty());
    }

    #[test]
    fn a_typed_task_goes_on_top() {
        assert_eq!(top(&[None, None]), 0);
        assert_eq!(top(&[Some(5 * GAP), None, Some(2 * GAP)]), GAP);
    }
}
