// SPDX-License-Identifier: MPL-2.0

//! Composition over the host's migrated trust database. Platform configuration
//! enters through a trusted application adapter, never a product command.
use std::sync::Arc;

use koru_account_client::AccountClient;
use koru_account_core::DevicePlatform;
use koru_account_runtime::account::{AccountPorts, ProductionAccountService, lifecycle};
use koru_account_runtime::enrollment::{
    Control, ProductionPort, ProductionResponder, ResponderSessionGate,
};
use koru_account_runtime::ports::{Clock, Entropy, SystemClock, SystemEntropy};
use koru_oauth_credentials::CredentialContext;

use super::{EmbeddedProfileInventory, MobileAccount, NativeDelivery, PlatformBridge};
use crate::MobileHost;

/// Trusted build/platform configuration. These values must come from the
/// application's configuration, not from a document, QR payload or hosted
/// response. No production service or Google client ID is assumed here.
#[derive(Clone, PartialEq, Eq)]
pub struct TrustedAccountConfig {
    pub origin: String,
    pub issuer: String,
    pub google_server_client_id: String,
    pub credential_channel: String,
    pub platform: DevicePlatform,
    pub credential_rollout: Option<koru_account_session::RegionalCredentialPolicy>,
    /// A local object worker this build may send blob bytes to, beside the
    /// compiled-in per-region set (`cloud-blob-object-v1` §2). Empty on every
    /// build that is not pointed at somebody's development stack — which is
    /// every build anyone ships.
    ///
    /// **It exists because a phone has no environment.** The desktop reads
    /// `KORU_CLOUD_BLOB_BYTE_ORIGINS`; Android's configuration is
    /// `BuildConfig` fields compiled from `.env.local`, and nothing sets a
    /// variable in that process. Without this a phone allocates a session,
    /// is handed `http://192.168.1.118:8081/` by the operator's own stack and
    /// refuses its own byte URL — which reads as a broken transfer engine
    /// rather than a missing setting.
    ///
    /// **Its rule is not applied here.** Where a person's file may be sent is
    /// the allowlist's question and it is answered in exactly one place: this
    /// goes to `BlobSync::set_development_origins`, which admits only a
    /// loopback, private-range or link-local address and refuses everything
    /// else with the compiled-in set left standing. Refusing account
    /// activation over it would take a working phone down for a setting that
    /// is not part of the account at all.
    pub blob_byte_origins: String,
}

impl TrustedAccountConfig {
    fn validate(&self) -> Result<(), &'static str> {
        if let Some(policy) = &self.credential_rollout {
            policy
                .validate()
                .map_err(|_| "account_configuration_invalid")?;
            if self.issuer != policy.issuance_issuer() {
                return Err("account_configuration_invalid");
            }
        }
        if !matches!(self.platform, DevicePlatform::Android | DevicePlatform::Ios) {
            return Err("account_platform_invalid");
        }
        if self.issuer.is_empty()
            || self.issuer.len() > 2048
            || self.issuer.chars().any(char::is_whitespace)
            || self.issuer.chars().any(char::is_control)
        {
            return Err("account_issuer_invalid");
        }
        Ok(())
    }
}

