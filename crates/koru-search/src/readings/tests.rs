// SPDX-License-Identifier: MPL-2.0

//! The seam's contract, and nothing behind it.

use super::*;

/// The empty source answers *unknown*, never *known to be empty*: a host that
/// wired no mirror must not tell the index that every file has been read and
/// held no words.
#[test]
fn the_empty_source_answers_nothing_rather_than_an_empty_reading() {
    let source = NoReadings;
    assert_eq!(
        source.readings(WorkspaceId::new(), &["b3:00ff".to_owned()]),
        None
    );
}

/// What a real source looks like from the index's side: one answer per hash it
/// has, nothing for one it lacks. An in-memory stand-in, as Plan 1.6's tests
/// will use.
struct InMemory(Vec<ReadingText>);

impl Readings for InMemory {
    fn readings(&self, _workspace: WorkspaceId, hashes: &[String]) -> Option<Vec<ReadingText>> {
        Some(
            self.0
                .iter()
                .filter(|r| hashes.contains(&r.hash))
                .cloned()
                .collect(),
        )
    }
}

#[test]
fn a_source_answers_only_for_the_hashes_it_has_and_can_be_boxed_for_the_indexer() {
    let manual = ReadingText {
        hash: "b3:aa".into(),
        page_count: 40,
        pages: vec![(3, "torque the bolts".into())],
    };
    let source: Box<dyn Readings> = Box::new(InMemory(vec![manual.clone()]));
    let answer = std::thread::spawn(move || {
        source.readings(
            WorkspaceId::new(),
            &["b3:aa".to_owned(), "b3:bb".to_owned()],
        )
    })
    .join()
    .unwrap();
    assert_eq!(answer, Some(vec![manual]));
}
