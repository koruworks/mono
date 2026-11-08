// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::cloud::object::causal::tests::{cp, fixture, segment, template};
use crate::cloud::object::tests::{bind_fixture, bytes, head, limits};
mod creation;
mod messages;

#[test]
fn checkpoint_from_recovered_content_preserves_exact_head_and_requires_its_coverage() {
    for index in 0..5 {
        let base = cp(index, 1, 0, "base", "base_state");
        let update = segment(index, 2, 1);
        let bound = bind_fixture(&update);
        let restored = restore_chain(
            bind_fixture(&base),
            CheckpointOrigin::FirstCloudCheckpoint,
            &[bound],
            head(bound),
            limits(),
        )
        .unwrap();
        let prototype = cp(index, 3, 2, "base", "base_state");
        let metadata = bind_fixture(&prototype).metadata();
        let (bytes, proof) = restored.stage_checkpoint(metadata).unwrap().into_parts();
        let checkpoint = bind_fixture(&bytes);
        assert_eq!(
            checkpoint.body().result_version,
            bound.body().result_version
        );
        assert_eq!(proof.head(), restored.proof().head());
        assert_eq!(proof.checkpoint_object_sequence(), 3);
        assert_eq!(proof.last_replayed_segment_sequence(), None);
        let receiver = LoroDoc::new();
        receiver.import(checkpoint.body().payload).unwrap();
        let original = LoroDoc::new();
        original.import(&restored.snapshot().unwrap()).unwrap();
        assert_eq!(receiver.get_deep_value(), original.get_deep_value());
        assert_eq!(receiver.state_vv(), original.state_vv());
        for wrong in [
            Metadata {
                covers_through_sequence: 1,
                ..metadata
            },
            Metadata {
                covers_through_sequence: 0,
                ..metadata
            },
            Metadata {
                sequence: 2,
                ..metadata
            },
            Metadata {
                object_id: bound.metadata().object_id,
                ..metadata
            },
        ] {
            assert!(restored.stage_checkpoint(wrong).is_err());
        }
        assert_eq!(restored.result_version(), bound.body().result_version);
    }
}

#[test]
fn first_checkpoints_require_full_history_and_preserve_received_snapshots() {
    for index in 0..5 {
        let raw = cp(index, 1, 0, "base", "base_state");
        let bound = bind_fixture(&raw);
        let (bytes, proof) =
            stage_initial_checkpoint(bound.metadata(), bound.body().payload, limits())
                .unwrap()
                .into_parts();
        assert_eq!(bind_fixture(&bytes).body().payload, bound.body().payload);
        assert_eq!(proof.checkpoint_object_sequence(), 1);
        let doc = LoroDoc::new();
        doc.import(bound.body().payload).unwrap();
        let frontiers = doc.state_frontiers();
        for mode in [
            ExportMode::all_updates(),
            ExportMode::shallow_snapshot(&frontiers),
        ] {
            assert!(
                stage_initial_checkpoint(bound.metadata(), &doc.export(mode).unwrap(), limits())
                    .is_err()
            );
        }
        let retry = Metadata {
            sequence: 2,
            ..bound.metadata()
        };
        let (_, retry_proof) = stage_initial_checkpoint(retry, bound.body().payload, limits())
            .unwrap()
            .into_parts();
        assert_eq!(retry_proof.checkpoint_object_sequence(), 2);
        let wrong = Metadata {
            sequence: 0,
            ..bound.metadata()
        };
        assert!(stage_initial_checkpoint(wrong, bound.body().payload, limits()).is_err());
    }
}

#[test]
fn stage_all_five_units_preserves_payload_and_recovered_head() {
    for index in 0..5 {
        let base = cp(index, 1, 0, "base", "base_state");
        let bound = bind_fixture(&base);
        let restored = restore_chain(
            bound,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[],
            head(bound),
            limits(),
        )
        .unwrap();
        let raw = segment(index, 2, 1);
        let incoming = bind_fixture(&raw);
        let reused = Metadata {
            object_id: bound.metadata().object_id,
            ..incoming.metadata()
        };
        assert!(matches!(
            restored.stage_segment(reused, incoming.body().payload),
            Err(Error::Chain)
        ));
        let staged = restored
            .stage_segment(incoming.metadata(), incoming.body().payload)
            .unwrap()
            .unwrap();
        let (bytes, proof) = staged.into_parts();
        let staged = bind_fixture(&bytes);
        assert_eq!(staged.body().payload, incoming.body().payload);
        assert_eq!(
            staged.body().predecessor_version,
            bound.body().result_version
        );
        assert_eq!(
            verify_chain(
                bound,
                CheckpointOrigin::FirstCloudCheckpoint,
                &[staged],
                head(staged),
                limits()
            )
            .unwrap(),
            proof
        );
        assert_eq!(restored.proof().head(), head(bound));
        let original = LoroDoc::new();
        original.import(&restored.snapshot().unwrap()).unwrap();
        assert_eq!(
            original.state_vv(),
            VersionVector::decode(bound.body().result_version).unwrap()
        );
        let committed = restore_chain(
            bound,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[staged],
            head(staged),
            limits(),
        )
        .unwrap();
        let next = Metadata {
            sequence: 3,
            ..incoming.metadata()
        };
        assert!(
            committed
                .stage_segment(next, incoming.body().payload)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn stage_refuses_missing_ancestors_foreign_scope_and_future_schema_without_mutation() {
    for index in 0..5 {
        let base = cp(index, 1, 0, "base", "base_state");
        let bound = bind_fixture(&base);
        let restored = restore_chain(
            bound,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[],
            head(bound),
            limits(),
        )
        .unwrap();
        let metadata = Metadata {
            kind: Kind::Segment,
            sequence: 2,
            ..template(index)
        };
        let future = segment(index, 2, 2);
        assert!(
            restored
                .stage_segment(metadata, bind_fixture(&future).body().payload)
                .is_err()
        );
        let altered = LoroDoc::new();
        altered.import(bound.body().payload).unwrap();
        altered
            .get_map("meta")
            .insert("schema_version", i64::MAX)
            .unwrap();
        altered.commit();
        assert!(
            restored
                .stage_segment(metadata, &altered.export(ExportMode::snapshot()).unwrap())
                .is_err()
        );
        let different = Metadata {
            cloud_unit_id: template((index + 1) % 5).cloud_unit_id,
            ..metadata
        };
        assert!(matches!(
            restored.stage_segment(different, bound.body().payload),
            Err(Error::Scope)
        ));
        assert!(restored.stage_segment(metadata, &[255]).is_err());
        assert_eq!(restored.result_version(), bound.body().result_version);
        if index >= 2 {
            let foreign = LoroDoc::new();
            foreign
                .import(&bytes(fixture(index)["base"].as_str().unwrap()))
                .unwrap();
            foreign
                .get_map("meta")
                .insert(
                    if index == 4 {
                        "owner_profile_id"
                    } else {
                        "profile_id"
                    },
                    "90000000-0000-4000-8000-000000000099",
                )
                .unwrap();
            foreign.commit();
            assert!(
                restored
                    .stage_segment(metadata, &foreign.export(ExportMode::snapshot()).unwrap())
                    .is_err()
            );
        }
    }
}
