// SPDX-License-Identifier: MPL-2.0

#[test]
fn bot_role_refuses_missing_nil_or_extra_arguments() {
    let id = uuid::Uuid::new_v4();
    assert_eq!(super::parse_bot(&["--bot".into(), id.to_string()]), Ok(id));
    assert!(super::parse_bot(&[]).is_err());
    assert!(super::parse_bot(&["--bot".into(), uuid::Uuid::nil().to_string()]).is_err());
    assert!(super::parse_bot(&["--bot".into(), id.to_string(), "extra".into()]).is_err());
}

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
