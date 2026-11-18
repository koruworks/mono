// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Fixture {
    open: AtomicBool,
    revoked: AtomicBool,
    bound: AtomicUsize,
}
impl Port for Fixture {
    fn catalog(&self, _: &Secret) -> Result<Vec<Value>, Error> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(Error::CredentialRevoked);
        }
        Ok(crate::own_tools::catalog())
    }
    fn bind(&self, _: &Secret, _: Uuid, _: &Call) -> Result<Binding, Error> {
        if !self.open.load(Ordering::Acquire) {
            return Err(Error::NoTurn);
        }
        self.bound.fetch_add(1, Ordering::Relaxed);
        Ok(Binding {
            id: Uuid::new_v4(),
            turn: crate::messages::Turn {
                id: Uuid::new_v4(),
                generation: 1,
                worker: Uuid::new_v4(),
                session: Uuid::new_v4(),
            },
        })
    }
    fn journal(&self, _: Binding, _: &str) -> Result<(), Error> {
        Ok(())
    }
    fn execute(&self, _: &Secret, _: Binding, _: &Call) -> Result<Value, Error> {
        Ok(serde_json::json!({"messages":[]}))
    }
}
fn fixture() -> (Arc<Fixture>, Door) {
    let port = Arc::new(Fixture {
        open: AtomicBool::new(false),
        revoked: AtomicBool::new(false),
        bound: AtomicUsize::new(0),
    });
    let door = Door::open(Secret::parse("ab".repeat(32)).unwrap(), port.clone()).unwrap();
    (port, door)
}
#[test]
fn discovery_before_turn_is_static_but_tool_calls_require_a_turn() {
    let (port, door) = fixture();
    let reply = rpc(&door, br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#).unwrap().unwrap();
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    let call = br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_older_messages","arguments":{}}}"#;
    assert_eq!(
        rpc(&door, call).unwrap().unwrap()["result"]["structuredContent"]["code"],
        "no_turn"
    );
    port.open.store(true, Ordering::Release);
    assert_eq!(
        rpc(&door, call).unwrap().unwrap()["result"]["isError"],
        false
    );
    assert_eq!(port.bound.load(Ordering::Relaxed), 1);
    port.revoked.store(true, Ordering::Release);
    assert_eq!(
        rpc(&door, br#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#).err(),
        Some(Error::CredentialRevoked)
    );
}
#[test]
fn duplicate_keys_in_outer_packet_do_not_disappear_before_binding() {
    let (port, door) = fixture();
    port.open.store(true, Ordering::Release);
    assert_eq!(rpc(&door, br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_older_messages","arguments":{"after":0,"after":1}}}"#).err(), Some(Error::InvalidRequest));
    assert_eq!(port.bound.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn http_requires_exact_host_bearer_and_no_browser_origin() {
    let (_, door) = fixture();
    let registry = Arc::new(Registry::default());
    let secret = door.secret.clone();
    registry.install(door).unwrap();
    let state = Arc::new(HttpState {
        registry: registry.clone(),
        host: "127.0.0.1:43210".into(),
        calls: Arc::new(Semaphore::new(1)),
    });
    for (host, bearer, origin, expected) in [
        (
            "127.0.0.1:43210",
            format!("Bearer {}", secret.expose()),
            false,
            StatusCode::OK,
        ),
        (
            "rebound.example:43210",
            format!("Bearer {}", secret.expose()),
            false,
            StatusCode::FORBIDDEN,
        ),
        (
            "127.0.0.1:43210",
            "Bearer invalid".into(),
            false,
            StatusCode::UNAUTHORIZED,
        ),
        (
            "127.0.0.1:43210",
            format!("Bearer {}", secret.expose()),
            true,
            StatusCode::FORBIDDEN,
        ),
    ] {
        let mut request = axum::http::Request::builder()
            .header("host", host)
            .header("authorization", bearer);
        if origin {
            request = request.header("origin", "http://example.test");
        }
        let request = request
            .body(axum::body::Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            ))
            .unwrap();
        assert_eq!(
            handle_request(State(state.clone()), request).await.status(),
            expected
        );
    }
    registry.remove(&secret);
    assert!(
        registry
            .resolve(&format!("Bearer {}", secret.expose()))
            .is_err()
    );
}
