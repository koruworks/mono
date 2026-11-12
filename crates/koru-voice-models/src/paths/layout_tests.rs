// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::fixtures;

/// Voice names a model directory with its own `ModelId`; the store underneath
/// names it with the generic one. They must be the same directory, or voice
/// would look for models where the store never put them.
#[test]
fn voices_model_directory_is_the_stores() {
    let paths = ModelPaths::at("/cache");
    let descriptor = fixtures::descriptor();
    let generic = descriptor.to_model().unwrap();
    assert_eq!(
        paths.model_dir(&descriptor),
        paths.generic().model_dir(&generic)
    );
    assert_eq!(
        paths.version_dir(&descriptor.id, &descriptor.version),
        paths.generic().version_dir(&generic.id, &generic.version)
    );
}
