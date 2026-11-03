// SPDX-License-Identifier: MPL-2.0

use super::*;
use NavigatorMode::*;

/// Nothing saved, or nothing readable, is the default order — and the
/// default order writes no line.
#[test]
fn an_empty_or_unreadable_line_is_the_default_order() {
    for line in ["", " ", ",,", "nonsense"] {
        assert_eq!(RailOrder::parse(line), RailOrder::default(), "{line:?}");
    }
    assert_eq!(RailOrder::default().to_storage(), "");
}

/// **The merge.** The views the line names keep its order; a token this build
/// does not know is dropped; a view the line does not name — newer than the
/// line — is appended where [`NavigatorMode::ALL`] puts it relative to the
/// other missing ones; a repeat counts once.
#[test]
fn saved_order_wins_unknown_tokens_drop_and_new_views_append_in_default_order() {
    let order = RailOrder::parse("chats, documents,from-a-newer-build,tasks,chats");
    let mut want = vec![Chats, Documents, Tasks];
    want.extend(
        NavigatorMode::ALL
            .into_iter()
            .filter(|m| !matches!(m, Chats | Documents | Tasks)),
    );
    assert_eq!(order.0, want);
    // Every view this build knows, once.
    assert_eq!(order.0.len(), NavigatorMode::ALL.len());
}

/// What is written reads back as the same order.
#[test]
fn a_saved_order_round_trips() {
    let order = RailOrder::parse("files,chats,documents");
    let line = order.to_storage();
    assert!(line.starts_with("files,chats,documents,"), "{line}");
    assert_eq!(RailOrder::parse(&line), order);
}

/// **Home stays hidden, wherever the line puts it.** It is in the stored
/// order — so a build that shows it would keep its place — but never among
/// the views this build draws.
#[test]
fn home_is_never_shown_however_the_line_orders_it() {
    koru_features::testing::with(koru_features::Features::ALL_ON, || {
        let order = RailOrder::parse("home,chats,documents");
        assert!(!order.visible().contains(&Home), "{:?}", order.visible());
        assert_eq!(&order.visible()[..2], &[Chats, Documents]);
    });
}

/// A move permutes the shown views among the slots they hold; a hidden view
/// keeps its own slot, and a list that is not a permutation is refused.
#[test]
fn reordering_the_shown_views_leaves_the_hidden_ones_in_place() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        // Shown here: Documents and Files only.
        let mut order = RailOrder::default();
        assert_eq!(order.visible(), vec![Documents, Files]);
        assert!(order.set_visible(&[Files, Documents]));
        assert_eq!(order.visible(), vec![Files, Documents]);
        assert_eq!(order.0[0], Home, "the hidden view kept its slot");
        assert_eq!(&order.0[1..3], &[Files, Documents]);
        assert_eq!(order.0[3..], NavigatorMode::ALL[3..], "and so did the rest");

        assert!(!order.set_visible(&[Files]), "a view lost");
        assert!(!order.set_visible(&[Files, Files]), "a view doubled");
        assert!(!order.set_visible(&[Files, Chats]), "a hidden view drawn");
        assert_eq!(order.visible(), vec![Files, Documents], "unchanged");
    });
}
