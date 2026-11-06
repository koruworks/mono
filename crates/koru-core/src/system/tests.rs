// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::attributes::{KEY_DUE, system_definition};

// ------------------------------------------------------------ definitions

#[test]
fn a_custom_definition_round_trips() {
    let mut sys = SystemDoc::create().unwrap();
    let effort = AttributeDefinition::custom("user.effort", "Effort", ValueType::Select)
        .with_options(vec!["small".into(), "medium".into(), "large".into()]);
    sys.define(&effort).unwrap();

    let back = SystemDoc::from_snapshot(&sys.export_snapshot().unwrap()).unwrap();
    assert_eq!(back.definitions(), vec![effort.clone()]);
    assert_eq!(back.definition("user.effort"), Some(effort));
}

#[test]
fn the_koru_namespace_is_refused() {
    let mut sys = SystemDoc::create().unwrap();
    let shadow = AttributeDefinition::custom("koru.due", "Deadline", ValueType::Date);
    assert!(matches!(
        sys.define(&shadow),
        Err(StoreError::AttributeRejected { .. })
    ));
    assert!(sys.definitions().is_empty());
}

/// `definition` answers the system registry first: a workspace cannot
/// override what `koru.due` means.
#[test]
fn system_keys_resolve_from_the_registry_not_the_document() {
    let sys = SystemDoc::create().unwrap();
    assert_eq!(sys.definition(KEY_DUE), system_definition(KEY_DUE));
}

/// A definition whose value type this build does not know is skipped — the
/// additive rule attribute values follow, applied to definitions.
#[test]
fn an_unknown_value_type_in_a_stored_definition_is_skipped() {
    let mut sys = SystemDoc::create().unwrap();
    let known = AttributeDefinition::custom("customer", "Customer", ValueType::Text);
    sys.define(&known).unwrap();

    // A record from a future build, written by hand.
    let defs = sys.doc.get_map(C_DEFINITIONS);
    let record = defs.ensure_mergeable_map("future-def").unwrap();
    record.insert(K_DEF_KEY, "owner").unwrap();
    record.insert(K_DEF_TYPE, "person").unwrap();
    sys.doc.commit();

    assert_eq!(sys.definitions(), vec![known]);
}

// ------------------------------------------------------------ tags

#[test]
fn a_tag_is_created_renamed_and_recolored_by_id() {
    let mut sys = SystemDoc::create().unwrap();
    let tag = sys.create_tag("mvp", Some("teal"), 1234).unwrap();

    let entry = sys.tag_entry(tag).unwrap();
    assert_eq!(entry.name, "mvp");
    assert_eq!(entry.color.as_deref(), Some("teal"));
    assert_eq!(entry.created_ms, Some(1234));
    assert_eq!(entry.alias_of, None);

    assert!(sys.rename_tag(tag, "launch").unwrap());
    assert!(sys.set_tag_color(tag, None).unwrap());
    let entry = sys.tag_entry(tag).unwrap();
    assert_eq!(entry.name, "launch");
    assert_eq!(entry.color, None);
    assert_eq!(entry.id, tag, "rename never touches identity");
}

#[test]
fn an_unknown_tag_answers_false_rather_than_erroring() {
    let mut sys = SystemDoc::create().unwrap();
    let ghost = TagId::new();
    assert!(!sys.rename_tag(ghost, "x").unwrap());
    assert!(!sys.set_tag_color(ghost, Some("red")).unwrap());
    assert!(!sys.merge_tags(ghost, TagId::new()).unwrap());
}

/// Merge re-points **nothing node-side**: the losing id becomes an alias
/// tombstone, and readers resolve through it.
#[test]
fn merging_tags_leaves_an_alias_the_old_id_resolves_through() {
    let mut sys = SystemDoc::create().unwrap();
    let ui = sys.create_tag("ui", None, 1).unwrap();
    let design = sys.create_tag("design", None, 2).unwrap();

    assert!(sys.merge_tags(ui, design).unwrap());
    assert_eq!(
        sys.resolve(ui),
        design,
        "a node still storing `ui` reads as `design`"
    );
    assert_eq!(sys.resolve(design), design);

    let listed: Vec<TagId> = sys.tags().into_iter().map(|t| t.id).collect();
    assert_eq!(listed, vec![design], "the alias earns no row");
    assert!(
        sys.tag_entry(ui).is_some(),
        "but the tombstone stays, or old ids would dangle"
    );
}

