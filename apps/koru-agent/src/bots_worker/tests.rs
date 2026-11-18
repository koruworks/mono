// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn role_refuses_unknown_missing_duplicate_and_relative_arguments() {
    for args in [
        vec![],
        vec!["--root"],
        vec!["--root", "relative"],
        vec!["--root", "--help"],
        vec!["--unknown"],
    ] {
        assert!(parse(args.into_iter().map(String::from)).is_err());
    }
    let root = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        parse(["--root".into(), root.clone()]).unwrap(),
        PathBuf::from(&root)
    );
    assert!(parse(["--root".into(), root.clone(), "--root".into(), root]).is_err());
}
