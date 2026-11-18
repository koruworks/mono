// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_session_runtime::AttachmentPathResolver;
#[test]
fn paths_are_session_scoped_and_retirement_forgets_only_that_session() {
    let paths = Paths::default();
    let first = SessionId::mint();
    let second = SessionId::mint();
    let attachment = AttachmentId::mint();
    let path = PathBuf::from("synthetic-never-opened");
    paths
        .0
        .lock()
        .unwrap()
        .insert((first, attachment), path.clone());
    assert_eq!(paths.resolve(first, attachment).unwrap(), path);
    assert!(paths.resolve(second, attachment).is_err());
    paths
        .0
        .lock()
        .unwrap()
        .insert((second, attachment), path.clone());
    paths.remove(first.as_uuid());
    assert!(paths.resolve(first, attachment).is_err());
    assert_eq!(paths.resolve(second, attachment).unwrap(), path);
}