#[test]
fn a_self_merge_is_refused() {
    let mut sys = SystemDoc::create().unwrap();
    let tag = sys.create_tag("mvp", None, 1).unwrap();
    assert!(!sys.merge_tags(tag, tag).unwrap());
    assert_eq!(sys.resolve(tag), tag);
}

/// Two peers merging two tags into each other concurrently make a cycle; the
/// resolver is bounded and deterministic rather than spinning.
#[test]
fn a_merge_cycle_cannot_spin_the_resolver() {
    let mut a = SystemDoc::create().unwrap();
    let x = a.create_tag("x", None, 1).unwrap();
    let y = a.create_tag("y", None, 2).unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.merge_tags(x, y).unwrap();
    b.merge_tags(y, x).unwrap();
    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();

    assert_eq!(a.resolve(x), b.resolve(x), "same answer on every peer");
    assert_eq!(a.resolve(y), b.resolve(y));
}

#[test]
fn the_listing_is_sorted_and_live_only() {
    let mut sys = SystemDoc::create().unwrap();
    sys.create_tag("zeta", None, 1).unwrap();
    sys.create_tag("alpha", None, 2).unwrap();
    let names: Vec<String> = sys.tags().into_iter().map(|t| t.name).collect();
    assert_eq!(names, vec!["alpha", "zeta"]);
}

// ------------------------------------------------------- identity and paths

/// The eight supported languages' titles, used as tag names. Copied from
/// `koru_store_core::fixtures::SAMPLES` rather than imported — see the note in
/// `tags/tests.rs`.
const EIGHT_LANGUAGES: &[&str] = &[
    "Quarterly plan",
    "Plan trimestral",
    "Planejamento trimestral",
    "Quartalsplanung",
    "Plan trimestriel",
    "الخطة الفصلية",
    "季度计划",
    "四半期計画",
];

/// Identity is case-insensitive, display is case-preserving: `#HOME` answers
/// `#home`'s id and the chip keeps saying `home`.
#[test]
fn a_recased_name_reuses_the_record_and_keeps_the_original_casing() {
    let mut sys = SystemDoc::create().unwrap();
    let home = sys.create_tag("home", None, 1).unwrap();
    assert_eq!(sys.create_tag("HOME", None, 2).unwrap(), home);
    assert_eq!(sys.create_tag("  Home  ", Some("red"), 3).unwrap(), home);

    assert_eq!(sys.tags().len(), 1, "one tag, not three");
    let entry = sys.tag_entry(home).unwrap();
    assert_eq!(entry.name, "home", "the name is as first typed");
    assert_eq!(entry.color, None, "reuse does not repaint the record");
    assert_eq!(entry.created_ms, Some(1));
}

/// Full Unicode folding, not `eq_ignore_ascii_case` — the bug this closes —
/// and **accents fold too**, so the table agrees with the search index about
/// what one word is.
#[test]
fn folding_is_unicode_wide_and_reaches_accents() {
    let mut sys = SystemDoc::create().unwrap();
    let cafe = sys.create_tag("Café", None, 1).unwrap();
    for same in ["café", "CAFÉ", "cafe", "CAFE", "Cafe\u{301}"] {
        assert_eq!(sys.create_tag(same, None, 2).unwrap(), cafe, "{same:?}");
    }
    assert_eq!(
        sys.tag_entry(cafe).unwrap().name,
        "Café",
        "and the accent the first person typed is what everyone sees"
    );

    let strasse = sys.create_tag("straße", None, 4).unwrap();
    assert_eq!(sys.create_tag("STRASSE", None, 5).unwrap(), strasse);
    assert_eq!(sys.create_tag("Strasse", None, 6).unwrap(), strasse);

    // Folding accents is not folding words.
    assert_ne!(sys.create_tag("cave", None, 7).unwrap(), cafe);
    assert_eq!(sys.tags().len(), 3);
}

#[test]
fn every_language_folds_its_own_case_and_no_others() {
    let mut sys = SystemDoc::create().unwrap();
    let ids: Vec<TagId> = EIGHT_LANGUAGES
        .iter()
        .map(|title| sys.create_tag(title, None, 1).unwrap())
        .collect();
    assert_eq!(sys.tags().len(), EIGHT_LANGUAGES.len());
    for (title, id) in EIGHT_LANGUAGES.iter().zip(&ids) {
        assert_eq!(
            sys.create_tag(&title.to_uppercase(), None, 2).unwrap(),
            *id,
            "{title:?}"
        );
    }
    assert_eq!(sys.tags().len(), EIGHT_LANGUAGES.len(), "no second record");
}

