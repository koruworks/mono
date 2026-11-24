// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn only_a_vouched_time_sweeps_a_cloud_workspace() {
    assert_eq!(clock(TransportMode::LocalOnly, None, 7), Some(7));
    assert_eq!(
        clock(TransportMode::Cloud, None, 7),
        None,
        "waits for a session"
    );
    assert_eq!(
        clock(TransportMode::Cloud, Some(3), 7),
        Some(3),
        "never the device clock"
    );
    assert_eq!(clock(TransportMode::P2p, Some(3), 7), None);
}

#[test]
fn the_trusted_time_only_moves_forward() {
    let times = TrustedTimes::default();
    let workspace = WorkspaceId::new();
    times.record(workspace, 10);
    times.record(workspace, 5);
    assert_eq!(times.get(workspace), Some(10));
    times.record(workspace, 12);
    assert_eq!(times.get(workspace), Some(12));
    assert_eq!(times.get(WorkspaceId::new()), None);
}