/// Name this build's local object worker to the transfer engine, and say out
/// loud when the value could not be one.
///
/// **Loud, once, and not fatal** — the desktop's rule, which drops an
/// unusable `KORU_CLOUD_BLOB_BYTE_ORIGINS` and lets the production set stand.
/// Unconditional rather than behind a flag because there is nothing routine
/// about it: it is written only when a build carried a value, and it is the
/// one line that explains a phone refusing its own byte URL.
fn install_development_byte_origins(host: &Arc<MobileHost>, raw: &str) {
    let raw = raw.trim();
    if raw.is_empty() {
        // **Say so.** An empty value is the ordinary case for every build that
        // is not pointed at somebody's development stack, but it is also what
        // a misspelt `.env.local` name, a stale APK and a Gradle property that
        // never arrived all look like — and the symptom of all four is a phone
        // refusing its own stack's byte URL, which reads as a broken transfer
        // engine. Three of the four states here are already news; silence for
        // the fourth is what makes the other three hard to trust.
        tracing::info!(
            target: "koru_blob_sync",
            event = "cloud.blob.development_origins_absent",
            "no development byte origin in this build; only the compiled-in set is admitted",
        );
        return;
    }
    let Some(blobs) = host.blobs() else {
        tracing::warn!(
            target: "koru_blob_sync",
            event = "cloud.blob.development_origins_unavailable",
            "a development byte origin was configured on a host with no transfer engine",
        );
        return;
    };
    match blobs.install_development_origins(raw) {
        // The origins, never the raw value: one is what this device holds and
        // the other is whatever a `.env.local` happened to contain.
        Ok(()) => tracing::info!(
            target: "koru_blob_sync",
            event = "cloud.blob.development_origins_installed",
        ),
        Err(refused) => tracing::warn!(
            target: "koru_blob_sync",
            event = "cloud.blob.development_origins_refused",
            code = refused.message(),
        ),
    }
}