/// `#fun/home` with no `fun` creates both records, and a mis-cased parent does
/// not spawn a second one.
#[test]
fn a_path_creates_its_parents_exactly_once() {
    let mut sys = SystemDoc::create().unwrap();
    let home = sys.create_tag("fun/home", None, 1).unwrap();
    let names: Vec<String> = sys.tags().into_iter().map(|t| t.name).collect();
    assert_eq!(names, vec!["fun".to_string(), "fun/home".into()]);

    // A second child reuses the parent; a mis-cased path lands in the branch.
    sys.create_tag("Fun/Outside", None, 2).unwrap();
    let names: Vec<String> = sys.tags().into_iter().map(|t| t.name).collect();
    assert_eq!(
        names,
        vec!["Fun/Outside".to_string(), "fun".into(), "fun/home".into()],
        "one `fun`, three records"
    );
    assert_eq!(
        sys.create_tag("FUN/HOME", None, 3).unwrap(),
        home,
        "the leaf folds per segment too"
    );

    // Three deep creates two parents.
    sys.create_tag("work/eu/berlin", None, 4).unwrap();
    let created: Vec<String> = sys
        .tags()
        .into_iter()
        .map(|t| t.name)
        .filter(|n| n.starts_with("work"))
        .collect();
    assert_eq!(
        created,
        vec![
            "work".to_string(),
            "work/eu".into(),
            "work/eu/berlin".into()
        ]
    );
}

/// Renaming a parent rewrites N **tag records** and zero documents — a node
/// stores a `TagId`, and every id here is untouched.
#[test]
fn renaming_a_parent_renames_the_subtree_and_no_ids_move() {
    let mut sys = SystemDoc::create().unwrap();
    let home = sys.create_tag("fun/home", None, 1).unwrap();
    let outside = sys.create_tag("Fun/outside", None, 2).unwrap();
    let attic = sys.create_tag("fun/home/attic", None, 3).unwrap();
    let elsewhere = sys.create_tag("funhouse", None, 4).unwrap();
    let fun = sys.live_tag_for_key("fun").unwrap();

    assert!(sys.rename_tag(fun, "leisure").unwrap());

    assert_eq!(sys.tag_entry(fun).unwrap().name, "leisure");
    assert_eq!(sys.tag_entry(home).unwrap().name, "leisure/home");
    assert_eq!(
        sys.tag_entry(outside).unwrap().name,
        "leisure/outside",
        "a mis-cased child travels with the branch"
    );
    assert_eq!(sys.tag_entry(attic).unwrap().name, "leisure/home/attic");
    assert_eq!(
        sys.tag_entry(elsewhere).unwrap().name,
        "funhouse",
        "a segment prefix is not a child"
    );
    for id in [fun, home, outside, attic, elsewhere] {
        assert_eq!(sys.resolve(id), id, "no node ever needs re-pointing");
    }

    // Renaming a leaf touches only itself.
    assert!(sys.rename_tag(home, "leisure/indoors").unwrap());
    assert_eq!(sys.tag_entry(attic).unwrap().name, "leisure/indoors/attic");
    assert_eq!(sys.tag_entry(fun).unwrap().name, "leisure");
}

/// A rename onto a name that already exists is one tag by the fold rule, and
/// the merge that follows is `merge_tags`' own alias machinery.
#[test]
fn renaming_onto_an_existing_name_folds_rather_than_duplicating() {
    let mut sys = SystemDoc::create().unwrap();
    let ui = sys.create_tag("ui", None, 1).unwrap();
    let design = sys.create_tag("design", None, 2).unwrap();

    assert!(sys.rename_tag(ui, "Design").unwrap());
    assert_eq!(sys.tags().len(), 1, "one live tag");
    let winner = ui.min(design);
    let loser = ui.max(design);
    assert_eq!(sys.resolve(loser), winner);
    assert_eq!(sys.tags()[0].id, winner);
}

// ------------------------------------------------- concurrent duplicates

