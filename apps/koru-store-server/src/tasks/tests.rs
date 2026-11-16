// SPDX-License-Identifier: MPL-2.0

use super::*;

fn base() -> wire::TaskQuery {
    wire::TaskQuery::default()
}

/// **The six shared filters are one translation, not two.** The point of
/// routing them through the attribute port's translator is that a refusal has
/// the same wording and the same shape on both routes — so a caller who typed
/// `critical` is told the same thing whichever question they asked.
#[test]
fn a_shared_filter_is_refused_here_exactly_as_it_is_on_the_attribute_route() {
    let mine = translate(&wire::TaskQuery {
        min_priority: Some("critical".into()),
        ..base()
    });
    let theirs = crate::attributes::translate(&koru_store_wire::attributes::AttributeQuery {
        min_priority: Some("critical".into()),
        ..Default::default()
    });
    match (mine, theirs) {
        (
            Err(TaskServiceError::Invalid(mine)),
            Err(koru_store_api::routes::attributes::AttributeServiceError::Invalid(theirs)),
        ) => assert_eq!(mine, theirs, "one refusal, spelled once"),
        other => panic!("both routes should refuse the same token: {other:?}"),
    }
}

#[test]
fn a_tag_filter_must_hold_tag_ids() {
    assert!(matches!(
        translate(&wire::TaskQuery {
            tags: vec!["#mvp".into()],
            ..base()
        }),
        Err(TaskServiceError::Invalid(_))
    ));
}

/// A list filter is an id for the tag filter's reason: answering "no tasks" to
/// a *name* looks exactly like an empty list, and the list table owns names.
#[test]
fn a_list_filter_must_hold_a_list_id() {
    assert!(matches!(
        translate(&wire::TaskQuery {
            list: Some("Wedding".into()),
            ..base()
        }),
        Err(TaskServiceError::Invalid(_))
    ));
    let id = koru_core::ids::ListId::new();
    assert_eq!(
        translate(&wire::TaskQuery {
            list: Some(id.to_storage()),
            ..base()
        })
        .unwrap()
        .list,
        Some(id.to_storage())
    );
}

/// `list` and `bucket` are the grouping tokens, and an unknown one is refused
/// rather than silently answered ungrouped — a rail that asked for counts and
/// drew none would look like a store bug.
#[test]
fn the_groupings_are_list_and_bucket() {
    assert_eq!(group_by(None).unwrap(), None);
    assert_eq!(group_by(Some("list")).unwrap(), Some(GroupBy::List));
    assert_eq!(group_by(Some("bucket")).unwrap(), Some(GroupBy::Bucket));
    assert!(matches!(
        group_by(Some("priority")),
        Err(TaskServiceError::Invalid(_))
    ));
}

/// The windows a caller in Zurich sends on 2026-06-15 — every edge resolved on
/// the client, which is the whole shape.
fn zurich_windows() -> koru_store_wire::tasks::BucketWindows {
    koru_store_wire::tasks::BucketWindows {
        today: "2026-06-15".into(),
        horizon: "2026-06-22".into(),
        upcoming_horizon: "2026-07-15".into(),
        today_ends_ms: 1_781_215_200_000,
        horizon_ends_ms: 1_781_820_000_000,
        upcoming_ends_ms: 1_783_807_200_000,
        now_ms: 1_781_164_800_000,
        min_next_priority: "high".into(),
    }
}

/// **The five tokens are the domain's five**, so a caller's vocabulary and the
/// rule's are one list rather than two that agree today. An unknown token is
/// refused: a view that asked for Today and got the whole corpus would look
/// like the bucketing had quietly stopped working.
#[test]
fn a_bucket_token_is_the_domain_enum_and_an_unknown_one_is_refused() {
    for bucket in koru_projection::store::Bucket::ALL {
        assert_eq!(
            bucket_token(Some(bucket.as_str())).unwrap(),
            Some(bucket),
            "{bucket:?} is spelled the same on the wire as in the rule"
        );
    }
    assert_eq!(
        koru_store_wire::tasks::BUCKETS.len(),
        koru_projection::store::Bucket::ALL.len(),
        "the wire's vocabulary and the domain's are the same length"
    );
    assert_eq!(bucket_token(None).unwrap(), None);
    assert!(matches!(
        bucket_token(Some("tomorrow")),
        Err(TaskServiceError::Invalid(_))
    ));
}

