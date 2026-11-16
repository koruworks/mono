// SPDX-License-Identifier: MPL-2.0

//! M5: the private-operation row is content-free. It holds only the
//! attributes the registry lists, each an id, a closed token, a flag or a
//! size; and what a stage does not record, the row does not carry.

use koru_delegate_core::{PrivateEffectRefusal, PrivateOperationKind};
use koru_events::{AttrKind, AttrValue, registry};

use super::*;

const NAME: &str = "delegate.private_operation.settled";

pub(crate) fn private_fact(outcome: PrivateOutcome) -> PrivateOperationSettled {
    PrivateOperationSettled {
        bot: koru_delegate_core::BotId::from_uuid(v4(0x31)),
        operation: v4(0x32),
        connection: koru_delegate_core::ConnectionId::from_uuid(v4(0x33)),
        kind: PrivateOperationKind::ReadItem,
        outcome,
        decision: Some(v4(0x34).hyphenated().to_string().parse().unwrap()),
    }
}

fn chain() -> RunChain {
    RunChain::rejoin(&ctx(), run_ref(), event_id(0))
}

fn keys(envelope: &Envelope) -> BTreeSet<String> {
    envelope.attrs.keys().map(|key| key.to_string()).collect()
}

fn set(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|key| key.to_string()).collect()
}

#[test]
fn m5_events_content_free_the_settled_row_declares_exactly_the_listed_attributes() {
    let spec = registry::spec(NAME).expect("the row is registered");
    let declared: Vec<(&str, AttrKind, bool)> = spec
        .attrs
        .iter()
        .map(|attr| (attr.key, attr.kind, attr.required))
        .collect();
    assert_eq!(
        declared,
        vec![
            ("run", AttrKind::Id, true),
            ("bot", AttrKind::Id, true),
            ("operation", AttrKind::Id, true),
            ("connection", AttrKind::Id, true),
            ("kind", AttrKind::Token, true),
            ("stage", AttrKind::Token, true),
            ("refusal", AttrKind::Token, false),
            ("withheld", AttrKind::Flag, false),
            ("decision", AttrKind::Id, false),
            ("outcome_bytes", AttrKind::Count, false),
        ]
    );
    // No attribute kind of the row can hold text a person or a vendor wrote.
    for attr in spec.attrs {
        assert!(
            matches!(
                attr.kind,
                AttrKind::Id | AttrKind::Token | AttrKind::Flag | AttrKind::Count
            ),
            "{}",
            attr.key
        );
    }
}

#[test]
fn m5_events_content_free_each_stage_carries_only_what_it_records() {
    let always = ["run", "bot", "operation", "connection", "kind", "stage"];
    let confirmed = chain().private_operation_settled(
        stamp(40),
        private_fact(PrivateOutcome::Confirmed {
            outcome_bytes: 4096,
            withheld: true,
        }),
    );
    let mut expected = set(&always);
    expected.extend(set(&["withheld", "decision", "outcome_bytes"]));
    assert_eq!(keys(&confirmed), expected);
    assert_eq!(
        confirmed.attrs.get("stage"),
        Some(&AttrValue::Token("confirmed".into()))
    );
    assert_eq!(
        confirmed.attrs.get("withheld"),
        Some(&AttrValue::Flag(true))
    );
    assert_eq!(
        confirmed.attrs.get("outcome_bytes"),
        Some(&AttrValue::Count(4096))
    );

    let refused = chain().private_operation_settled(
        stamp(41),
        private_fact(PrivateOutcome::Refused {
            code: PrivateEffectRefusal::CredentialLocked,
        }),
    );
    let mut expected = set(&always);
    expected.extend(set(&["refusal", "decision"]));
    assert_eq!(keys(&refused), expected);
    assert_eq!(
        refused.attrs.get("refusal"),
        Some(&AttrValue::Token("credential_locked".into()))
    );

    // A granted read that ended unknown: no decision, and nothing but the
    // stage says how it ended.
    let mut granted = private_fact(PrivateOutcome::Unknown);
    granted.decision = None;
    let unknown = chain().private_operation_settled(stamp(42), granted);
    assert_eq!(keys(&unknown), set(&always));
    assert_eq!(
        unknown.attrs.get("stage"),
        Some(&AttrValue::Token("unknown".into()))
    );

    for envelope in [&confirmed, &refused, &unknown] {
        koru_events::validate(envelope).expect("the boundary accepts the row");
        assert_eq!(envelope.name.as_str(), NAME);
        // Every value is an id, a closed token, a flag or a count.
        for (key, value) in &envelope.attrs {
            assert!(
                matches!(
                    value,
                    AttrValue::Id(_)
                        | AttrValue::Token(_)
                        | AttrValue::Flag(_)
                        | AttrValue::Count(_)
                ),
                "{key}"
            );
        }
        // The Debug and the wire form name no stage a row cannot be in.
        let json = serde_json::to_string(envelope).unwrap();
        for open in ["prepared", "dispatched"] {
            assert!(!json.contains(open), "{open}");
        }
    }
}

#[test]
fn m5_events_content_free_a_retry_under_the_same_stamp_is_the_same_envelope() {
    let fact = private_fact(PrivateOutcome::Confirmed {
        outcome_bytes: 12,
        withheld: false,
    });
    let first = chain().private_operation_settled(stamp(43), fact);
    let retry = chain().private_operation_settled(stamp(43), fact);
    assert_eq!(first, retry);
    assert_eq!(first.event_id, stamp(43).event_id);
    assert_eq!(first.correlation_id, Some(event_id(0)));
}