/// Two peers, both offline, both typing `#home`. No picker check can prevent
/// it, so the fix is a reconcile: same fold key ⇒ the higher id aliases into
/// the lower, and **both peers reach that answer independently**.
#[test]
fn two_peers_each_minting_home_converge_on_one_tag() {
    let mut a = SystemDoc::create().unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    let from_a = a.create_tag("home", None, 1).unwrap();
    let from_b = b.create_tag("HOME", None, 2).unwrap();
    assert_ne!(from_a, from_b, "two v4 ids, minted with no coordination");

    let a_updates = a.export_all_updates().unwrap();
    let b_updates = b.export_all_updates().unwrap();
    a.import(&b_updates).unwrap();
    b.import(&a_updates).unwrap();

    let winner = from_a.min(from_b);
    let loser = from_a.max(from_b);
    for (who, peer) in [("a", &a), ("b", &b)] {
        assert_eq!(peer.tags().len(), 1, "peer {who} lists one tag");
        assert_eq!(peer.tags()[0].id, winner, "peer {who} agrees who survived");
        assert_eq!(peer.resolve(loser), winner, "peer {who} resolves the loser");
        assert!(
            peer.tag_entry(loser).is_some(),
            "peer {who} keeps the tombstone, or old ids dangle"
        );
    }
    assert_eq!(
        a.tags()[0].name,
        b.tags()[0].name,
        "and on the winner's display name"
    );
}

/// The fold reaches paths, and is idempotent — a second import writes nothing.
#[test]
fn the_fold_covers_paths_and_settles() {
    let mut a = SystemDoc::create().unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    a.create_tag("fun/home", None, 1).unwrap();
    b.create_tag("Fun/Home", None, 2).unwrap();

    a.import(&b.export_all_updates().unwrap()).unwrap();
    let names: Vec<String> = a.tags().into_iter().map(|t| t.name).collect();
    assert_eq!(names.len(), 2, "one `fun`, one `fun/home`: {names:?}");
    assert_eq!(
        a.fold_duplicate_tags().unwrap(),
        0,
        "nothing left to fold, so nothing is written"
    );
}

/// A merged-away tombstone's name is history, not a reservation: typing the
/// old name again mints a live tag rather than resurrecting the alias.
#[test]
fn an_alias_tombstone_does_not_claim_its_old_name() {
    let mut sys = SystemDoc::create().unwrap();
    let ui = sys.create_tag("ui", None, 1).unwrap();
    let design = sys.create_tag("design", None, 2).unwrap();
    sys.merge_tags(ui, design).unwrap();

    let fresh = sys.create_tag("UI", None, 3).unwrap();
    assert_ne!(fresh, ui);
    assert_eq!(sys.resolve(ui), design, "the tombstone still forwards");
    assert_eq!(sys.resolve(fresh), fresh);
}

// ------------------------------------------------------------ lists

#[test]
fn a_list_is_created_renamed_recolored_and_reordered_by_id() {
    let mut sys = SystemDoc::create().unwrap();
    let list = sys.create_list("Wedding", Some("pink"), 1234).unwrap();

    let entry = sys.list_entry(list).unwrap();
    assert_eq!(entry.name, "Wedding");
    assert_eq!(entry.color.as_deref(), Some("pink"));
    assert_eq!(entry.created_ms, Some(1234));
    assert_eq!(entry.order, None);
    assert_eq!(entry.deleted_ms, None);

    assert!(sys.rename_list(list, "Wedding day").unwrap());
    assert!(sys.set_list_color(list, Some("red")).unwrap());
    assert!(sys.set_list_order(list, Some(2)).unwrap());
    let entry = sys.list_entry(list).unwrap();
    assert_eq!(entry.name, "Wedding day");
    assert_eq!(entry.color.as_deref(), Some("red"));
    assert_eq!(entry.order, Some(2));
    assert_eq!(entry.id, list, "rename never touches identity");

    assert!(sys.set_list_color(list, None).unwrap());
    assert!(sys.set_list_order(list, None).unwrap());
    let entry = sys.list_entry(list).unwrap();
    assert_eq!(entry.color, None);
    assert_eq!(entry.order, None);
}

#[test]
fn an_unknown_list_answers_false_rather_than_erroring() {
    let mut sys = SystemDoc::create().unwrap();
    let ghost = ListId::new();
    assert!(!sys.rename_list(ghost, "x").unwrap());
    assert!(!sys.set_list_color(ghost, Some("red")).unwrap());
    assert!(!sys.set_list_order(ghost, Some(1)).unwrap());
    assert!(!sys.delete_list(ghost, 1).unwrap());
    assert_eq!(sys.list_entry(ghost), None);
}

