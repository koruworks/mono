// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn only_the_published_generation_is_authorized() {
    let projection = AuthorityProjection::default();
    let run = RunId::mint();
    let first = AttemptGeneration::FIRST;
    let second = first.next();
    assert!(projection.resolve(run, first).is_err());
    projection.publish(run, first).unwrap();
    assert!(projection.resolve(run, first).is_ok());
    assert!(projection.resolve(run, second).is_err());
    projection.publish(run, second).unwrap();
    assert!(projection.resolve(run, first).is_err());
    assert!(projection.resolve(run, second).is_ok());
    assert!(projection.publish(run, first).is_err());
    assert!(projection.resolve(run, second).is_ok());
    projection.remove(run);
    assert!(projection.resolve(run, second).is_err());
}