/// **A bucket needs windows, at either door.** Paging one bucket and counting
/// all five both depend on the reader's zone, and this store has none — so the
/// refusal is one sentence rather than a date invented in two places.
#[test]
fn a_bucket_without_windows_is_refused_at_either_door() {
    let bucketed = translate(&wire::TaskQuery {
        bucket: Some("today".into()),
        ..base()
    })
    .unwrap();
    assert!(matches!(
        windows_are_present(&bucketed, None),
        Err(TaskServiceError::Invalid(_))
    ));
    assert!(matches!(
        windows_are_present(&translate(&base()).unwrap(), Some(GroupBy::Bucket)),
        Err(TaskServiceError::Invalid(_))
    ));

    let armed = translate(&wire::TaskQuery {
        bucket: Some("today".into()),
        bucket_windows: Some(zurich_windows()),
        ..base()
    })
    .unwrap();
    assert_eq!(armed.bucket, Some(koru_projection::store::Bucket::Today));
    assert!(windows_are_present(&armed, Some(GroupBy::Bucket)).is_ok());
    // Grouping by bucket while paging none is a rail over an ungrouped list,
    // and the windows it carries are enough for both.
    let rail = translate(&wire::TaskQuery {
        bucket_windows: Some(zurich_windows()),
        ..base()
    })
    .unwrap();
    assert!(windows_are_present(&rail, Some(GroupBy::Bucket)).is_ok());
}

/// The windows are the caller's numbers and the refusals are this port's: a
/// malformed day, transposed horizons or an unknown priority token is a 400
/// with a reason, because each of them would otherwise answer a wrong question
/// plausibly.
#[test]
fn malformed_windows_are_refused_with_a_reason() {
    let refused = |windows: koru_store_wire::tasks::BucketWindows| {
        matches!(
            translate(&wire::TaskQuery {
                bucket_windows: Some(windows),
                ..base()
            }),
            Err(TaskServiceError::Invalid(_))
        )
    };
    assert!(refused(koru_store_wire::tasks::BucketWindows {
        min_next_priority: "important".into(),
        ..zurich_windows()
    }));
    assert!(refused(koru_store_wire::tasks::BucketWindows {
        today: "2026-6-15".into(),
        ..zurich_windows()
    }));
    assert!(refused(koru_store_wire::tasks::BucketWindows {
        horizon: "2026-08-01".into(),
        ..zurich_windows()
    }));
    assert!(refused(koru_store_wire::tasks::BucketWindows {
        horizon_ends_ms: 1_999_999_999_999,
        ..zurich_windows()
    }));

    let good = translate(&wire::TaskQuery {
        bucket_windows: Some(zurich_windows()),
        ..base()
    })
    .unwrap();
    let windows = good.windows.expect("the windows survive translation");
    assert_eq!(windows.today, "2026-06-15");
    assert_eq!(
        windows.min_next_priority,
        koru_core::attributes::Priority::High,
        "SPEC §7's floor is the caller's value, carried rather than compiled in"
    );
    assert_eq!(windows.now_ms, zurich_windows().now_ms);
}

/// **Absence is `Open`, and `Due` is the order.** Both in the query and in the
/// data: a node that is a task only because it carries a due date has nobody to
/// write it a lifecycle, and it is still an answer to what to do next.
#[test]
fn an_empty_query_asks_for_open_tasks_by_due_date() {
    let translated = translate(&base()).unwrap();
    assert_eq!(translated.lifecycle, LifecycleFilter::Open);
    assert_eq!(translated.sort, TaskSort::Due);
    assert!(!translated.include_trashed);
    assert_eq!(translated.after, None);
}