/// **Deleting a list is a tombstone, and a tombstone is not a rename target.**
/// The row leaves the rail at once and the record stays, so a task still
/// carrying the id reads *deleted* rather than *unknown*.
#[test]
fn a_deleted_list_leaves_the_rail_and_cannot_be_written_again() {
    let mut sys = SystemDoc::create().unwrap();
    let wedding = sys.create_list("Wedding", None, 1).unwrap();
    let bike = sys.create_list("Bike Project", None, 2).unwrap();

    assert!(sys.delete_list(wedding, 3).unwrap());
    let listed: Vec<ListId> = sys.lists().into_iter().map(|l| l.id).collect();
    assert_eq!(listed, vec![bike], "the tombstone earns no row");
    assert_eq!(sys.list_entry(wedding).unwrap().deleted_ms, Some(3));

    assert!(!sys.rename_list(wedding, "Anything").unwrap());
    assert!(!sys.set_list_color(wedding, Some("teal")).unwrap());
    assert!(!sys.delete_list(wedding, 4).unwrap());
    assert_eq!(
        sys.list_entry(wedding).unwrap().name,
        "Wedding",
        "a deleted list cannot be edited back into existence"
    );
}

/// A deleted list's name is history, not a reservation — the tag table's rule
/// for an alias tombstone, applied to a delete.
#[test]
fn a_deleted_lists_name_can_be_used_again_as_a_new_list() {
    let mut sys = SystemDoc::create().unwrap();
    let first = sys.create_list("Reading", None, 1).unwrap();
    sys.delete_list(first, 2).unwrap();
    let second = sys.create_list("Reading", None, 3).unwrap();
    assert_ne!(second, first, "a new list, not a resurrection");
    assert_eq!(sys.lists().len(), 1);
}

/// Typing a name a live list already carries answers that list rather than
/// drawing a second identical row — `create_tag`'s local rule.
#[test]
fn a_recased_list_name_reuses_the_live_record() {
    let mut sys = SystemDoc::create().unwrap();
    let wedding = sys.create_list("Wedding", Some("pink"), 1).unwrap();
    assert_eq!(
        sys.create_list("WEDDING", Some("blue"), 2).unwrap(),
        wedding
    );
    assert_eq!(
        sys.list_entry(wedding).unwrap().color.as_deref(),
        Some("pink"),
        "and leaves the original's display alone"
    );
    assert_eq!(sys.lists().len(), 1);
}

/// Rail order: ranked lists by rank, then the unranked by name.
#[test]
fn lists_come_back_in_rail_order() {
    let mut sys = SystemDoc::create().unwrap();
    let reading = sys.create_list("Reading", None, 1).unwrap();
    let wedding = sys.create_list("Wedding", None, 2).unwrap();
    let bike = sys.create_list("Bike Project", None, 3).unwrap();
    sys.set_list_order(wedding, Some(0)).unwrap();
    sys.set_list_order(bike, Some(1)).unwrap();

    let order: Vec<ListId> = sys.lists().into_iter().map(|l| l.id).collect();
    assert_eq!(order, vec![wedding, bike, reading], "unranked sorts last");
}

/// **Two peers, both offline, each making a list.** The registry is one Loro
/// map of mergeable records, so the union is the import — both peers list both
/// lists, in the same order, and neither id was invented from a name.
#[test]
fn two_peers_creating_lists_offline_converge_on_both() {
    let mut a = SystemDoc::create().unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();
    let from_a = a.create_list("Wedding", Some("pink"), 1).unwrap();
    let from_b = b.create_list("Bike Project", Some("blue"), 2).unwrap();
    assert_ne!(from_a, from_b, "two v4 ids, minted with no coordination");

    let a_updates = a.export_all_updates().unwrap();
    let b_updates = b.export_all_updates().unwrap();
    a.import(&b_updates).unwrap();
    b.import(&a_updates).unwrap();

    let listed = |peer: &SystemDoc| -> Vec<(ListId, String, Option<String>)> {
        peer.lists()
            .into_iter()
            .map(|l| (l.id, l.name, l.color))
            .collect()
    };
    assert_eq!(listed(&a), listed(&b), "both peers see one table");
    let ids: Vec<ListId> = a.lists().into_iter().map(|l| l.id).collect();
    assert!(ids.contains(&from_a) && ids.contains(&from_b), "{ids:?}");
}

