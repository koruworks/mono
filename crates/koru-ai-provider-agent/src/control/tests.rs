// SPDX-License-Identifier: MPL-2.0

use super::*;

/// D7's throttle: a refused model asks for a re-probe once per window,
/// shared with the failed-readiness re-probe, per agent.
#[test]
fn a_refused_model_re_probes_its_agent_at_most_once_per_window() {
    let opencode: ProviderKind = "opencode".parse().unwrap();
    let claude: ProviderKind = "claude".parse().unwrap();
    let now = Instant::now();
    let mut refreshed = HashMap::new();
    assert!(refresh_due(&refreshed, &opencode, now), "never asked");
    refreshed.insert(opencode.clone(), now);
    assert!(!refresh_due(&refreshed, &opencode, now));
    assert!(!refresh_due(
        &refreshed,
        &opencode,
        now + REFRESH_WINDOW - Duration::from_millis(1)
    ));
    assert!(refresh_due(&refreshed, &opencode, now + REFRESH_WINDOW));
    assert!(
        refresh_due(&refreshed, &claude, now),
        "another agent's own window"
    );
}
