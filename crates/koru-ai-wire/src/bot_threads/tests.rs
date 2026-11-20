// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn routine_cards_preserve_exact_occurrences_and_old_cards_still_decode() {
    let reply = Uuid::new_v4();
    let mut value = serde_json::json!({
        "id":Uuid::new_v4(),"sequence":4,"message":Uuid::new_v4(),"kind":"thread",
        "title":"Weekly digest","thread":Uuid::new_v4(),"bot":Uuid::new_v4(),
        "bot_name":"Scout","workspace":null,"document":null,"messages":45,
        "outputs":3,"state":"active"
    });
    let old: Card = serde_json::from_value(value.clone()).unwrap();
    assert!(old.routine.is_none());
    assert!(old.dispatches.is_empty());
    value["routine"] = serde_json::json!({
        "routine":Uuid::new_v4(),"occurrence":Uuid::new_v4(),"at_ms":60_000,
        "manual":false,"reply":reply,
        "preview":{"message":reply,"markdown":"Actual returned words","truncated":false}
    });
    let card: Card = serde_json::from_value(value.clone()).unwrap();
    let result = card.routine.as_ref().unwrap();
    assert_eq!(result.reply, reply);
    assert_eq!(result.preview.as_ref().unwrap().message.as_uuid(), reply);
    assert_eq!(
        serde_json::to_value(card).unwrap()["routine"],
        value["routine"]
    );
}
