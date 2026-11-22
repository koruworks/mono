// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn the_measured_settings_are_the_ones_used() {
    assert_eq!(DEFAULT_THREADS, 4);
    assert_eq!(REGION_BATCH, 1);
    assert_eq!(
        DETECTION,
        Detection {
            score_threshold: 0.2,
            box_threshold: 0.45,
            unclip_ratio: 1.4,
            max_candidates: 3000,
            max_side: 960,
        }
    );
}

#[test]
fn the_thread_setting_is_read_clamped_and_never_fatal() {
    assert_eq!(Settings::from_setting(None), Ok(Settings::default()));
    assert_eq!(Settings::from_setting(Some(" ")), Ok(Settings::default()));
    assert_eq!(Settings::from_setting(Some("2")).unwrap().threads(), 2);
    assert_eq!(Settings::from_setting(Some(" 8 ")).unwrap().threads(), 8);
    assert_eq!(Settings::from_setting(Some("0")).unwrap().threads(), 1);
    assert_eq!(
        Settings::from_setting(Some("4096")).unwrap().threads(),
        MAX_THREADS
    );
    assert_eq!(
        Settings::from_setting(Some("four")),
        Err(Settings::default())
    );
    assert_eq!(Settings::from_setting(Some("-1")), Err(Settings::default()));
}
