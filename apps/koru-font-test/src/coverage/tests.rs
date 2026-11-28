// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn neutral_defaults_and_explicit_parameters() {
    assert!(!Coverage::from_args(&[]).unwrap().active());
    let setting = Coverage::from_args(&[
        "--coverage-gamma=1.25".into(),
        "--coverage-cutoff=0.05".into(),
    ])
    .unwrap();
    assert_eq!(setting.gamma, 1.25);
    assert_eq!(setting.cutoff, 0.05);
    assert!(setting.active());

    let zero = Coverage::from_args(&["--coverage-gamma=0".into()]).unwrap();
    assert_eq!(zero.gamma, 0.0);
    assert!(zero.active());
}

#[test]
fn invalid_parameters_cannot_reach_the_shader() {
    for arg in [
        "--coverage-gamma=NaN",
        "--coverage-gamma=-0.1",
        "--coverage-gamma=4",
        "--coverage-cutoff=-0.1",
        "--coverage-cutoff=1",
        "--coverage-gamma=x",
        "--coverage-gama=1.2",
    ] {
        assert!(Coverage::from_args(&[arg.into()]).is_err(), "{arg}");
    }
}

#[test]
fn controls_cycle_back_to_neutral() {
    let mut setting = Coverage::default();
    let mut visited = Vec::new();
    for _ in 0..11 {
        setting.next_gamma();
        visited.push(setting.gamma);
    }
    assert_eq!(
        visited,
        vec![1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 0.0, 0.5, 0.75, 1.0]
    );
    for _ in 0..7 {
        setting.next_cutoff();
    }
    assert!(!setting.active());
}
