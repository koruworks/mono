// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn trusted_device_requires_existing_non_nil_uuid() {
    assert!(trusted_device("").is_none());
    assert!(trusted_device("caller-label").is_none());
    assert!(trusted_device("00000000-0000-0000-0000-000000000000").is_none());
    assert!(trusted_device("00000000000000000000000000000000").is_none());
    let id = uuid::Uuid::new_v4();
    assert_eq!(
        trusted_device(&id.to_string()),
        Some(DeviceId::from_uuid(id))
    );
}
