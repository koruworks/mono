// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_replication::cloud::transfer::removal;

#[tokio::test(start_paused = true)]
async fn removal_read_uses_idle_carrier_without_opening_content() {
    let (client, mut reader, mut writer, _, scope, budget) =
        super::super::super::tests::pair_with_workspace(true).await;
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    let mut sync = Reconciler::new(
        client,
        move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(std::future::ready(Ok(Answer::Ready(
                VersionVector::default(),
            ))))
        },
        |_| -> Result<std::future::Ready<Result<Answer<Option<Export>>, Error>>, Error> {
            panic!("removal read must not export content")
        },
    );
    let (answer, ()) = tokio::join!(sync.removal_exchange(removal::read(12)), async {
        let deadline = Instant::now() + Duration::from_secs(10);
        let request = records::read(&mut reader, deadline, &budget).await.unwrap();
        assert!(matches!(
            request.record(),
            Record::Control(Control::CloudRemovalsRead { cursor: 12, .. })
        ));
        records::write(
            &mut writer,
            &Record::Control(removal::page(vec![], 12)),
            deadline,
            &budget,
        )
        .await
        .unwrap();
    });
    assert!(matches!(
        answer.unwrap().control,
        Control::CloudRemovals {
            next_cursor: 12,
            ..
        }
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    let unit = Unit::Document {
        workspace_id: koru_device::cloud::CloudId::from_bytes(
            scope.eligibility_workspace.as_uuid().into_bytes(),
        ),
        document_id: koru_device::cloud::CloudId::from_bytes(
            koru_store_core::DocumentId::new().as_uuid().into_bytes(),
        ),
    };
    // A removal exchange during a content pass must be refused before its
    // answer can interleave with that pass or open another Store job.
    sync.start(unit).unwrap();
    assert!(matches!(
        sync.removal_exchange(removal::read(12)).await,
        Err(Error::Invalid)
    ));
    assert!(sync.client.removal.is_none());
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}
