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
}

#[test]
fn invalid_parameters_cannot_reach_the_shader() {
    for arg in [
        "--coverage-gamma=NaN",
        "--coverage-gamma=0",
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
    for _ in 0..6 {
        setting.next_gamma();
    }
    for _ in 0..7 {
        setting.next_cutoff();
    }
    assert!(!setting.active());
}
