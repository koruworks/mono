// SPDX-License-Identifier: MPL-2.0

//! **D19's one click**, from the column's side: what pressing Register
//! does, what each refusal reads as, and that the keyboard can reach every
//! one of them.
//!
//! The refusals here are the codes the gateway route actually answers with
//! — `artifacts/run_folder/tests.rs` is where each is *produced*, and this
//! is where each is *read*. The two lists meeting is the point: a code
//! renamed on one side fails on the other.

use koru_i18n::Lang;
use koru_i18n::strings::{Key, t};

use super::*;
use crate::model::Registering;
use crate::rows::Row;
use crate::{Press, Work};

fn with_one_unregistered() -> ArtifactsSurface {
    showing(Catalog::build(
        Vec::new(),
        vec![folder(
            run(1),
            0,
            vec![
                unregistered("findings.md", 1_400),
                unregistered("out/data.csv", 42),
            ],
        )],
        Some(Default::default()),
    ))
}

/// Pressing the word sends it, and the row says so while it is in flight.
#[test]
fn pressing_register_marks_the_row_and_a_second_press_does_not_send_twice() {
    let mut surface = with_one_unregistered();
    surface.register(run(1), path("findings.md"));
    assert!(matches!(
        surface.registering(run(1), &path("findings.md")),
        Some(Registering::Sending)
    ));
    surface.register(run(1), path("findings.md"));
    assert!(matches!(
        surface.registering(run(1), &path("findings.md")),
        Some(Registering::Sending)
    ));
    // The other file is untouched: one row, one gesture.
    assert!(surface.registering(run(1), &path("out/data.csv")).is_none());
}

/// A success takes the row out of the section — the two lists have to agree
/// about what has been adopted.
#[test]
fn a_registered_file_stops_being_offered() {
    let mut surface = with_one_unregistered();
    surface.register(run(1), path("findings.md"));
    surface.take_registered(run(1), path("findings.md"), Ok(()));
    assert!(surface.registering(run(1), &path("findings.md")).is_none());

    // The refreshed catalog is what actually moves it, and it does.
    let one = family(1, "findings.md", 500);
    let versions = history(&one, 1);
    let present = all_present(&[(one.clone(), versions.clone())]);
    surface.take_catalog(Ok(Catalog::build(
        vec![(one, versions)],
        vec![folder(run(1), 1, vec![unregistered("out/data.csv", 42)])],
        Some(present),
    )));
    let rows = rows_of(&mut surface);
    assert!(rows.contains(&Row::Family(0)), "{rows:?}");
    assert_eq!(
        rows.iter()
            .filter(|row| matches!(row, Row::File { .. }))
            .count(),
        1
    );
}

/// **Every refusal renders its stable code as a sentence**, on its own line
/// under the row it was pressed on — and the code itself is never drawn.
#[test]
fn every_refusal_is_a_sentence_under_the_row_and_never_a_code() {
    for code in super::locales::REFUSAL_CODES {
        let mut surface = with_one_unregistered();
        surface.register(run(1), path("findings.md"));
        surface.take_registered(
            run(1),
            path("findings.md"),
            Err(koru_artifact_wire::Failure::new(
                *code,
                "developer text",
                false,
            )),
        );
        let rows = rows_of(&mut surface);
        assert!(
            rows.contains(&Row::FileRefusal { run: 0, file: 0 }),
            "{code}: {rows:?}"
        );
        assert!(
            !rows.contains(&Row::FileRefusal { run: 0, file: 1 }),
            "{code}: the other row grew a refusal"
        );
        let sentence = crate::text::refusal(surface.lang(), code);
        assert!(!sentence.contains(code), "{code} is drawn as itself");
        assert!(!sentence.contains("developer text"));
    }
}

/// The three the roadmap names, spelled out, so a wording change is a
/// deliberate one.
#[test]
fn the_three_named_refusals_read_as_themselves() {
    let lang = Lang::En;
    assert_eq!(
        crate::text::refusal(lang, "file_changed"),
        t(lang, Key::ArtifactsRefusedFileChanged)
    );
    assert_eq!(
        crate::text::refusal(lang, "path_outside_run_folder"),
        "That file is no longer inside the run's results folder."
    );
    assert_eq!(
        crate::text::refusal(lang, "artifact_family_retired"),
        "That result is retired and takes no new version."
    );
}

