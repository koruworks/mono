// SPDX-License-Identifier: MPL-2.0
use super::*;

fn row(status: SubscriptionStatus, paid: bool, price: &str) -> SubscriptionSnapshot {
    SubscriptionSnapshot {
        subscription_id: price.into(),
        customer_id: "customer".into(),
        checkout_session_id: "checkout".into(),
        purchase_email: "test@example.test".into(),
        status,
        price_id: price.into(),
        livemode: false,
        invoice_paid: paid,
        current_period_end_millis: 200,
        cancel_at_period_end: false,
        cancel_at_millis: None,
        canceled_at_millis: None,
    }
}
fn input(rows: &[SubscriptionSnapshot], grant: Option<i64>, now: i64) -> CloudPlanInput {
    let (subscription, purchase) = select_purchase(
        rows,
        &[
            ("month".into(), PurchaseInterval::Month),
            ("year".into(), PurchaseInterval::Year),
        ],
        false,
        now,
    );
    CloudPlanInput {
        billing_mode: BillingMode::Billed,
        subscription,
        complimentary_until_millis: Ok(grant),
        free_place: Ok(None),
        purchase,
    }
}

#[test]
fn paid_trial_grant_and_cancellation_boundaries() {
    for status in [SubscriptionStatus::Active, SubscriptionStatus::Trialing] {
        let mut sub = row(status, true, "month");
        sub.cancel_at_millis = Some(150);
        let input = input(&[sub], Some(300), 100);
        let before = CloudPlanPolicy::evaluate(&input, 100);
        assert_eq!(before.valid_until_millis, Some(150));
        assert_eq!(before.access_until_millis, Some(300));
        assert_eq!(before.reason, CloudAccessReason::ComplimentaryActive);
        let after = CloudPlanPolicy::evaluate(&input, 150);
        assert_eq!(after.valid_until_millis, Some(300));
        assert_eq!(
            after.allowance,
            CloudAllowance::Finite {
                base_bytes: PLUS_BYTES,
                effective_bytes: PLUS_BYTES
            }
        );
        assert_eq!(
            CloudPlanPolicy::evaluate(&input, 300).tier,
            CloudTier::Local
        );
    }
}

#[test]
fn unpaid_and_expired_purchases_remain_manageable() {
    for status in [
        SubscriptionStatus::Active,
        SubscriptionStatus::Unpaid,
        SubscriptionStatus::Canceled,
        SubscriptionStatus::Trialing,
    ] {
        let input = input(&[row(status, false, "year")], None, 100);
        let policy = CloudPlanPolicy::evaluate(&input, 100);
        assert_eq!(policy.tier, CloudTier::Local);
        assert!(matches!(
            policy.purchase,
            CloudPurchase::Known {
                manageable: true,
                interval: Some(PurchaseInterval::Year),
                ..
            }
        ));
    }
    let trial = input(
        &[row(SubscriptionStatus::Trialing, true, "year")],
        None,
        100,
    );
    assert_eq!(
        CloudPlanPolicy::evaluate(&trial, 199).reason,
        CloudAccessReason::TrialActive
    );
    assert_eq!(
        CloudPlanPolicy::evaluate(&trial, 200).tier,
        CloudTier::Local
    );
}

#[test]
fn selection_preserves_configured_prices_and_conflicts() {
    let mut month = row(SubscriptionStatus::Active, true, "month");
    month.cancel_at_period_end = true;
    let year = row(SubscriptionStatus::Active, true, "year");
    let selected = input(&[month, year.clone()], None, 100);
    assert!(matches!(
        selected.purchase,
        CloudPurchase::Known {
            interval: Some(PurchaseInterval::Year),
            ..
        }
    ));
    let (_, purchase) = select_purchase(
        &[year.clone()],
        &[("month".into(), PurchaseInterval::Month)],
        false,
        100,
    );
    assert_eq!(
        purchase,
        CloudPurchase::Known {
            manageable: false,
            status: None,
            interval: None,
            available_intervals: vec![PurchaseInterval::Month]
        }
    );
    assert_eq!(
        CloudPlanPolicy::evaluate(
            &input(
                &[row(SubscriptionStatus::Active, true, "month"), year],
                None,
                100
            ),
            100
        )
        .tier,
        CloudTier::Unknown
    );
}