pub(crate) fn activate(
    host: &Arc<MobileHost>,
    configuration: TrustedAccountConfig,
) -> Result<(), &'static str> {
    configuration.validate()?;

    // **A development byte origin belongs to this build, not to this account**,
    // so it is installed before the already-activated check below rather than
    // after it. That check returns `Ok(())` on every launch after the first —
    // the account is already composed and the configuration is identical — and
    // installing afterwards meant the origin reached the engine exactly once,
    // on the launch that first configured the account, and never again. A
    // phone restarted after that kept only the compiled-in production set and
    // refused its own stack's byte URL with `byte_origin_refused`, which reads
    // as a broken transfer engine rather than a setting that was dropped.
    // Found on a physical phone; every lane below this is one process.
    //
    // Installing it twice is harmless: the engine replaces the development
    // allowance rather than accumulating one.
    install_development_byte_origins(host, &configuration.blob_byte_origins);

    let mut account = host.account.lock().map_err(|_| "account_lock")?;
    if let Some(current) = account.as_ref() {
        return if current.configuration.as_ref() == Some(&configuration) {
            Ok(())
        } else {
            Err("account_configuration_conflict")
        };
    }

    // No repository is opened or initialized here. The host has already
    // completed migration and identity verification, or activation stops.
    let descriptor = host
        .trust
        .lock()
        .map_err(|_| "account_trust_not_ready")?
        .as_ref()
        .ok_or("account_trust_not_ready")?
        .local_descriptor()
        .map_err(|_| "account_trust_not_ready")?;
    let signed = koru_account_wire::SignedDeviceDescriptorV1::try_from(descriptor.clone())
        .map_err(|_| "account_device_identity")?;
    let entry = CredentialContext::new(
        &configuration.credential_channel,
        &host.root,
        descriptor.device_id,
    )
    .map_err(|_| "credential_scope")?
    .entry(&configuration.origin)
    .map_err(|_| "account_origin")?;
    let mut client = AccountClient::new(&configuration.origin).map_err(|_| "account_origin")?;
    if let Some(policy) = &configuration.credential_rollout {
        client = client
            .with_regional_credentials(policy.clone())
            .map_err(|_| "account_configuration_invalid")?;
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let entropy: Arc<dyn Entropy> = Arc::new(SystemEntropy);
    let bridge = PlatformBridge::with_clock(
        configuration.google_server_client_id.clone(),
        Arc::clone(&clock),
    )?;
    let local_profiles: Arc<dyn lifecycle::LocalProfileInventory> =
        Arc::new(EmbeddedProfileInventory(host.store.clone()));
    let lifecycle_repository =
        lifecycle::LifecycleRepository::open(&host.root.join("profile-lifecycle.db"))
            .map_err(|_| "account_lifecycle_unavailable")?;
    let identity_db = host.root.join("trust.db");
    let signing_secret = host.signing_key.secret_bytes();
    let port = ProductionPort::new(
        client.clone(),
        configuration.issuer.clone(),
        identity_db.clone(),
        descriptor.clone(),
        signing_secret,
        host.network.clone(),
        Arc::clone(&clock),
        Arc::clone(&entropy),
    )?
    .with_local_profiles(Arc::clone(&local_profiles));
    let changed_host = Arc::downgrade(host);
    let enrollment = Arc::new(Control::start_with_clock_and_foreground(
        port,
        Arc::clone(&clock),
        false,
        move |report| {
            if report.durable_revision.is_some()
                && let Some(host) = changed_host.upgrade()
            {
                host.refresh_authorization();
            }
        },
    ));
    let responder_session = Arc::new(ResponderSessionGate::new());
    let responder_host = Arc::downgrade(host);
    let responder = ProductionResponder::new(
        client.clone(),
        configuration.issuer.clone(),
        Arc::clone(&local_profiles),
        Arc::clone(&clock),
        identity_db.clone(),
        descriptor,
        signing_secret,
        Arc::clone(&responder_session),
        move |_| {
            if let Some(host) = responder_host.upgrade() {
                host.refresh_authorization();
            }
        },
    );
    let mut service = ProductionAccountService::new(
        client.clone(),
        signed,
        koru_device::DeviceSigningKey::from_secret_bytes(signing_secret),
        configuration.platform,
        env!("CARGO_PKG_VERSION").into(),
        AccountPorts {
            delivery: Arc::new(NativeDelivery(Arc::clone(&bridge))),
            local_profiles,
            lifecycle_repository,
            clock: Arc::clone(&clock),
            entropy: Arc::clone(&entropy),
            // The phone enumerates its own Workspaces from the embedded
            // Store — the same catalog the Workspaces screen lists — and
            // remembers the switch beside its other durable state, so the
            // shared handler applies the choice here exactly as on desktop.
            cloud_sync: Some(Arc::new(super::cloud_sync::MobileCloudSync::new(
                host.store.clone(),
                &host.root,
            ))),
        },
    )
    .with_responder_session(Arc::clone(&responder_session));
    let directory = Arc::new(
        koru_account_runtime::account::directory::SqliteAccountDirectory::new(identity_db),
    );
    // The Cloud coordinator, composed only when the configuration carries a
    // regional credential policy: that names the region and issuer a sync
    // credential is verified against. Without one
    // the account works as before and Cloud is not attached.
    let mut inspection_port = None;
    let cloud: Option<Arc<dyn koru_account_runtime::cloud::Driver>> =
        configuration.credential_rollout.as_ref().map(|policy| {
            let host = super::cloud::MobileCloudHost::new(
                Arc::downgrade(host),
                policy.region,
                // The sync credential is signed under the regional issuer.
                policy.regional_issuer.clone(),
                // The account API this build was configured with. The blob
                // control routes hang off it (`cloud-blob-object-v1` §3);
                // where bytes may be *sent* stays compiled in per region.
                configuration.origin.clone(),
            );
            inspection_port = Some(host.inspection_port());
            let coordinator = koru_account_runtime::cloud::Control::start_with_clock(
                koru_account_runtime::cloud::HostedApi::new(client.clone()),
                host,
                Arc::clone(&clock),
                Arc::clone(&entropy),
                false,
                |snapshot| {
                    tracing::debug!(
                        stage = snapshot.stage.as_str(),
                        records = snapshot.records.len(),
                        "cloud: status changed"
                    );
                },
            );
            Arc::new(coordinator) as Arc<dyn koru_account_runtime::cloud::Driver>
        });
    if let Some(port) = inspection_port {
        service = service.with_cloud_document_port(port);
    }
    let mut composed = MobileAccount::compose(service, bridge, entry, directory, enrollment, cloud);
    let value = Arc::get_mut(&mut composed).expect("new account is not shared yet");
    value.control.set_responder_session_gate(responder_session);
    value.configuration = Some(configuration);
    host.network
        .set_enrollment_handler(Some(Arc::new(responder)));
    *account = Some(composed);
    Ok(())
}

#[cfg(test)]
mod tests;
