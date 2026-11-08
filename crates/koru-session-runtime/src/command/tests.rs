// SPDX-License-Identifier: MPL-2.0

use super::*;

fn request(environment: EnvironmentPolicy) -> SpawnRequest {
    SpawnRequest {
        environment,
        ..SpawnRequest::new(["sh"], PathBuf::from("/"), GridSize::new(80, 24).unwrap())
    }
}

fn path_of(environment: &[(OsString, OsString)]) -> Vec<&OsString> {
    environment
        .iter()
        .filter(|(key, _)| same_environment_key(key, OsStr::new("PATH")))
        .map(|(_, value)| value)
        .collect()
}

/// **An inheriting session runs with the effective `PATH`**, once, in
/// place of this process's own.
#[test]
fn an_inheriting_session_gets_the_effective_path() {
    let environment =
        request(EnvironmentPolicy::Inherit).environment_with_path(|| "/effective:/path".into());
    assert_eq!(path_of(&environment), [&OsString::from("/effective:/path")]);
}

/// **A profile that names `PATH` decides it** — set or removed — and a
/// clean environment is never given one.
#[test]
fn a_profiles_own_path_wins_and_a_clean_environment_stays_clean() {
    let set = request(EnvironmentPolicy::InheritWith {
        set: BTreeMap::from([(OsString::from("PATH"), OsString::from("/profile"))]),
        remove: BTreeSet::new(),
    })
    .environment_with_path(|| "/effective".into());
    assert_eq!(path_of(&set), [&OsString::from("/profile")]);

    let removed = request(EnvironmentPolicy::InheritWith {
        set: BTreeMap::new(),
        remove: BTreeSet::from([OsString::from("PATH")]),
    })
    .environment_with_path(|| "/effective".into());
    assert!(path_of(&removed).is_empty());

    let other = request(EnvironmentPolicy::InheritWith {
        set: BTreeMap::from([(OsString::from("KORU_X"), OsString::from("1"))]),
        remove: BTreeSet::new(),
    })
    .environment_with_path(|| "/effective".into());
    assert_eq!(path_of(&other), [&OsString::from("/effective")]);

    let clean = request(EnvironmentPolicy::Clean)
        .environment_with_path(|| unreachable!("a clean environment never asks"));
    assert!(clean.is_empty());
}

/// With nothing to search, the inherited `PATH` is left as it was.
#[test]
fn an_empty_effective_path_leaves_the_inherited_one() {
    let environment = request(EnvironmentPolicy::Inherit).environment_with_path(OsString::new);
    let inherited = std::env::var_os("PATH");
    assert_eq!(path_of(&environment).first().copied(), inherited.as_ref());
}