#[test]
fn unavailable_local_unbilled_and_free_are_distinct() {
    let mut input = input(&[], None, 100);
    assert_eq!(
        CloudPlanPolicy::evaluate(&input, 100).tier,
        CloudTier::Local
    );
    input.subscription = Err(PolicyUnavailable);
    let unknown = CloudPlanPolicy::evaluate(&input, 100);
    assert_eq!(unknown.tier, CloudTier::Unknown);
    assert_eq!(unknown.mcp_allowed, None);
    assert!(unknown.mcp_entitlement().is_err());
    input.billing_mode = BillingMode::Unbilled;
    input.complimentary_until_millis = Err(PolicyUnavailable);
    let unbilled = CloudPlanPolicy::evaluate(&input, 100);
    assert_eq!(unbilled.allowance, CloudAllowance::Unbounded);
    assert!(unbilled.mcp_entitlement().unwrap().is_active_at(i64::MAX));
    let free = CloudPlanPolicy::fake_free();
    assert_eq!(free.access, CloudAccessState::ReadWrite);
    assert_eq!(
        free.allowance,
        CloudAllowance::Finite {
            base_bytes: FREE_BYTES,
            effective_bytes: FREE_BYTES
        }
    );
    assert!(free.mcp_entitlement().unwrap().is_active_at(i64::MAX));
}

fn free_allowance() -> CloudAllowance {
    CloudAllowance::Finite {
        base_bytes: FREE_BYTES,
        effective_bytes: FREE_BYTES,
    }
}

#[test]
fn an_active_place_is_free_with_one_gigabyte_and_cloud_mcp() {
    let mut input = input(&[], None, 100);
    input.free_place = Ok(Some(FreePlace::Active));
    let policy = CloudPlanPolicy::evaluate(&input, 100);
    assert_eq!(policy.tier, CloudTier::Free);
    assert_eq!(policy.allowance, free_allowance());
    assert_eq!(FREE_BYTES, 1_000_000_000);
    assert_eq!(policy.access, CloudAccessState::ReadWrite);
    assert_eq!(policy.reason, CloudAccessReason::FreeActive);
    assert_eq!(policy.mcp_allowed, Some(true));
    // Free has no end of its own, so there is nothing to revalidate at.
    assert_eq!(policy.valid_until_millis, None);
    assert!(matches!(
        policy.mcp_entitlement(),
        Ok(CloudEntitlement::Active { until_millis: None })
    ));
}

#[test]
fn only_an_active_place_grants_free() {
    for place in [
        None,
        Some(FreePlace::Offered),
        Some(FreePlace::RemovalPending),
        Some(FreePlace::Released),
    ] {
        let mut input = input(&[], None, 100);
        input.free_place = Ok(place);
        let policy = CloudPlanPolicy::evaluate(&input, 100);
        assert_eq!(policy.tier, CloudTier::Local, "{place:?}");
        assert_eq!(policy.access, CloudAccessState::None);
        assert_eq!(policy.mcp_allowed, Some(false));
        assert!(matches!(
            policy.mcp_entitlement(),
            Ok(CloudEntitlement::None)
        ));
    }
    let mut unknown = input(&[], None, 100);
    unknown.free_place = Err(PolicyUnavailable);
    assert_eq!(
        CloudPlanPolicy::evaluate(&unknown, 100).tier,
        CloudTier::Unknown
    );
}

#[test]
fn plus_overrides_a_place_and_a_lapse_returns_to_it() {
    for place in [FreePlace::Active, FreePlace::HeldPlus] {
        let mut input = input(&[row(SubscriptionStatus::Active, true, "month")], None, 100);
        input.free_place = Ok(Some(place));
        let plus = CloudPlanPolicy::evaluate(&input, 100);
        assert_eq!(plus.tier, CloudTier::Plus);
        assert_eq!(
            plus.allowance,
            CloudAllowance::Finite {
                base_bytes: PLUS_BYTES,
                effective_bytes: PLUS_BYTES
            }
        );
        assert_eq!(plus.valid_until_millis, Some(200));
    }
    // The period ended. An active place is Free again at once; a place still
    // marked held is neither Free nor Local until its return is committed.
    let mut input = input(&[row(SubscriptionStatus::Active, true, "month")], None, 100);
    input.free_place = Ok(Some(FreePlace::Active));
    let lapsed = CloudPlanPolicy::evaluate(&input, 200);
    assert_eq!(lapsed.tier, CloudTier::Free);
    assert_eq!(lapsed.allowance, free_allowance());
    input.free_place = Ok(Some(FreePlace::HeldPlus));
    let held = CloudPlanPolicy::evaluate(&input, 200);
    assert_eq!(held.tier, CloudTier::Unknown);
    assert_eq!(held.access, CloudAccessState::Unavailable);
    assert!(held.mcp_entitlement().is_err());
}

#[test]
fn an_unbilled_deployment_ignores_places() {
    let mut input = input(&[], None, 100);
    input.billing_mode = BillingMode::Unbilled;
    input.free_place = Ok(Some(FreePlace::Active));
    let policy = CloudPlanPolicy::evaluate(&input, 100);
    assert_eq!(policy.tier, CloudTier::Unbilled);
    assert_eq!(policy.allowance, CloudAllowance::Unbounded);
}
