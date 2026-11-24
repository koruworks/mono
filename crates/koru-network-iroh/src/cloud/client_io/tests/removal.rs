// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_replication::cloud::transfer::removal;

const WAIT: Duration = Duration::from_millis(transfer::CONTROL_WAIT_MS);

fn event() -> removal::Event {
    removal::Event {
        event_id: uuid::Uuid::new_v4(),
        document_id: uuid::Uuid::new_v4(),
        root_id: uuid::Uuid::new_v4(),
        version: None,
        by: "other device".into(),
    }
}

async fn answer(
    client: &mut TestClient,
    reader: &mut ReadHalf<DuplexStream>,
    writer: &mut WriteHalf<DuplexStream>,
    budget: &MemoryBudget,
    request: Control,
    response: Control,
) -> Result<RemovalMessage, Error> {
    let expected = request.clone();
    let (result, ()) = tokio::join!(client.removal_exchange(request), async {
        let received = records::read(reader, Instant::now() + WAIT, budget)
            .await
            .unwrap();
        assert!(matches!(received.record(), Record::Control(value) if value == &expected));
        records::write(
            writer,
            &Record::Control(response),
            Instant::now() + WAIT,
            budget,
        )
        .await
        .unwrap();
    });
    result
}

#[tokio::test(start_paused = true)]
async fn probe_remove_and_read_match_their_exact_answers() {
    let (mut client, mut reader, mut writer, _, _, budget) = pair_with_workspace(true).await;
    let probe = answer(
        &mut client,
        &mut reader,
        &mut writer,
        &budget,
        removal::read(0),
        removal::page(vec![], 0),
    )
    .await
    .unwrap();
    assert!(
        matches!(probe.control, Control::CloudRemovals { events, next_cursor: 0, .. } if events.is_empty())
    );
    let event = event();
    let ack = answer(
        &mut client,
        &mut reader,
        &mut writer,
        &budget,
        removal::request(event.clone()),
        Control::CloudDocumentRemoved {
            version: Version1,
            event_id: event.event_id,
            document_id: event.document_id,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(ack.control, Control::CloudDocumentRemoved { event_id, .. } if event_id == event.event_id)
    );
    let entry = removal::Entry {
        sequence: 7,
        document_id: event.document_id,
        state: removal::State::Deleting,
        event,
    };
    let page = answer(
        &mut client,
        &mut reader,
        &mut writer,
        &budget,
        removal::read(3),
        removal::page(vec![entry.clone()], 7),
    )
    .await
    .unwrap();
    assert!(
        matches!(page.control, Control::CloudRemovals { events, next_cursor: 7, .. } if events == vec![entry])
    );
    assert!(client.removal.is_none());
}

#[tokio::test(start_paused = true)]
async fn canceled_response_wait_keeps_deadline_and_does_not_resend() {
    let (mut client, mut reader, _writer, _, _, budget) = pair_with_workspace(true).await;
    let began = Instant::now();
    let request = removal::read(5);
    let (wait, received) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(3),
            client.removal_exchange(request.clone())
        ),
        records::read(&mut reader, began + WAIT, &budget),
    );
    assert!(wait.is_err());
    assert!(matches!(
        received.unwrap().record(),
        Record::Control(Control::CloudRemovalsRead { cursor: 5, .. })
    ));
    assert!(client.writing_deadline.is_none(), "the request has flushed");
    assert!(
        !client.available_writer(),
        "pending answer owns the carrier"
    );
    assert!(!inspection::idle(&client));
    tokio::select! {
        result = client.removal_exchange(request) => assert!(matches!(result, Err(Error::Timeout))),
        _ = records::read(&mut reader, began + WAIT * 2, &budget) => panic!("resumed wait sent a duplicate request"),
    }
    assert_eq!(
        began.elapsed(),
        WAIT,
        "response uses the first request's deadline"
    );
    assert!(client.removal.is_none());
}

#[tokio::test(start_paused = true)]
async fn response_deadline_includes_waiting_for_the_writer() {
    let (mut client, mut reader, _writer, _, scope, budget) = pair_with_workspace(true).await;
    let unit = Unit::Document {
        workspace_id: CloudId::from_bytes(scope.eligibility_workspace.as_uuid().into_bytes()),
        document_id: CloudId::from_bytes(DocumentId::new().as_uuid().into_bytes()),
    };
    // The earlier metadata frame cannot fit in the duplex until the worker
    // starts reading. The removal request waits behind it for six seconds.
    let mut version = loro::VersionVector::default();
    for peer in 1u64..=2048 {
        version.set_last(loro::ID::new(peer.wrapping_mul(0x9e3779b97f4a7c15), 1));
    }
    let version = version.encode();
    assert!(version.len() > 4096);
    client.join(unit, version).unwrap();
    let began = Instant::now();
    let (result, ()) = tokio::join!(client.removal_exchange(removal::read(0)), async {
        tokio::time::sleep(Duration::from_secs(6)).await;
        let previous = records::read(&mut reader, began + WAIT * 2, &budget)
            .await
            .unwrap();
        assert!(matches!(previous.record(), Record::Peer(_)));
        let request = records::read(&mut reader, began + WAIT * 2, &budget)
            .await
            .unwrap();
        assert!(matches!(
            request.record(),
            Record::Control(Control::CloudRemovalsRead { .. })
        ));
    });
    assert!(matches!(result, Err(Error::Timeout)));
    assert_eq!(began.elapsed(), WAIT);
}

#[tokio::test(start_paused = true)]
async fn mismatched_and_unsolicited_answers_are_terminal() {
    let event = event();
    let ack = Control::CloudDocumentRemoved {
        version: Version1,
        event_id: event.event_id,
        document_id: event.document_id,
    };
    let entry = removal::Entry {
        sequence: 4,
        document_id: event.document_id,
        state: removal::State::Removed,
        event: event.clone(),
    };
    for (request, response) in [
        (removal::read(0), ack.clone()),
        (removal::request(event.clone()), removal::page(vec![], 0)),
        (
            removal::request(event.clone()),
            Control::CloudDocumentRemoved {
                version: Version1,
                event_id: uuid::Uuid::new_v4(),
                document_id: event.document_id,
            },
        ),
        (
            removal::request(event.clone()),
            Control::CloudDocumentRemoved {
                version: Version1,
                event_id: event.event_id,
                document_id: uuid::Uuid::new_v4(),
            },
        ),
        (removal::read(5), removal::page(vec![], 4)),
        (removal::read(5), removal::page(vec![], 6)),
        (removal::read(4), removal::page(vec![entry], 4)),
    ] {
        let (mut client, mut reader, mut writer, _, _, budget) = pair_with_workspace(true).await;
        assert!(matches!(
            answer(
                &mut client,
                &mut reader,
                &mut writer,
                &budget,
                request,
                response
            )
            .await,
            Err(Error::Invalid)
        ));
        assert!(client.removal.is_none());
        assert!(client.next().await.is_err());
    }
    let (mut client, _reader, mut writer, _, _, budget) = pair_with_workspace(true).await;
    records::write(
        &mut writer,
        &Record::Control(ack),
        Instant::now() + WAIT,
        &budget,
    )
    .await
    .unwrap();
    assert!(matches!(client.next().await, Err(Error::Invalid)));
}
