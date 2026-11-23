// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::io::{Read, Write};

fn delivered(query: &str) -> (Result<Callback, ServiceError>, String) {
    let listener =
        CallbackListener::bind(koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH).unwrap();
    let port = listener.local_addr().port();
    let query = query.to_owned();
    let sender = std::thread::spawn(move || {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "GET {}?{} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            koru_account_wire::NATIVE_LOOPBACK_CALLBACK_PATH,
            query
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    });
    let mut session = Session {
        listener: listener.into_two_stage(),
        port,
    };
    let result = session.wait_with("seeded", &|| false);
    (result, sender.join().unwrap())
}

#[test]
fn desktop_delivery_preserves_duplicate_fields_and_success_html() {
    let (callback, response) = delivered("state=seeded&grant=first&outcome=completed&grant=second");
    let callback = callback.unwrap();
    assert_eq!(
        callback.field_names().collect::<Vec<_>>(),
        ["state", "grant", "outcome", "grant"]
    );
    assert_eq!(
        callback.values("grant").collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(response.contains(callback_success_html()));
    assert!(response.contains("data:image/png;base64,"));
    assert!(response.contains("window.close()"));
}

#[test]
fn desktop_delivery_rejects_wrong_state_and_keeps_error_page_visible() {
    let (callback, response) = delivered("state=foreign&outcome=completed&grant=secret");
    let Err(error) = callback else {
        panic!("foreign state was accepted")
    };
    assert_eq!(error.code, "callback_invalid");
    assert!(response.contains(callback_error_html()));
    assert!(response.contains("data:image/png;base64,"));
    assert!(!response.contains("window.close()"));
    assert!(!response.contains("secret"));
}