/// Pressing again after reading the refusal clears it and asks a second
/// time — the person fixed what the sentence named.
#[test]
fn pressing_again_after_a_refusal_asks_again() {
    let mut surface = with_one_unregistered();
    surface.register(run(1), path("findings.md"));
    surface.take_registered(
        run(1),
        path("findings.md"),
        Err(koru_artifact_wire::Failure::new("file_changed", "", false)),
    );
    let rows: Vec<Row> = rows_of(&mut surface);
    let index = rows
        .iter()
        .position(|row| *row == Row::File { run: 0, file: 0 })
        .unwrap();
    let rect = surface.rows(tall(), TOP)[index].1;
    let parts = surface.row_parts(Row::File { run: 0, file: 0 }, rect);
    // On the word, which is the button.
    assert!(surface.mouse_down(
        tall(),
        TOP,
        parts.trailing.x + parts.trailing.w * 0.5,
        rect.y + rect.h * 0.5
    ));
    assert!(matches!(
        surface.registering(run(1), &path("findings.md")),
        Some(Registering::Sending)
    ));
}

/// A press on the row but **not** on the word puts the keyboard there and
/// adopts nothing: one click registers, and it is the click on *Register*.
#[test]
fn a_press_beside_the_word_focuses_without_registering() {
    let mut surface = with_one_unregistered();
    let rows = surface.rows(tall(), TOP);
    let (row, rect) = rows
        .into_iter()
        .find(|(row, _)| matches!(row, Row::File { .. }))
        .unwrap();
    assert!(surface.mouse_down(tall(), TOP, rect.x + 20.0, rect.y + rect.h * 0.5));
    assert_eq!(surface.focus(), Some(row));
    assert!(surface.registering(run(1), &path("findings.md")).is_none());
}

/// **Keyboard reachability**: every row the pointer can press, the arrows
/// can reach and Enter can choose — including Register.
#[test]
fn the_keyboard_reaches_every_row_the_pointer_can_press() {
    let one = family(1, "Findings", 500);
    let versions = history(&one, 3);
    let present = all_present(&[(one.clone(), versions.clone())]);
    let mut surface = showing(Catalog::build(
        vec![(one.clone(), versions)],
        vec![folder(run(1), 0, vec![unregistered("findings.md", 10)])],
        Some(present),
    ));

    // Down takes the keyboard from nowhere onto the first row that answers.
    assert!(surface.key(tall(), TOP, Press::Down));
    assert_eq!(surface.focus(), Some(Row::Family(0)));
    assert!(surface.has_keyboard());

    // Right opens its history, and again steps into the first older version.
    assert!(surface.key(tall(), TOP, Press::Right));
    assert!(surface.is_expanded(one.id));
    assert!(surface.key(tall(), TOP, Press::Right));
    assert_eq!(
        surface.focus(),
        Some(Row::Version {
            family: 0,
            version: 1
        })
    );

    // Enter selects the version, which is what shows its sentence.
    assert!(surface.key(tall(), TOP, Press::Enter));
    assert!(surface.selected().is_some());

    // Left folds the family and lands on it.
    assert!(surface.key(tall(), TOP, Press::Left));
    assert_eq!(surface.focus(), Some(Row::Family(0)));
    assert!(!surface.is_expanded(one.id));

    // Down walks on to the file row, and Enter registers it.
    while !matches!(surface.focus(), Some(Row::File { .. })) {
        assert!(surface.key(tall(), TOP, Press::Down));
    }
    assert!(surface.key(tall(), TOP, Press::Enter));
    assert!(matches!(
        surface.registering(run(1), &path("findings.md")),
        Some(Registering::Sending)
    ));

    // Escape gives the keyboard back — a list that keeps the arrows after
    // you have left it is a window that has stopped answering.
    assert!(surface.key(tall(), TOP, Press::Escape));
    assert!(!surface.has_keyboard());
    // And with the keyboard elsewhere, a key that is not an arrow falls
    // through rather than being swallowed.
    assert!(!surface.key(tall(), TOP, Press::Enter));
}

