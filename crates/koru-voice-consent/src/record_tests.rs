// SPDX-License-Identifier: MPL-2.0

//! **Consent is never inherited across features.**
//!
//! `Granted::covers` compares only a licence and a size. That is the right
//! question within one feature and the wrong one across two: an Apache-2.0
//! speech model somebody agreed to would otherwise "cover" an Apache-2.0 reader
//! model of the same size or smaller. These are the tests that say it does not.

use super::*;

/// One offer, used for both features on purpose: the same licence and the
/// same size is exactly the case `covers` alone would wave through.
fn offer() -> Terms {
    Terms {
        model: "some-model".into(),
        version: "1.0.0".into(),
        licence: "Apache-2.0".into(),
        attribution: "somebody, Apache-2.0".into(),
        download_bytes: 16 * 1024 * 1024,
    }
}

/// A reader offer that is the same licence and *smaller* than the voice grant,
/// which `Granted::covers` would answer `true` for.
fn reader_offer() -> Terms {
    Terms {
        model: "text-reader".into(),
        download_bytes: offer().download_bytes - 1,
        ..offer()
    }
}

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temporary directory")
}

/// Voice's record is the file it always was, written the way it always was.
#[test]
fn voice_keeps_its_file_its_format_and_its_free_functions() {
    assert_eq!(VOICE.name(), "voice-consent.conf");
    assert_eq!(VOICE.format(), "koru-voice-consent");
    assert_eq!(VOICE.name(), CONSENT_FILE);
    assert_eq!(VOICE.format(), FORMAT);

    let dir = scratch();
    assert_eq!(path_in(dir.path()), VOICE.path_in(dir.path()));
    grant(&path_in(dir.path()), offer(), 3).unwrap();
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path_in(dir.path())).unwrap()).unwrap();
    assert_eq!(written["format"], "koru-voice-consent");
    assert_eq!(written["schema_version"], SCHEMA_VERSION);
    assert_eq!(
        read(&path_in(dir.path())).granted().unwrap().record(),
        VOICE
    );
    assert_eq!(Granted::recorded(offer(), 1).record(), VOICE);
}

#[test]
fn the_reader_has_its_own_file_and_format() {
    assert_eq!(READ.name(), "read-consent.conf");
    assert_eq!(READ.format(), "koru-read-consent");
}

/// One list, and no two records share a file or a format — which is what
/// stops a third feature being added by copying a name.
#[test]
fn no_two_records_share_a_file_or_a_format() {
    for (at, record) in ConsentFile::ALL.iter().enumerate() {
        for other in &ConsentFile::ALL[..at] {
            assert_ne!(record.name(), other.name());
            assert_ne!(record.format(), other.format());
        }
    }
}

/// **The roadmap's test.** A grant in `voice-consent.conf` does not cover a
/// reader offer — not through the file the reader reads, and not through the
/// gate a store asks, even for an offer `covers` alone would accept.
#[test]
fn a_grant_in_voice_consent_does_not_cover_a_reader_offer() {
    let dir = scratch();
    let voice = VOICE.grant(&VOICE.path_in(dir.path()), offer(), 1).unwrap();
    assert!(
        voice.covers(&reader_offer()),
        "the premise: licence and size alone would wave the reader through"
    );

    // The reader's own record, in the same directory, was never written.
    assert_eq!(
        READ.read(&READ.path_in(dir.path())),
        Consent::Absent(Reason::NeverAsked)
    );
    // And the token voice holds does not open the reader's door.
    assert!(!READ.covers(&voice, &reader_offer()));
    let reread = VOICE.read(&VOICE.path_in(dir.path()));
    assert!(!READ.covers(reread.granted().unwrap(), &reader_offer()));
    assert!(VOICE.covers(&voice, &offer()));
}

/// The mirror: a reader grant does not cover a voice offer.
#[test]
fn a_grant_in_read_consent_does_not_cover_a_voice_offer() {
    let dir = scratch();
    let reader = READ.grant(&READ.path_in(dir.path()), offer(), 1).unwrap();
    assert_eq!(reader.record(), READ);

    assert_eq!(
        read(&path_in(dir.path())),
        Consent::Absent(Reason::NeverAsked)
    );
    assert!(!VOICE.covers(&reader, &offer()));
    assert!(READ.covers(&reader, &offer()));
}

/// Revoking one feature's consent leaves the other's exactly as it was —
/// which in one shared file would have been impossible.
#[test]
fn revoking_one_feature_leaves_the_other_granted() {
    let dir = scratch();
    VOICE.grant(&VOICE.path_in(dir.path()), offer(), 1).unwrap();
    READ.grant(&READ.path_in(dir.path()), offer(), 2).unwrap();
    let reader_before = std::fs::read(READ.path_in(dir.path())).unwrap();

    VOICE.revoke(&VOICE.path_in(dir.path())).unwrap();

    assert_eq!(
        VOICE.read(&VOICE.path_in(dir.path())),
        Consent::Absent(Reason::Revoked)
    );
    assert!(READ.read(&READ.path_in(dir.path())).is_granted());
    assert_eq!(
        std::fs::read(READ.path_in(dir.path())).unwrap(),
        reader_before
    );
}

/// The format tag is what refuses a record copied under the other feature's
/// name — a person "fixing" a missing reader consent by copying voice's file.
#[test]
fn a_record_copied_under_the_other_name_is_not_consent() {
    let dir = scratch();
    VOICE.grant(&VOICE.path_in(dir.path()), offer(), 1).unwrap();
    std::fs::copy(VOICE.path_in(dir.path()), READ.path_in(dir.path())).unwrap();
    assert_eq!(
        READ.read(&READ.path_in(dir.path())),
        Consent::Absent(Reason::Malformed)
    );

    let other = scratch();
    READ.grant(&READ.path_in(other.path()), offer(), 1).unwrap();
    std::fs::copy(READ.path_in(other.path()), VOICE.path_in(other.path())).unwrap();
    assert_eq!(
        VOICE.read(&VOICE.path_in(other.path())),
        Consent::Absent(Reason::Malformed)
    );
}

/// A grant that has not reached the disk carries its record too, so the
/// in-memory token a consent step holds is as separated as the file.
#[test]
fn an_in_memory_grant_names_its_record() {
    let reader = READ.recorded(offer(), 1);
    assert_eq!(reader.record(), READ);
    assert!(!VOICE.covers(&reader, &offer()));
    let voice = Granted::recorded_all(vec![offer()], 1);
    assert!(!READ.covers(&voice, &offer()));
}