/// A delete on one peer and an edit on the other: the tombstone wins the row
/// away from the rail on both, and neither peer loses the record.
#[test]
fn a_delete_and_a_rename_made_apart_converge() {
    let mut a = SystemDoc::create().unwrap();
    let list = a.create_list("Wedding", None, 1).unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.delete_list(list, 2).unwrap();
    b.rename_list(list, "Wedding day").unwrap();
    let a_updates = a.export_all_updates().unwrap();
    let b_updates = b.export_all_updates().unwrap();
    a.import(&b_updates).unwrap();
    b.import(&a_updates).unwrap();

    for (who, peer) in [("a", &a), ("b", &b)] {
        assert!(peer.lists().is_empty(), "peer {who} draws no row");
        assert_eq!(
            peer.list_entry(list).unwrap().deleted_ms,
            Some(2),
            "peer {who} keeps the tombstone"
        );
    }
}

// ------------------------------------------------------------ reconciliation

/// Two peers each lazily create a system document before they meet. The
/// manifest-side reconcile keeps the lowest [`DocumentId`] and absorbs the
/// other — and the absorb is a union, because both documents share the same
/// named root containers.
#[test]
fn concurrent_creation_reconciles_by_lowest_id_union() {
    let mut a = SystemDoc::create().unwrap();
    let mvp = a.create_tag("mvp", None, 1).unwrap();
    a.define(&AttributeDefinition::custom(
        "customer",
        "Customer",
        ValueType::Text,
    ))
    .unwrap();

    let mut b = SystemDoc::create().unwrap();
    let research = b.create_tag("research", None, 2).unwrap();
    b.define(&AttributeDefinition::custom(
        "effort",
        "Effort",
        ValueType::Number,
    ))
    .unwrap();

    let doc_a = DocumentId::new();
    let doc_b = DocumentId::new();
    let canonical = SystemDoc::canonical_document(&[doc_a, doc_b]).unwrap();
    assert_eq!(canonical, doc_a.min(doc_b), "lowest id, no talking needed");

    a.absorb(&b.export_snapshot().unwrap()).unwrap();
    let tag_ids: Vec<TagId> = a.tags().into_iter().map(|t| t.id).collect();
    assert!(
        tag_ids.contains(&mvp) && tag_ids.contains(&research),
        "{tag_ids:?}"
    );
    let keys: Vec<String> = a.definitions().into_iter().map(|d| d.key).collect();
    assert_eq!(keys, vec!["customer", "effort"]);
}

/// Two collaborators each teach the workspace its first word before their
/// documents meet — both words survive the absorb, and an older build's
/// system document (no dictionary container) reads an empty dictionary.
#[test]
fn workspace_dictionary_words_merge_and_read_back() {
    let mut a = SystemDoc::create().unwrap();
    assert!(a.dictionary().is_empty(), "additive: absent reads empty");
    assert!(a.add_dictionary_word("en-US", "Koru").unwrap());
    assert!(
        !a.add_dictionary_word("en-US", "Koru").unwrap(),
        "idempotent"
    );

    let mut b = SystemDoc::create().unwrap();
    assert!(b.add_dictionary_word("en-US", "iroh").unwrap());
    assert!(b.add_dictionary_word("de", "Arbeitsplatzrechner").unwrap());

    a.absorb(&b.export_snapshot().unwrap()).unwrap();
    let words = a.dictionary();
    assert_eq!(words["en-US"], vec!["Koru", "iroh"]);
    assert_eq!(words["de"], vec!["Arbeitsplatzrechner"]);

    assert!(
        a.remove_dictionary_word("de", "Arbeitsplatzrechner")
            .unwrap()
    );
    assert!(!a.remove_dictionary_word("de", "nichts").unwrap());
    assert!(!a.dictionary().contains_key("de"));
}

#[test]
fn open_or_create_opens_what_exists_and_creates_otherwise() {
    let mut sys = SystemDoc::create().unwrap();
    let tag = sys.create_tag("mvp", None, 1).unwrap();
    let snapshot = sys.export_snapshot().unwrap();

    let opened = SystemDoc::open_or_create(Some(&snapshot)).unwrap();
    assert!(opened.tag_entry(tag).is_some());

    let fresh = SystemDoc::open_or_create(None).unwrap();
    assert!(fresh.tags().is_empty());
}

// ------------------------------------------------------------ pages

