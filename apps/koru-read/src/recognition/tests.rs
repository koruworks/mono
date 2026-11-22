// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures::fake_recognition;

/// **The model is loaded by the first page that needs it**, and then lent out
/// and given back, not loaded again.
#[test]
fn the_reader_is_loaded_lazily_once_and_reused() {
    let (recognition, _) = fake_recognition(|_, _| String::new());
    assert!(recognition.available());
    assert_eq!(recognition.loads(), 0, "nothing is loaded at start");
    let reader = recognition.take().unwrap();
    assert_eq!(recognition.loads(), 1);
    recognition.give_back(reader);
    let reader = recognition.take().unwrap();
    assert_eq!(recognition.loads(), 1, "given back, so reused");
    // A reader that was not given back (its page was abandoned) is replaced.
    drop(reader);
    let _ = recognition.take().unwrap();
    assert_eq!(recognition.loads(), 2);
}

/// A model that will not load makes recognition unavailable for the rest of
/// the process, so `Status` says so and nothing tries again.
#[test]
fn a_model_that_will_not_load_is_unavailable_from_then_on() {
    let recognition = Recognition::with_loader(|| {
        Err(ReadError {
            code: FailureCode::Internal,
            message: "the recognizer has the wrong number of classes".into(),
        })
    });
    assert!(recognition.available());
    assert_eq!(recognition.take().err(), Some(FailureCode::NeedsReader));
    assert!(!recognition.available());
    assert_eq!(recognition.take().err(), Some(FailureCode::NeedsReader));
}

/// **Why there is no reader decides the answer**: a build with recognition
/// and no model waits for the reader; a build without cannot.
#[test]
fn no_reader_answers_needs_reader_or_unsupported_by_the_build() {
    let waiting = Recognition::none(true);
    assert!(!waiting.available());
    assert_eq!(waiting.absent(), FailureCode::NeedsReader);
    assert_eq!(waiting.take().err(), Some(FailureCode::NeedsReader));
    let unbuilt = Recognition::none(false);
    assert_eq!(unbuilt.absent(), FailureCode::Unsupported);
    assert_eq!(unbuilt.take().err(), Some(FailureCode::Unsupported));
}

/// **An ordinary build links no runtime**: `recognition` is off by default
/// and is the only thing that turns on the adapter's `runtime`, so
/// `cargo test --workspace` on a machine without the ONNX Runtime library
/// links and runs.
#[test]
fn recognition_is_a_feature_off_by_default() {
    let manifest = include_str!("../../Cargo.toml");
    assert!(manifest.contains("\ndefault = []\n"));
    assert!(manifest.contains("\nrecognition = [\"koru-read-paddle/runtime\"]\n"));
    let dependency = manifest
        .lines()
        .find(|line| line.starts_with("koru-read-paddle = "))
        .expect("the adapter is a dependency");
    assert!(
        !dependency.contains("features"),
        "the adapter's runtime is the feature's to turn on, never the dependency's"
    );
}

/// No model given: no recognition, and nothing on disk is looked at.
#[test]
fn the_product_without_a_model_has_no_recognition() {
    let recognition = Recognition::product(None, Settings::default());
    assert!(!recognition.available());
    assert_eq!(
        recognition.absent(),
        if koru_read_paddle::RECOGNITION_BUILT {
            FailureCode::NeedsReader
        } else {
            FailureCode::Unsupported
        }
    );
}
