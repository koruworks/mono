// SPDX-License-Identifier: MPL-2.0

//! M6 cohort `m6_events_content_free`: the two child-request rows hold only
//! the attributes the registry lists, each an id, a count or a closed token.

use koru_delegate_core::children::{ChildClock, ChildRequestState, ChildSettledBy};
use koru_events::{AttrKind, AttrValue, registry};

use super::*;

const REQUESTED: &str = "delegate.child.requested";
const CHANGED: &str = "delegate.child.changed";

pub(crate) fn child_ref(revision: u64) -> ChildRequestRef {
    ChildRequestRef {
        request: v4(0x41).hyphenated().to_string().parse().unwrap(),
        child_run: koru_delegate_core::RunId::from_uuid(v4(0x42)),
        parent_bot: koru_delegate_core::BotId::from_uuid(v4(0x43)),
        recipient_bot: koru_delegate_core::BotId::from_uuid(v4(0x44)),
        revision,
    }
}

pub(crate) fn child_change(
    revision: u64,
    state: ChildRequestState,
    clock: ChildClock,
    settled_by: Option<ChildSettledBy>,
) -> ChildRequestChanged {
    ChildRequestChanged {
        request: child_ref(revision),
        state,
        clock,
        settled_by,
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

const SHARED: [(&str, AttrKind, bool); 6] = [
    ("run", AttrKind::Id, true),
    ("request", AttrKind::Id, true),
    ("child_run", AttrKind::Id, true),
    ("parent_bot", AttrKind::Id, true),
    ("recipient_bot", AttrKind::Id, true),
    ("revision", AttrKind::Count, true),
];

#[test]
fn m6_events_content_free_both_rows_declare_exactly_the_listed_attributes() {
    let declared = |name: &str| -> Vec<(&'static str, AttrKind, bool)> {
        registry::spec(name)
            .expect("the row is registered")
            .attrs
            .iter()
            .map(|attr| (attr.key, attr.kind, attr.required))
            .collect()
    };
    assert_eq!(declared(REQUESTED), SHARED.to_vec());
    let mut changed = SHARED.to_vec();
    changed.extend([
        ("state", AttrKind::Token, true),
        ("clock", AttrKind::Token, true),
        ("settled_by", AttrKind::Token, false),
    ]);
    assert_eq!(declared(CHANGED), changed);
    // No attribute kind of either row can hold text, a time or a size.
    for name in [REQUESTED, CHANGED] {
        let spec = registry::spec(name).unwrap();
        assert_eq!(spec.version, 1);
        for attr in spec.attrs {
            assert!(
                matches!(attr.kind, AttrKind::Id | AttrKind::Token | AttrKind::Count),
                "{}",
                attr.key
            );
            // Nothing names a page, a version, a title, a time or a deadline.
            for forbidden in [
                "page", "document", "version", "title", "name", "path", "ms", "deadline",
            ] {
                assert!(!attr.key.contains(forbidden), "{}", attr.key);
            }
        }
    }
}

#[test]
fn m6_events_content_free_each_row_carries_only_what_its_revision_records() {
    let requested = chain().child_requested(stamp(50), child_ref(1));
    assert_eq!(
        keys(&requested),
        set(&[
            "run",
            "request",
            "child_run",
            "parent_bot",
            "recipient_bot",
            "revision"
        ])
    );
    assert_eq!(requested.attrs.get("revision"), Some(&AttrValue::Count(1)));
    assert_eq!(
        requested.attrs.get("run"),
        Some(&AttrValue::Id(run_ref().run.as_uuid())),
        "the chain is the parent Run's"
    );

    let paused = chain().child_changed(
        stamp(51),
        child_change(2, ChildRequestState::Open, ChildClock::Paused, None),
    );
    assert_eq!(
        keys(&paused),
        set(&[
            "run",
            "request",
            "child_run",
            "parent_bot",
            "recipient_bot",
            "revision",
            "state",
            "clock"
        ])
    );
    assert_eq!(
        paused.attrs.get("clock"),
        Some(&AttrValue::Token("paused".into()))
    );
    assert_eq!(
        paused.attrs.get("state"),
        Some(&AttrValue::Token("open".into()))
    );

    for settled_by in [
        ChildSettledBy::ChildEnded,
        ChildSettledBy::TimedOut,
        ChildSettledBy::ParentStopped,
    ] {
        let settled = chain().child_changed(
            stamp(52),
            child_change(
                3,
                ChildRequestState::Settled,
                ChildClock::Stopped,
                Some(settled_by),
            ),
        );
        assert_eq!(
            settled.attrs.get("settled_by"),
            Some(&AttrValue::Token(settled_by.token().into()))
        );
        assert_eq!(
            settled.attrs.get("state"),
            Some(&AttrValue::Token("settled".into()))
        );
        assert_eq!(
            settled.attrs.get("clock"),
            Some(&AttrValue::Token("stopped".into()))
        );
        koru_events::validate(&settled).expect("the registry accepts the row");
    }
    koru_events::validate(&requested).unwrap();
    koru_events::validate(&paused).unwrap();
}

#[test]
fn m6_events_content_free_the_tokens_equal_the_registered_vocabularies() {
    use crate::tokens::RowToken;
    let vocabulary = |name: &str, key: &str| -> Vec<&'static str> {
        registry::spec(name)
            .unwrap()
            .attrs
            .iter()
            .find(|attr| attr.key == key)
            .map(|attr| attr.vocabulary.to_vec())
            .expect("the attribute is declared")
    };
    assert_eq!(
        vocabulary(CHANGED, "state"),
        [ChildRequestState::Open, ChildRequestState::Settled].map(RowToken::token)
    );
    assert_eq!(
        vocabulary(CHANGED, "clock"),
        [ChildClock::Running, ChildClock::Paused, ChildClock::Stopped].map(RowToken::token)
    );
    assert_eq!(
        vocabulary(CHANGED, "settled_by"),
        [
            ChildSettledBy::ChildEnded,
            ChildSettledBy::TimedOut,
            ChildSettledBy::ParentStopped
        ]
        .map(RowToken::token)
    );
    // The row's spelling is the vocabulary's own serde spelling.
    for clock in [ChildClock::Running, ChildClock::Paused, ChildClock::Stopped] {
        assert_eq!(
            serde_json::to_value(clock).unwrap(),
            serde_json::json!(clock.token())
        );
    }
}