/// A page's generations are a grow-only set on its record, and the current
/// one is the numeric maximum (page-document plan §6.2).
#[test]
fn a_lists_page_generations_grow_and_the_current_one_is_the_numeric_maximum() {
    let mut sys = SystemDoc::create().unwrap();
    let list = sys.create_list("Bike", None, 1).unwrap();
    let page = PageKey::List(list);
    assert_eq!(sys.page_generations(page), Vec::<u64>::new());
    assert_eq!(sys.current_page_generation(page), None);

    assert!(sys.add_page_generation(page, 0).unwrap());
    assert!(
        sys.add_page_generation(page, 0).unwrap(),
        "a repeat is a no-op"
    );
    for n in [1, 10, 9] {
        assert!(sys.add_page_generation(page, n).unwrap());
    }
    assert_eq!(sys.page_generations(page), vec![0, 1, 9, 10]);
    assert_eq!(
        sys.current_page_generation(page),
        Some(10),
        "numeric, not lexical"
    );
}

/// A deleted list keeps its document (D4), so its page record still reads
/// and still takes a generation; a list the table never held is refused.
#[test]
fn a_deleted_list_still_has_its_page_and_an_unknown_list_has_none() {
    let mut sys = SystemDoc::create().unwrap();
    let list = sys.create_list("Bike", None, 1).unwrap();
    let page = PageKey::List(list);
    assert!(sys.add_page_generation(page, 0).unwrap());
    assert!(sys.delete_list(list, 2).unwrap());
    assert_eq!(sys.page_generations(page), vec![0]);
    assert!(sys.add_page_generation(page, 1).unwrap());
    assert_eq!(sys.current_page_generation(page), Some(1));

    let ghost = PageKey::List(ListId::new());
    assert!(!sys.add_page_generation(ghost, 0).unwrap());
    assert_eq!(sys.page_generations(ghost), Vec::<u64>::new());
}

/// Buckets keep their page records under the `pages` root, one per token,
/// created on first use; nothing else in the document moves.
#[test]
fn a_buckets_page_record_is_its_own() {
    let mut sys = SystemDoc::create().unwrap();
    let today = PageKey::Bucket(crate::tasks::Bucket::Today);
    let next = PageKey::Bucket(crate::tasks::Bucket::Next);
    assert_eq!(sys.current_page_generation(today), None);
    assert!(sys.add_page_generation(today, 0).unwrap());
    assert_eq!(sys.page_generations(today), vec![0]);
    assert_eq!(sys.page_generations(next), Vec::<u64>::new(), "per bucket");
    assert!(sys.lists().is_empty(), "a bucket's page is not a list");
}

/// Two devices replacing a page's document offline converge on the union of
/// their generations, and both then agree on the current one.
#[test]
fn two_peers_adding_generations_offline_converge_on_the_union() {
    let mut a = SystemDoc::create().unwrap();
    let list = a.create_list("Bike", None, 1).unwrap();
    let page = PageKey::List(list);
    a.add_page_generation(page, 0).unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.add_page_generation(page, 1).unwrap();
    b.add_page_generation(page, 2).unwrap();
    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();

    assert_eq!(a.page_generations(page), vec![0, 1, 2]);
    assert_eq!(b.page_generations(page), vec![0, 1, 2]);
    assert_eq!(a.current_page_generation(page), Some(2));
    assert_eq!(b.current_page_generation(page), Some(2));
}

// ------------------------------------------------------------ titles

fn sync(a: &mut SystemDoc, b: &mut SystemDoc) {
    let from_a = a.export_all_updates().unwrap();
    let from_b = b.export_all_updates().unwrap();
    a.import(&from_b).unwrap();
    b.import(&from_a).unwrap();
}

/// §6.6: the current generation reads the list's name; a direct rename
/// overrides it; a list rename takes the override back (Q5, Q10).
#[test]
fn a_lists_note_follows_the_list_until_renamed_directly_and_again_after() {
    let mut sys = SystemDoc::create().unwrap();
    let list = sys.create_list("Bike", None, 1).unwrap();
    let page = PageKey::List(list);
    sys.add_page_generation(page, 0).unwrap();
    assert_eq!(sys.page_title(page, 0), PageTitle::Text("Bike".into()));

    assert!(sys.set_page_override(page, 0, Some("Old research")).unwrap());
    assert_eq!(sys.page_title(page, 0), PageTitle::Text("Old research".into()));
    assert!(sys.rename_list(list, "Bike Build").unwrap());
    assert_eq!(sys.page_override(page, 0), None, "the list rename took it back");
    assert_eq!(sys.page_title(page, 0), PageTitle::Text("Bike Build".into()));

    sys.delete_list(list, 2).unwrap();
    assert_eq!(
        sys.page_title(page, 0),
        PageTitle::Text("Bike Build".into()),
        "a deleted list's note keeps its last name (D4)"
    );
    assert_eq!(sys.all_lists().len(), 1, "tombstones included");
    assert!(sys.lists().is_empty());
}

