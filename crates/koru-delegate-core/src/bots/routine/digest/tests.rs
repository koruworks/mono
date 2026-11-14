// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_digest_stream_preserves_domain_and_exact_utf8_json_bound() {
    #[derive(Serialize)]
    struct Value<'a> {
        version: u8,
        text: &'a str,
    }
    let value = Value {
        version: 1,
        text: "é\n",
    };
    let bytes = b"{\"version\":1,\"text\":\"\xc3\xa9\\n\"}";
    assert_eq!(serde_json::to_vec(&value).unwrap(), bytes);
    let mut reference = blake3::Hasher::new();
    reference.update(b"domain");
    reference.update(bytes);
    assert_eq!(
        routine_digest_json(b"domain", &value, bytes.len())
            .unwrap()
            .as_bytes(),
        reference.finalize().as_bytes()
    );
    assert_eq!(
        routine_digest_json(b"domain", &value, bytes.len() - 1),
        Err(RoutineDigestError::TooLarge)
    );
    assert_ne!(
        routine_digest_json(b"domain", &value, bytes.len()).unwrap(),
        routine_digest_json(b"other", &value, bytes.len()).unwrap()
    );
}

#[test]
fn routine_digest_stream_refuses_serializer_errors_without_returning_partial_hash() {
    struct Fail;
    impl Serialize for Fail {
        fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("private fixture failure"))
        }
    }
    assert_eq!(
        routine_digest_json(b"domain", &Fail, 10),
        Err(RoutineDigestError::Invalid)
    );
    assert_eq!(
        routine_digest_json(b"domain", &true, 0),
        Err(RoutineDigestError::TooLarge)
    );
    // The writer streams directly into the hash; no full JSON allocation or
    // second serialization pass is needed, even for an oversized typed value.
    struct Counted<'a>(&'a std::cell::Cell<usize>, &'a str);
    impl Serialize for Counted<'_> {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            self.0.set(self.0.get() + 1);
            s.serialize_str(self.1)
        }
    }
    let calls = std::cell::Cell::new(0);
    let text = "x".repeat(100_000);
    assert_eq!(
        routine_digest_json(b"domain", &Counted(&calls, &text), 16),
        Err(RoutineDigestError::TooLarge)
    );
    assert_eq!(calls.get(), 1);
}
