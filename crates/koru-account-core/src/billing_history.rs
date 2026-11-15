// SPDX-License-Identifier: MPL-2.0

//! Read-only, invoice-scoped history. Payment amounts belong to one invoice,
//! never the potentially shared underlying charge or payment intent.
use crate::{KoruIdentityId, PortFuture, subscription::BillingError};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryPosition {
    First,
    Exact(String),
    After(String),
}

#[derive(Clone, Debug)]
pub struct HistoryBinding {
    pub subscription_id: String,
    pub customer_id: String,
    pub livemode: bool,
    pub has_older: bool,
}

pub trait BillingHistoryRepository: Send + Sync {
    /// Claim unambiguous pending purchases, then return one owned binding.
    /// Exact/After anchors must also belong to this active account and mode.
    /// Finish the transaction before any provider request.
    fn history_binding(
        &self,
        account: KoruIdentityId,
        livemode: bool,
        position: HistoryPosition,
    ) -> PortFuture<'_, Result<Option<HistoryBinding>, BillingError>>;
    /// Every subscription of the account and mode, newest first. `prices`
    /// are the Cloud prices currently allowed to grant access (monthly, then
    /// yearly when configured); an entry at any other price is listed without
    /// Cloud access.
    fn subscriptions<'a>(
        &'a self,
        account: KoruIdentityId,
        livemode: bool,
        prices: &'a [String],
        after: Option<&'a str>,
        limit: u8,
        now: i64,
    ) -> PortFuture<'a, Result<HistoryPage<SubscriptionEntry>, BillingError>>;
}

#[derive(Clone, Serialize)]
pub struct SubscriptionEntry {
    #[serde(flatten)]
    pub subscription: crate::subscription::SubscriptionSummary,
    pub price_id: String,
    pub livemode: bool,
}

#[derive(Clone, Serialize)]
pub struct InvoicePayment {
    pub id: String,
    pub status: String,
    pub currency: String,
    pub amount_requested: i64,
    pub amount_paid: Option<i64>,
    pub created_at_millis: i64,
    pub paid_at_millis: Option<i64>,
    pub canceled_at_millis: Option<i64>,
}

#[derive(Clone)]
pub struct HistoryPage<T> {
    pub items: Vec<T>,
    pub has_more: bool,
}

#[derive(Clone, Serialize)]
pub struct Invoice {
    pub id: String,
    pub number: Option<String>,
    pub status: Option<String>,
    pub currency: String,
    pub total: i64,
    pub amount_due: i64,
    pub amount_paid: i64,
    pub amount_remaining: i64,
    pub created_at_millis: i64,
    pub due_at_millis: Option<i64>,
    pub finalized_at_millis: Option<i64>,
    pub paid_at_millis: Option<i64>,
    pub voided_at_millis: Option<i64>,
    pub marked_uncollectible_at_millis: Option<i64>,
    pub hosted_invoice_url: Option<String>,
    pub invoice_pdf: Option<String>,
    #[serde(skip)]
    pub payments: HistoryPage<InvoicePayment>,
}

pub trait BillingHistoryProvider: Send + Sync {
    fn invoices<'a>(
        &'a self,
        binding: &'a HistoryBinding,
        after: Option<&'a str>,
        limit: u8,
    ) -> PortFuture<'a, Result<HistoryPage<Invoice>, BillingError>>;
    fn payments<'a>(
        &'a self,
        binding: &'a HistoryBinding,
        invoice: &'a str,
        after: &'a str,
        limit: u8,
    ) -> PortFuture<'a, Result<HistoryPage<InvoicePayment>, BillingError>>;
}
