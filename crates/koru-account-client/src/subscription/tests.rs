// SPDX-License-Identifier: MPL-2.0

use super::*;
use serde_json::json;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
struct Script {
    pages: Mutex<std::collections::VecDeque<serde_json::Value>>,
    requests: Mutex<Vec<HttpRequest>>,
    cancel: Arc<AtomicBool>,
    cancel_on_read: bool,
}
impl Transport for Script {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request);
        if self.cancel_on_read {
            self.cancel.store(true, Ordering::SeqCst);
        }
        let page = self
            .pages
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(TransportError::Unavailable)?;
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(&page).unwrap(),
        })
    }
}
fn script(pages: Vec<serde_json::Value>) -> Arc<Script> {
    Arc::new(Script {
        pages: Mutex::new(pages.into()),
        requests: Mutex::new(vec![]),
        cancel: Arc::new(AtomicBool::new(false)),
        cancel_on_read: false,
    })
}
fn run(transport: Arc<Script>) -> Result<bool, ClientError> {
    let cancel = transport.cancel.clone();
    AccountClient::with_transport(transport).has_cloud_subscription(
        &AccountAuthorization::Bearer(&AccessToken("secret-only-in-header".into())),
        &|| cancel.load(Ordering::SeqCst),
    )
}
#[test]
fn history_must_finish_before_absence_and_any_nonterminal_purchase_goes_to_management() {
    for status in [
        "active",
        "incomplete",
        "trialing",
        "past_due",
        "unpaid",
        "paused",
    ] {
        let transport = script(vec![
            json!({"subscriptions":[{"status":"canceled"}],"next_cursor":"next_page"}),
            json!({"subscriptions":[{"status":status}],"next_cursor":"not_needed"}),
        ]);
        assert!(run(transport.clone()).unwrap());
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].path_and_query,
            "/v1/account/subscription?limit=25&cursor=next_page"
        );
        assert!(
            requests
                .iter()
                .all(|r| !r.path_and_query.contains("secret"))
        );
        assert!(requests.iter().all(|r| {
            r.headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer secret-only-in-header")
        }));
    }
    assert!(
        !run(script(vec![
            json!({"subscriptions":[],"next_cursor":"next"}),
            json!({"subscriptions":[{"status":"incomplete_expired"}],"next_cursor":null})
        ]))
        .unwrap()
    );
}
#[test]
fn errors_missing_cursor_unknown_status_and_nonprogress_never_mean_unsubscribed() {
    for pages in [
        vec![json!({"subscriptions":[]})],
        vec![json!({"subscriptions":[{"status":"future_state"}],"next_cursor":null})],
        vec![json!({"subscriptions":[],"next_cursor":"https://other.example"})],
        vec![
            json!({"subscriptions":[],"next_cursor":"again"}),
            json!({"subscriptions":[],"next_cursor":"again"}),
        ],
        vec![json!({"subscriptions":[],"next_cursor":"unavailable"})],
        (0..32)
            .map(|n| json!({"subscriptions":[],"next_cursor":format!("page_{n}")}))
            .collect(),
    ] {
        assert!(run(script(pages)).is_err());
    }
}
#[test]
fn cancellation_during_the_final_http_response_discards_the_decision() {
    let transport = Arc::new(Script {
        pages: Mutex::new(vec![json!({"subscriptions":[],"next_cursor":null})].into()),
        requests: Mutex::new(vec![]),
        cancel: Arc::new(AtomicBool::new(false)),
        cancel_on_read: true,
    });
    assert!(run(transport).is_err());
}

#[test]
fn checkout_handoff_route_is_admitted_by_transport_path_guard() {
    assert!(crate::is_allowed_api_path(
        "/v1/billing/stripe/checkout-handoff"
    ));
    assert!(!crate::is_allowed_api_path(
        "/v1/billing/stripe/checkout-handoff#frag"
    ));
    assert!(!crate::is_allowed_api_path("/v1/other"));
}

#[test]
fn subscription_summary_reports_kind_and_complimentary_access() {
    let run_summary = |pages| {
        let transport = script(pages);
        let cancel = transport.cancel.clone();
        AccountClient::with_transport(transport)
            .cloud_subscription_summary(
                &AccountAuthorization::Bearer(&AccessToken("secret".into())),
                &|| cancel.load(Ordering::SeqCst),
            )
            .unwrap()
    };

    assert_eq!(
        run_summary(vec![json!({
            "subscriptions": [{"status": "trialing"}],
            "next_cursor": null
        })]),
        CloudSubscriptionSummary {
            has_subscription: true,
            kind: Some(SubscriptionKind::Trialing),
        }
    );
    assert_eq!(
        run_summary(vec![json!({
            "subscriptions": [{"status": "trialing"}, {"status": "active"}],
            "next_cursor": null
        })]),
        CloudSubscriptionSummary {
            has_subscription: true,
            kind: Some(SubscriptionKind::Active),
        }
    );
    assert_eq!(
        run_summary(vec![json!({
            "subscriptions": [],
            "next_cursor": null,
            "cloud_access": true
        })]),
        CloudSubscriptionSummary {
            has_subscription: true,
            kind: Some(SubscriptionKind::Complimentary),
        }
    );
    assert_eq!(
        run_summary(vec![json!({
            "subscriptions": [{"status": "canceled"}],
            "next_cursor": null,
            "cloud_access": false
        })]),
        CloudSubscriptionSummary {
            has_subscription: false,
            kind: None,
        }
    );
}