/// Only the seven keys are claimed. Everything else reaches the document.
#[test]
fn only_the_navigation_keys_are_claimed() {
    use winit::keyboard::{Key, NamedKey};
    assert_eq!(
        Press::of(&Key::Named(NamedKey::ArrowDown)),
        Some(Press::Down)
    );
    assert_eq!(Press::of(&Key::Named(NamedKey::Enter)), Some(Press::Enter));
    assert_eq!(
        Press::of(&Key::Named(NamedKey::Escape)),
        Some(Press::Escape)
    );
    assert_eq!(Press::of(&Key::Character("a".into())), None);
    assert_eq!(Press::of(&Key::Named(NamedKey::Tab)), None);
    assert_eq!(Press::of(&Key::Named(NamedKey::F2)), None);
}

/// **The column has no chord of its own.** The rail's digits belong to its
/// slots (the user, 2026-11-03), so the column is `Alt+n` for whichever slot
/// the person has put it in — the container's to say, not this crate's — and
/// no registry row carries its name.
#[test]
fn the_column_has_no_chord_of_its_own() {
    assert!(
        !koru_keys::REGISTRY
            .iter()
            .any(|entry| entry.label == "Artifacts"),
        "a row named for the column would be a chord that cannot follow it"
    );
}

/// Nothing on the open path builds a reader, so nothing in a test build
/// can dial a gateway.
#[test]
fn a_surface_built_under_test_has_no_reader_and_polls_to_nothing() {
    let mut surface = ArtifactsSurface::new(None);
    assert!(!surface.poll());
    assert_eq!(surface.work(), Work::default());
    // A refresh without a workspace asks for nothing at all.
    surface.refresh();
    assert_eq!(surface.load(), crate::Load::Cold);
}

/// Changing workspace forgets the other one's results rather than mixing
/// them.
#[test]
fn switching_workspace_clears_what_was_on_screen() {
    let mut surface = with_one_unregistered();
    surface.toggle_family(family(1, "x", 1).id);
    assert!(surface.set_workspace(Some(workspace())));
    assert!(surface.catalog().is_empty());
    assert_eq!(surface.load(), crate::Load::Cold);
    assert!(!surface.set_workspace(Some(workspace())));
}

/// **Nothing on the open path touches disk, the store or the network.**
///
/// Structurally: every read this surface makes is the `reader`'s, and a
/// surface without one has no path to a gateway at all. Behaviourally: with
/// no reader, drawing, pressing, scrolling and walking with the keyboard
/// change what is on screen and change nothing else — no state moves back
/// to `Loading`, no answer appears, and a Register that cannot be sent is
/// still recorded as sent so the row says so rather than looking ignored.
#[test]
fn the_window_threads_own_methods_ask_the_gateway_for_nothing() {
    let one = family(1, "Findings", 500);
    let versions = history(&one, 3);
    let present = all_present(&[(one.clone(), versions.clone())]);
    let mut surface = ArtifactsSurface::new(None);
    surface.set_now(10_000_000);
    surface.take_catalog(Ok(Catalog::build(
        vec![(one.clone(), versions)],
        vec![folder(run(1), 0, vec![unregistered("findings.md", 10)])],
        Some(present),
    )));
    assert_eq!(surface.load(), crate::Load::Ready);

    let mut engine = koru_draw::text::TextEngine::new();
    let mut scene = vello::Scene::new();
    surface.draw(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut engine,
        area(),
        TOP,
    );
    surface.mouse_move(area(), TOP, 40.0, TOP + 30.0);
    surface.mouse_down(area(), TOP, 40.0, TOP + 30.0);
    surface.scroll_by(area(), TOP, -20.0);
    surface.key(tall(), TOP, Press::Down);
    surface.select_version(surface.catalog().families[0].versions[0].id);
    surface.register(run(1), path("findings.md"));

    // The answer never came, because nothing was ever asked.
    assert_eq!(surface.load(), crate::Load::Ready);
    assert!(!surface.poll());
    assert!(matches!(
        surface.registering(run(1), &path("findings.md")),
        Some(Registering::Sending)
    ));
    assert!(surface.work().layouts > 0, "it did draw");
}

/// A refresh over an answer that is already showing does not blank it: what
/// is on screen stays until the newer answer replaces it.
#[test]
fn asking_again_keeps_what_is_on_screen() {
    let one = family(1, "Findings", 500);
    let versions = history(&one, 2);
    let present = all_present(&[(one.clone(), versions.clone())]);
    let mut surface = showing(Catalog::build(
        vec![(one, versions)],
        Vec::new(),
        Some(present),
    ));
    surface.refresh();
    assert_eq!(surface.load(), crate::Load::Ready);
    assert!(!surface.catalog().is_empty());
}
