// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn memory_node_ids_preserve_core_uuid_values() {
    for value in [
        "00000000-0000-0000-0000-000000000000",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "12345678-1234-4123-8123-123456789abc",
        "12345678-1234-5123-8123-123456789abc",
        "12345678-1234-7123-0123-123456789abc",
    ] {
        let id: BotMemoryNodeId = value.parse().unwrap();
        assert_eq!(id.as_uuid(), Uuid::parse_str(value).unwrap());
        assert_eq!(BotMemoryNodeId::from_uuid(id.as_uuid()), id);
        assert_eq!(
            serde_json::from_str::<BotMemoryNodeId>(&serde_json::to_string(&id).unwrap()).unwrap(),
            id
        );
        assert_eq!(id.to_storage(), value);
    }
    for value in [
        "12345678-1234-5123-8123-123456789ABC",
        "12345678123451238123123456789abc",
        "urn:uuid:12345678-1234-5123-8123-123456789abc",
        "invalid",
    ] {
        assert!(value.parse::<BotMemoryNodeId>().is_err());
        assert!(serde_json::from_value::<BotMemoryNodeId>(serde_json::json!(value)).is_err());
    }
    assert!(
        "12345678-1234-5123-8123-123456789abc"
            .parse::<koru_delegate_core::BlockNodeId>()
            .is_err()
    );
}