#[test]
fn the_three_lifecycles_and_the_three_sorts_survive_translation() {
    for (from, to) in [
        (wire::LifecycleFilter::Open, LifecycleFilter::Open),
        (wire::LifecycleFilter::Completed, LifecycleFilter::Completed),
        (wire::LifecycleFilter::All, LifecycleFilter::All),
    ] {
        let translated = translate(&wire::TaskQuery {
            lifecycle: from,
            ..base()
        })
        .unwrap();
        assert_eq!(translated.lifecycle, to);
    }
    for (from, to) in [
        (wire::TaskSort::Due, TaskSort::Due),
        (wire::TaskSort::Priority, TaskSort::Priority),
        (wire::TaskSort::Modified, TaskSort::Modified),
    ] {
        let translated = translate(&wire::TaskQuery {
            sort: from,
            ..base()
        })
        .unwrap();
        assert_eq!(translated.sort, to);
    }
}

/// **The port does not clamp.** `routes::tasks::query` already did it, at the
/// boundary; a second clamp here would be a second answer to one question, and
/// the first one to drift would be silent. What arrives is what is used.
#[test]
fn the_page_size_arrives_already_clamped_and_is_used_as_given() {
    let translated = translate(&wire::TaskQuery { limit: 7, ..base() }).unwrap();
    assert_eq!(translated.limit, 7);
}

/// The cursor is carried, never re-derived here: `Projections::task_rows` looks
/// the reference up against the *current* filter, which is what makes a page
/// boundary survive a task being completed between pages.
#[test]
fn the_cursor_is_handed_down_untouched() {
    let translated = translate(&wire::TaskQuery {
        after: Some("task:w:d:n".into()),
        ..base()
    })
    .unwrap();
    assert_eq!(translated.after.as_deref(), Some("task:w:d:n"));
}

// ---------------------------------------------------------- covering

fn a_requirement(peer: &str, counter: i64) -> wire::RevisionRequirement {
    wire::RevisionRequirement {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        peer: peer.to_owned(),
        counter,
    }
}

fn a_real_receipt() -> wire::RevisionRequirement {
    a_requirement("1", 3)
}

#[test]
fn a_real_requirement_decodes_and_keeps_its_document() {
    let requirement = a_real_receipt();
    let decoded = requirements(std::slice::from_ref(&requirement)).unwrap();
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].document, requirement.document);
    assert_eq!((decoded[0].peer, decoded[0].counter), (1, 3));
}

/// **A receipt from the top of the id range survives the round trip.** Loro
/// mints peer ids across the whole `u64`, and a JSON number is an `f64` — so
/// the id travels as a string, and this is the test that says why.
#[test]
fn a_peer_id_above_the_float_range_keeps_every_bit() {
    let peer = u64::MAX - 12345;
    let decoded = requirements(&[a_requirement(&peer.to_string(), 1)]).unwrap();
    assert_eq!(decoded[0].peer, peer);
}

/// A malformed requirement is a client bug, and the answer that surfaces one
/// is a refusal — not a quiet *uncovered* that the client would wait on.
///
/// **There is no length rule left to test.** A receipt is two integers, so the
/// shape that used to grow past a limit — and refuse every task query once a
/// document had enough writers — cannot arise.
#[test]
fn a_malformed_requirement_refuses_the_request() {
    for bad in ["abc", "", "-1", "99999999999999999999999999"] {
        assert!(
            matches!(
                requirements(&[a_requirement(bad, 1)]),
                Err(TaskServiceError::Invalid(_))
            ),
            "{bad} should refuse as a peer"
        );
    }
    assert!(
        matches!(
            requirements(&[a_requirement("1", -1)]),
            Err(TaskServiceError::Invalid(_))
        ),
        "a negative counter is not an operation"
    );
}