/// §6.6: a replacement freezes the superseded generation's name in the
/// same commit, so a later list rename moves only the new document — and
/// a concurrent rename cannot erase it, because the renaming replica never
/// wrote the frozen key.
#[test]
fn a_replacement_freezes_the_old_notes_name_and_a_concurrent_list_rename_moves_only_the_new() {
    let mut a = SystemDoc::create().unwrap();
    let list = a.create_list("Bike", None, 1).unwrap();
    let page = PageKey::List(list);
    a.add_page_generation(page, 0).unwrap();
    a.set_page_override(page, 0, Some("Old research")).unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    a.add_page_generation(page, 1).unwrap();
    assert_eq!(
        a.page_frozen(page, 0),
        Some(FrozenTitle::Text("Old research".into()))
    );
    b.rename_list(list, "Bike Build").unwrap();
    sync(&mut a, &mut b);
    for (who, sys) in [("a", &a), ("b", &b)] {
        assert_eq!(sys.page_generations(page), vec![0, 1], "{who}");
        assert_eq!(sys.page_override(page, 0), None, "{who}: B removed it");
        assert_eq!(
            sys.page_title(page, 0),
            PageTitle::Text("Old research".into()),
            "{who}: generation 0 is superseded and keeps its frozen name"
        );
        assert_eq!(
            sys.page_title(page, 1),
            PageTitle::Text("Bike Build".into()),
            "{who}: generation 1 follows the list"
        );
    }
}

/// §6.6: a replaced bucket note freezes the bucket, not a label, so every
/// reader localizes it; a literal title `bucket:today` stays literal.
#[test]
fn a_replaced_bucket_note_freezes_the_bucket_and_a_literal_title_stays_literal() {
    let mut sys = SystemDoc::create().unwrap();
    let today = PageKey::Bucket(crate::tasks::Bucket::Today);
    sys.add_page_generation(today, 0).unwrap();
    assert_eq!(sys.page_title(today, 0), PageTitle::Bucket(crate::tasks::Bucket::Today));
    sys.add_page_generation(today, 1).unwrap();
    assert_eq!(sys.page_frozen(today, 0), Some(FrozenTitle::Bucket("today".into())));
    assert_eq!(sys.page_title(today, 0), PageTitle::Bucket(crate::tasks::Bucket::Today));

    sys.set_page_override(today, 1, Some("bucket:today")).unwrap();
    sys.add_page_generation(today, 2).unwrap();
    assert_eq!(
        sys.page_frozen(today, 1),
        Some(FrozenTitle::Text("bucket:today".into()))
    );
    assert_eq!(sys.page_title(today, 1), PageTitle::Text("bucket:today".into()));
    assert_eq!(sys.page_title(today, 2), PageTitle::Bucket(crate::tasks::Bucket::Today));
}

/// §6.6: a direct rename of a superseded generation is its frozen name too,
/// and two writers freezing one generation concurrently with different
/// kinds converge on one whole value — never a kind from one and a value
/// from the other.
#[test]
fn frozen_names_are_whole_values() {
    let mut a = SystemDoc::create().unwrap();
    let today = PageKey::Bucket(crate::tasks::Bucket::Today);
    a.add_page_generation(today, 0).unwrap();
    let mut b = SystemDoc::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    // A replaces Today (freezing the bucket); B, not seeing that, renames
    // generation 0 directly and then replaces it too (freezing text).
    a.add_page_generation(today, 1).unwrap();
    b.set_page_override(today, 0, Some("Monday's plan")).unwrap();
    b.add_page_generation(today, 1).unwrap();
    sync(&mut a, &mut b);
    let frozen = a.page_frozen(today, 0).expect("frozen");
    assert_eq!(frozen, b.page_frozen(today, 0).unwrap(), "converged");
    assert!(
        matches!(
            &frozen,
            FrozenTitle::Bucket(t) if t == "today"
        ) || matches!(&frozen, FrozenTitle::Text(t) if t == "Monday's plan"),
        "one of the two whole values: {frozen:?}"
    );

    // Renaming a superseded generation afterwards is a frozen name as well.
    a.set_page_override(today, 0, Some("Archived")).unwrap();
    assert_eq!(a.page_frozen(today, 0), Some(FrozenTitle::Text("Archived".into())));
    assert_eq!(a.page_title(today, 0), PageTitle::Text("Archived".into()));
}
