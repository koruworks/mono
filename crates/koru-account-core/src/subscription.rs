// SPDX-License-Identifier: MPL-2.0

//! Billing coordination only; no workspace quota or content authority.
use crate::{KoruIdentityId, PortFuture};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Incomplete,
    IncompleteExpired,
    Trialing,
    Active,
    PastDue,
    Canceled,
    Unpaid,
    Paused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubscriptionTarget {
    Checkout(String),
    Subscription(String),
    Invoice(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BillingError {
    Invalid,
    Unauthorized,
    Conflict,
    Unavailable,
    Unsupported,
}

/// One authenticated Stripe delivery. JSON is retained for operator inspection,
/// never used as current entitlement authority. Do not log this value.
pub struct WebhookDelivery {
    pub event_id: String,
    pub event_type: String,
    pub livemode: bool,
    pub provider_created_at_millis: i64,
    pub received_at_millis: i64,
    pub payload: String,
    pub object_id: Option<String>,
    pub subscription_id: Option<String>,
    pub customer_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebhookCompletion {
    Processed,
    Ignored,
    Failed(BillingError),
}

pub trait WebhookRepository: Send + Sync {
    /// A separate cross-replica notification lease. Never acquire it while
    /// holding a subscription update; dropping it releases the lock.
    fn begin_notification<'a>(
        &'a self,
        key: &'a str,
    ) -> PortFuture<'a, Result<Box<dyn NotificationLease>, BillingError>>;
    /// Commit the received row before returning its independently minted ID.
    /// Repeated event IDs remain separate deliveries, including exact retries.
    fn receive_webhook(
        &self,
        delivery: WebhookDelivery,
    ) -> PortFuture<'_, Result<uuid::Uuid, BillingError>>;
    /// Commit the terminal outcome before the handler acknowledges delivery.
    fn complete_webhook(
        &self,
        receipt: uuid::Uuid,
        completion: WebhookCompletion,
        completed_at_millis: i64,
        subscription_id: Option<String>,
        customer_id: Option<String>,
    ) -> PortFuture<'_, Result<(), BillingError>>;
}

pub trait NotificationLease: Send {}

/// Authoritative provider snapshot. Purchase identity is immutable after insertion.
#[derive(Clone)]
pub struct SubscriptionSnapshot {
    pub subscription_id: String,
    pub customer_id: String,
    pub checkout_session_id: String,
    pub purchase_email: String,
    pub status: SubscriptionStatus,
    pub price_id: String,
    pub livemode: bool,
    /// The current period is settled. For `Active` this is a paid latest
    /// invoice covering the current period at a supported price. For
    /// `Trialing` nothing is owed until the trial ends, so the provider sets
    /// it when the current item is a supported price, whether the trial's $0
    /// invoice is present or not.
    pub invoice_paid: bool,
    /// End of the current period. While `Trialing` this is the trial end:
    /// the provider records the earlier of the item's period end and the
    /// subscription's `trial_end`.
    pub current_period_end_millis: i64,
    pub cancel_at_period_end: bool,
    pub cancel_at_millis: Option<i64>,
    pub canceled_at_millis: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SubscriptionSummary {
    pub subscription_id: String,
    pub customer_id: String,
    pub status: SubscriptionStatus,
    pub cloud_access: bool,
    pub storage_allowance_bytes: u64,
    pub paid_through_millis: Option<i64>,
    pub cancel_at_period_end: bool,
    pub cancel_at_millis: Option<i64>,
    pub canceled_at_millis: Option<i64>,
    pub account_linked: bool,
}

impl SubscriptionSummary {
    /// A provider/lock wait can cross the paid boundary. Recheck immediately
    /// before delivery; this can only remove eligibility, never create it.
    pub fn at(mut self, now: i64) -> Self {
        if self.paid_through_millis.is_none_or(|end| end <= now)
            || self.cancel_at_millis.is_some_and(|end| end <= now)
        {
            self.cloud_access = false;
            self.storage_allowance_bytes = 0;
        }
        self
    }
}

impl SubscriptionSnapshot {
    /// Whether this status, on its own, can grant Cloud access. `Active`
    /// needs its invoice paid. `Trialing` grants until the trial ends
    /// (`current_period_end_millis`); its period is settled by being free.
    /// Every other status, including a trial that turned `incomplete`,
    /// `past_due` or `unpaid`, grants nothing.
    fn in_good_standing(&self) -> bool {
        match self.status {
            SubscriptionStatus::Active | SubscriptionStatus::Trialing => self.invoice_paid,
            _ => false,
        }
    }

    pub fn summary(&self, account_linked: bool, now: i64) -> SubscriptionSummary {
        let standing = self.in_good_standing();
        let access = account_linked
            && standing
            && self.current_period_end_millis > now
            && self.cancel_at_millis.is_none_or(|end| end > now);
        SubscriptionSummary {
            subscription_id: self.subscription_id.clone(),
            customer_id: self.customer_id.clone(),
            status: self.status,
            cloud_access: access,
            storage_allowance_bytes: if access { 100_000_000_000 } else { 0 },
            // A trial's settled period runs through the trial end, so this is
            // the trial end while trialing and `at` revokes access after it.
            paid_through_millis: self.invoice_paid.then_some(self.current_period_end_millis),
            cancel_at_period_end: self.cancel_at_period_end,
            cancel_at_millis: self.cancel_at_millis,
            canceled_at_millis: self.canceled_at_millis,
            account_linked,
        }
    }
}

/// Which of an account's subscriptions at the current Cloud prices answers
/// for it. `rows` is every such subscription, newest first.
///
/// Live means anything not canceled or incomplete_expired. One live
/// subscription answers for the account. Several live ones are two plans
/// billing at once, which is a conflict for support, with one exception:
/// when exactly one of them is not scheduled to end (no
/// `cancel_at_period_end`, no `cancel_at`), every other one is on its way out
/// and that one answers. That is the switch from monthly to yearly (cancel
/// the monthly plan at period end, buy the yearly one), and the same rule
/// covers a cancel-and-rebuy at one price. Two or more renewing plans remain
/// a conflict. With nothing live, the newest subscription answers, as before.
pub fn current_subscription(
    rows: &[SubscriptionSnapshot],
) -> Result<Option<&SubscriptionSnapshot>, BillingError> {
    let live: Vec<_> = rows
        .iter()
        .filter(|s| {
            !matches!(
                s.status,
                SubscriptionStatus::Canceled | SubscriptionStatus::IncompleteExpired
            )
        })
        .collect();
    if live.len() > 1 {
        let mut renewing = live
            .iter()
            .filter(|s| !s.cancel_at_period_end && s.cancel_at_millis.is_none());
        return match (renewing.next(), renewing.next()) {
            (Some(only), None) => Ok(Some(*only)),
            _ => Err(BillingError::Conflict),
        };
    }
    Ok(live.first().copied().or(rows.first()))
}

pub trait SubscriptionProvider: Send + Sync {
    fn retrieve<'a>(
        &'a self,
        target: &'a SubscriptionTarget,
    ) -> PortFuture<'a, Result<SubscriptionSnapshot, BillingError>>;
}

/// The lease serializes the complete provider read and commit across replicas.
/// Dropping it rolls back. Implementations must bound lock acquisition.
pub trait SubscriptionUpdate: Send {
    fn commit(
        self: Box<Self>,
        snapshot: SubscriptionSnapshot,
        now: i64,
    ) -> PortFuture<'static, Result<SubscriptionSummary, BillingError>>;
}

pub trait SubscriptionRepository: Send + Sync {
    fn begin_update(&self) -> PortFuture<'_, Result<Box<dyn SubscriptionUpdate>, BillingError>>;
    /// `prices` are the Cloud prices currently allowed to grant access: the
    /// monthly price, then the yearly one when it is configured. Only
    /// subscriptions recorded at one of them are claimed and considered, and
    /// [`current_subscription`] picks the one that answers.
    fn for_account<'a>(
        &'a self,
        account: KoruIdentityId,
        now: i64,
        livemode: bool,
        prices: &'a [String],
    ) -> PortFuture<'a, Result<Option<SubscriptionSummary>, BillingError>>;
}

pub async fn reconcile(
    repository: &dyn SubscriptionRepository,
    provider: &dyn SubscriptionProvider,
    target: &SubscriptionTarget,
    now: i64,
) -> Result<SubscriptionSummary, BillingError> {
    let update = repository.begin_update().await?;
    let snapshot = provider.retrieve(target).await?;
    update.commit(snapshot, now).await
}

#[cfg(test)]
mod current_subscription_tests {
    use super::*;

    fn sub(
        id: &str,
        price: &str,
        status: SubscriptionStatus,
        ending: bool,
    ) -> SubscriptionSnapshot {
        SubscriptionSnapshot {
            subscription_id: id.into(),
            customer_id: "cus_a".into(),
            checkout_session_id: format!("cs_{id}"),
            purchase_email: "person@example.com".into(),
            status,
            price_id: price.into(),
            livemode: false,
            invoice_paid: true,
            current_period_end_millis: 2_000_000,
            cancel_at_period_end: ending,
            cancel_at_millis: None,
            canceled_at_millis: None,
        }
    }
    use SubscriptionStatus::{Active, Canceled};

    #[test]
    fn one_live_plan_answers_monthly_or_yearly() {
        for price in ["price_month", "price_year"] {
            let rows = [sub("sub_a", price, Active, false)];
            let current = current_subscription(&rows).unwrap().unwrap();
            assert_eq!(current.price_id, price);
        }
        assert!(current_subscription(&[]).unwrap().is_none());
    }

    #[test]
    fn switching_monthly_to_yearly_answers_with_the_renewing_plan() {
        // Newest first: the yearly purchase, then the monthly plan set to end.
        let rows = [
            sub("sub_year", "price_year", Active, false),
            sub("sub_month", "price_month", Active, true),
        ];
        assert_eq!(
            current_subscription(&rows)
                .unwrap()
                .unwrap()
                .subscription_id,
            "sub_year"
        );
        // Order does not matter; a cancel_at date counts as ending too.
        let mut month = sub("sub_month", "price_month", Active, false);
        month.cancel_at_millis = Some(1_500_000);
        let rows = [month, sub("sub_year", "price_year", Active, false)];
        assert_eq!(
            current_subscription(&rows)
                .unwrap()
                .unwrap()
                .subscription_id,
            "sub_year"
        );
    }

    #[test]
    fn two_renewing_plans_are_still_a_conflict() {
        let rows = [
            sub("sub_year", "price_year", Active, false),
            sub("sub_month", "price_month", Active, false),
        ];
        assert!(matches!(
            current_subscription(&rows),
            Err(BillingError::Conflict)
        ));
        let rows = [
            sub("sub_year", "price_year", Active, true),
            sub("sub_month", "price_month", Active, true),
        ];
        assert!(matches!(
            current_subscription(&rows),
            Err(BillingError::Conflict)
        ));
    }

    #[test]
    fn a_trial_counts_as_a_live_renewing_plan() {
        use SubscriptionStatus::Trialing;
        // One trial answers on its own.
        let rows = [sub("sub_trial", "price_month", Trialing, false)];
        assert_eq!(
            current_subscription(&rows)
                .unwrap()
                .unwrap()
                .subscription_id,
            "sub_trial"
        );
        // A trial set to end beside a renewing yearly plan: the yearly answers.
        let rows = [
            sub("sub_year", "price_year", Active, false),
            sub("sub_trial", "price_month", Trialing, true),
        ];
        assert_eq!(
            current_subscription(&rows)
                .unwrap()
                .unwrap()
                .subscription_id,
            "sub_year"
        );
        // A renewing trial beside a renewing paid plan is two plans billing.
        let rows = [
            sub("sub_year", "price_year", Active, false),
            sub("sub_trial", "price_month", Trialing, false),
        ];
        assert!(matches!(
            current_subscription(&rows),
            Err(BillingError::Conflict)
        ));
    }

    #[test]
    fn canceled_history_never_conflicts_and_the_newest_answers_when_none_is_live() {
        let rows = [
            sub("sub_new", "price_year", Active, false),
            sub("sub_old", "price_month", Canceled, false),
        ];
        assert_eq!(
            current_subscription(&rows)
                .unwrap()
                .unwrap()
                .subscription_id,
            "sub_new"
        );
        let rows = [
            sub("sub_new", "price_year", Canceled, false),
            sub("sub_old", "price_month", Canceled, false),
        ];
        assert_eq!(
            current_subscription(&rows)
                .unwrap()
                .unwrap()
                .subscription_id,
            "sub_new"
        );
    }
}

#[cfg(test)]
mod entitlement_tests {
    use super::*;
    use SubscriptionStatus::*;

    const TRIAL_END: i64 = 2_000_000;

    fn snapshot(status: SubscriptionStatus, invoice_paid: bool) -> SubscriptionSnapshot {
        SubscriptionSnapshot {
            subscription_id: "sub_a".into(),
            customer_id: "cus_a".into(),
            checkout_session_id: "cs_a".into(),
            purchase_email: "person@example.com".into(),
            status,
            price_id: "price_month".into(),
            livemode: false,
            invoice_paid,
            current_period_end_millis: TRIAL_END,
            cancel_at_period_end: false,
            cancel_at_millis: None,
            canceled_at_millis: None,
        }
    }

    #[test]
    fn active_still_needs_a_paid_invoice() {
        assert!(snapshot(Active, true).summary(true, 1_000_000).cloud_access);
        let unpaid = snapshot(Active, false).summary(true, 1_000_000);
        assert!(!unpaid.cloud_access);
        assert_eq!(unpaid.paid_through_millis, None);
    }

    #[test]
    fn a_trial_grants_access_until_its_end() {
        let summary = snapshot(Trialing, true).summary(true, 1_000_000);
        assert!(summary.cloud_access);
        assert_eq!(summary.storage_allowance_bytes, 100_000_000_000);
        assert_eq!(summary.paid_through_millis, Some(TRIAL_END));
        assert!(
            !snapshot(Trialing, true)
                .summary(false, 1_000_000)
                .cloud_access
        );
        // A trial whose period is not settled (unsupported item) grants nothing.
        assert!(
            !snapshot(Trialing, false)
                .summary(true, 1_000_000)
                .cloud_access
        );
        // A trial with a cancel_at date ends at that date.
        let mut ending = snapshot(Trialing, true);
        ending.cancel_at_millis = Some(1_500_000);
        assert!(ending.summary(true, 1_400_000).cloud_access);
        assert!(!ending.summary(true, 1_500_000).cloud_access);
    }

    #[test]
    fn a_lapsed_trial_grants_nothing() {
        // The provider has not reported the end of the trial yet: still
        // trialing on record, but past the trial end.
        assert!(
            !snapshot(Trialing, true)
                .summary(true, TRIAL_END)
                .cloud_access
        );
        assert!(
            !snapshot(Trialing, true)
                .summary(true, TRIAL_END + 1)
                .cloud_access
        );
        // Granted while trialing, delivered after the end: revoked.
        let delivered = snapshot(Trialing, true)
            .summary(true, 1_000_000)
            .at(TRIAL_END);
        assert!(!delivered.cloud_access);
        assert_eq!(delivered.storage_allowance_bytes, 0);
    }

    #[test]
    fn a_trial_that_failed_to_convert_grants_nothing() {
        for status in [
            Incomplete,
            IncompleteExpired,
            PastDue,
            Unpaid,
            Paused,
            Canceled,
        ] {
            // Even with a settled (paid $0) trial invoice still on record.
            assert!(
                !snapshot(status, true).summary(true, 1_000_000).cloud_access,
                "{status:?}"
            );
        }
    }
}
