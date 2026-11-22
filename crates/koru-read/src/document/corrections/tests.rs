// SPDX-License-Identifier: MPL-2.0
use super::*;

fn hash() -> String {
    format!("b3:{}", "ab".repeat(32))
}

#[test]
fn corrections_survive_fresh_ocr_and_reset_survives_replay() {
    let doc = ReadingDoc::new();
    let hash = hash();
    doc.begin(&hash, &ReaderId::text(), 1).unwrap();
    assert!(
        doc.correct(&hash, "one", &[], Some("corrected".into()))
            .unwrap()
    );
    let old = doc.export_snapshot().unwrap();
    doc.begin_fresh(&hash, &ReaderId::text(), 2).unwrap();
    assert_eq!(doc.corrections(&hash).text().unwrap().as_str(), "corrected");
    assert!(doc.correct(&hash, "reset", &["one".into()], None).unwrap());
    doc.import(&old).unwrap();
    assert!(doc.corrections(&hash).text().is_none());
    assert_eq!(doc.corrections(&hash).0.len(), 2);
}

#[test]
fn corrections_retain_concurrent_edits_and_refuse_a_stale_save() {
    let a = ReadingDoc::new();
    let hash = hash();
    a.begin(&hash, &ReaderId::text(), 1).unwrap();
    let b = ReadingDoc::from_bytes(&a.export_snapshot().unwrap()).unwrap();
    assert!(a.correct(&hash, "a", &[], Some("first".into())).unwrap());
    assert!(b.correct(&hash, "b", &[], Some("second".into())).unwrap());
    let a_bytes = a.export_snapshot().unwrap();
    a.import(&b.export_snapshot().unwrap()).unwrap();
    b.import(&a_bytes).unwrap();
    assert_eq!(a.corrections(&hash), b.corrections(&hash));
    assert_eq!(a.corrections(&hash).heads(), ["a", "b"]);
    assert!(!a.correct(&hash, "stale", &[], Some("lost".into())).unwrap());
    assert!(
        a.correct(
            &hash,
            "merged",
            &["a".into(), "b".into()],
            Some("both".into())
        )
        .unwrap()
    );
    assert_eq!(a.corrections(&hash).heads(), ["merged"]);
    assert_eq!(a.corrections(&hash).0.len(), 3);
}
