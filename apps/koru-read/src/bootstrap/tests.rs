// SPDX-License-Identifier: MPL-2.0

use super::*;

fn bootstrap(channel: &str, launch_id: &str) -> WorkerBootstrap {
    WorkerBootstrap {
        bootstrap_version: BOOTSTRAP_VERSION,
        launch_id: launch_id.into(),
        channel: channel.into(),
    }
}

/// The line is read up to its newline and no further: what follows is the
/// pipe that signals the parent's death, and must be left for it.
#[test]
fn the_line_is_read_and_the_rest_of_the_pipe_is_left_alone() {
    let mut pipe = std::io::Cursor::new(
        b"{\"bootstrap_version\":1,\"launch_id\":\"launch-7\",\"channel\":\"nightly\"}\nrest"
            .to_vec(),
    );
    let read = read(&mut pipe).unwrap();
    assert_eq!(read, bootstrap("nightly", "launch-7"));
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut pipe, &mut rest).unwrap();
    assert_eq!(rest, "rest");
}

#[test]
fn a_line_that_is_not_the_shape_is_refused() {
    assert!(parse(b"{\"launch_id\":\"x\"}").is_err());
    assert!(parse(b"not json").is_err());
    let mut closed = std::io::Cursor::new(Vec::new());
    assert!(read(&mut closed).is_err());
}

#[test]
fn the_version_the_channel_and_the_launch_id_are_checked() {
    assert!(validate(&bootstrap("nightly", "launch-1"), "nightly").is_ok());
    assert!(validate(&bootstrap("stable", "launch-1"), "nightly").is_err());
    let mut old = bootstrap("nightly", "launch-1");
    old.bootstrap_version = BOOTSTRAP_VERSION + 1;
    assert!(validate(&old, "nightly").is_err());
    for bad in ["", "has space", "line\nbreak", &"x".repeat(129)] {
        assert!(
            validate(&bootstrap("nightly", bad), "nightly").is_err(),
            "{bad:?} was accepted as a launch id"
        );
    }
}