#[test]
fn a_query_may_wait_on_at_most_sixteen_writes() {
    let many: Vec<_> = (0..=wire::MAX_COVERING).map(|_| a_real_receipt()).collect();
    assert!(matches!(
        requirements(&many),
        Err(TaskServiceError::Invalid(_))
    ));
    assert!(requirements(&many[..wire::MAX_COVERING]).is_ok());
}

// ------------------------------------------------ exactly these tasks (`only`)

fn a_ref() -> wire::TaskRef {
    wire::TaskRef {
        document: koru_store_core::ids::DocumentId::new(),
        node: koru_core::NodeId::new().to_storage(),
    }
}

fn selecting(only: Vec<wire::TaskRef>) -> wire::TaskQuery {
    wire::TaskQuery {
        workspaces: vec![koru_store_core::ids::WorkspaceId::new()],
        only: Some(only),
        ..base()
    }
}

/// The selector is exclusive: every filter it would override is refused
/// beside it rather than quietly dropped.
#[test]
fn a_selector_is_refused_beside_any_filter_it_would_override() {
    assert!(translate(&selecting(vec![a_ref()])).is_ok());

    let refused = |query: wire::TaskQuery| {
        matches!(translate(&query), Err(TaskServiceError::Invalid(_)))
    };
    assert!(refused(selecting(Vec::new())), "an empty selector");
    assert!(
        refused(selecting((0..=wire::MAX_ONLY).map(|_| a_ref()).collect())),
        "more than fifty"
    );
    assert!(
        refused(selecting(vec![wire::TaskRef {
            node: "not-a-node".to_owned(),
            ..a_ref()
        }])),
        "an id that does not parse"
    );
    assert!(
        refused(wire::TaskQuery {
            workspaces: Vec::new(),
            ..selecting(vec![a_ref()])
        }),
        "no workspace"
    );
    assert!(
        refused(wire::TaskQuery {
            workspaces: vec![
                koru_store_core::ids::WorkspaceId::new(),
                koru_store_core::ids::WorkspaceId::new()
            ],
            ..selecting(vec![a_ref()])
        }),
        "two workspaces"
    );
    let with = |edit: fn(&mut wire::TaskQuery)| {
        let mut query = selecting(vec![a_ref()]);
        edit(&mut query);
        query
    };
    for (what, query) in [
        ("bucket", with(|q| q.bucket = Some("today".into()))),
        ("list", with(|q| q.list = Some(koru_core::ids::ListId::new().to_storage()))),
        ("priority", with(|q| q.priority = Some("high".into()))),
        ("min_priority", with(|q| q.min_priority = Some("high".into()))),
        ("tags", with(|q| q.tags = vec!["fun".into()])),
        ("has", with(|q| q.has = Some("koru.due".into()))),
        ("missing", with(|q| q.missing = Some("koru.due".into()))),
        ("after", with(|q| q.after = Some("task:x".into()))),
        ("group_by", with(|q| q.group_by = Some("list".into()))),
    ] {
        assert!(refused(query), "`only` with {what}");
    }
}

/// What a selector leaves alone: the lifecycle and the completed windows are
/// carried but not applied, the trash flag applies as always, and windows are
/// welcome — they are what fills a row's bucket.
#[test]
fn a_selector_carries_what_it_does_not_apply() {
    let query = wire::TaskQuery {
        lifecycle: wire::LifecycleFilter::Open,
        completed_on_or_after: Some("2026-08-07".into()),
        include_trashed: true,
        limit: 3,
        ..selecting(vec![a_ref(), a_ref()])
    };
    let translated = translate(&query).unwrap();
    assert_eq!(translated.only.as_ref().map(Vec::len), Some(2));
    assert!(translated.include_trashed);
    // A query that names nothing has no selector.
    assert!(translate(&base()).unwrap().only.is_none());
}
