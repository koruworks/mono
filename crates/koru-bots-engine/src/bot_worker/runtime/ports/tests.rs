// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{bot_worker::runtime::client::Request, sessions::Port as _};
use std::sync::mpsc;

fn fixture() -> (Port, mpsc::Receiver<Request>, Turn) {
    let (sender, receiver) = mpsc::sync_channel(8);
    let reservation = Reservation {
        key: crate::capacity::SessionKey {
            bot: Uuid::new_v4(),
            thread: Uuid::new_v4(),
        },
        session: Uuid::new_v4(),
        assignment: Uuid::new_v4(),
    };
    let turn = Turn {
        id: Uuid::new_v4(),
        generation: 1,
        worker: Uuid::new_v4(),
        session: reservation.session,
    };
    (
        Port::new(
            Client(sender),
            reservation,
            Default::default(),
            koru_ipc::Endpoint::for_service(koru_ipc::BOT_WORKSPACE, "internal-debug").unwrap(),
            "http://127.0.0.1:1/mcp".parse().unwrap(),
            Arc::new(door::http::Registry::default()),
            Arc::new(Mutex::new(BotDatabase::in_memory().unwrap())),
            Arc::new(super::super::paths::Paths::default()),
        ),
        receiver,
        turn,
    )
}

#[test]
fn lost_publish_reply_retries_the_exact_receipt_identity_and_content() {
    let (port, receiver, turn) = fixture();
    let command = Uuid::new_v4();
    let engine = std::thread::spawn(move || {
        let mut first = None;
        for attempt in 0..2 {
            let request = receiver.recv().unwrap();
            let Command::Publish {
                request: publish, ..
            } = request.command
            else {
                panic!("publish expected")
            };
            assert_eq!(publish.command, command);
            assert_eq!(publish.turn, turn);
            let bytes = serde_json::to_vec(&publish).unwrap();
            if let Some(first) = &first {
                assert_eq!(first, &bytes);
            } else {
                first = Some(bytes);
            }
            let reply = if attempt == 0 {
                Err(Error::EngineUnavailable)
            } else {
                Ok(Reply::Published(crate::messages::Published {
                    message: Uuid::new_v4(),
                    answered: 1,
                }))
            };
            request.reply.send(reply).unwrap();
        }
    });
    port.publish(turn, command, "durable answer".into())
        .unwrap();
    engine.join().unwrap();
}

#[test]
fn retirement_is_local_until_the_manager_confirms_the_session_thread_ended() {
    let (port, receiver, turn) = fixture();
    port.retired(turn).unwrap();
    assert_eq!(port.turn(), Some(turn));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn pending_context_retries_and_another_thread_is_rejected_locally() {
    let (port, receiver, _) = fixture();
    assert!(matches!(
        port.context(Uuid::new_v4()),
        Err(Error::NotAllowed)
    ));
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let engine = std::thread::spawn(move || {
        for reply in [
            Reply::ContextPending,
            Reply::Context(sessions::Context {
                role: None,
                knowledge: vec![],
                memory: None,
            }),
        ] {
            let request = receiver.recv().unwrap();
            assert!(matches!(request.command, Command::Context { .. }));
            request.reply.send(Ok(reply)).unwrap();
        }
    });
    assert!(
        port.context(port.reservation.key.thread)
            .unwrap()
            .role
            .is_none()
    );
    engine.join().unwrap();
}
