// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn installed_instructions_follow_the_platform_data_directory_without_probing() {
    assert_eq!(
        data_directory(Path::new("/installed/koru-agent.exe"), false),
        Some(PathBuf::from("/installed"))
    );
    assert_eq!(
        data_directory(Path::new("/Koru.app/Contents/MacOS/koru-agent"), true),
        Some(PathBuf::from("/Koru.app/Contents/Resources"))
    );
    assert_eq!(
        data_directory(Path::new("/source/target/debug/koru-agent"), true),
        Some(PathBuf::from("/source/target/debug"))
    );
}
