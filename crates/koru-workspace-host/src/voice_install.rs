// SPDX-License-Identifier: MPL-2.0

//! **First use: the consent step, and the download it authorises.**
//!
//! Plan 2.5. Dictation is a feature that costs somebody several hundred
//! megabytes and asks for their microphone, and neither of those may happen
//! because a key was pressed. This module is the offer, the record and the
//! work; [`crate::app::App`] holds the state and routes it to a window, the
//! way it already does for a content import.
//!
//! ## Where each half of the gate lives, and why not here
//!
//! The gate is deliberately **not** a check in this file that somebody could
//! forget to write at the next call site.
//!
//! - **The download** is gated by a signature:
//!   `koru_voice_models::ModelStore::install` asks for a
//!   [`koru_voice_consent::Granted`] by value, so a caller who has asked
//!   nobody cannot compile, and a grant that does not cover the offer — a
//!   different licence, more bytes than were on the screen — is refused by the
//!   store itself.
//! - **The microphone** is gated by `koru_dictation::Dictation::press`, which
//!   refuses with `ErrorCode::ConsentRequired` until a host hands it a token.
//!   The controller is the only thing in a window that opens a session.
//!
//! What is here is the *offer*: reading a catalog, choosing something to show,
//! recording the answer, and running the install off the event loop.
//!
//! ## Why the window does this and not the agent
//!
//! The tray is where a person first meets the row, and the agent is the process
//! that is always there — both true, and neither decides it. Two things do.
//! **The consent step is a modal panel and the progress toast is a toast**, and
//! an agent has neither; the tray's row therefore hands off to a window in any
//! design. And **a window with no agent still works** — `koru.exe` treats an
//! agent that will not start as a degradation rather than a failure — so
//! putting the download behind the agent would make setting voice up the one
//! thing a degraded window could not do. The cost is recorded rather than
//! hidden: a download does not survive the last window closing. It resumes,
//! because the `.part` is keyed by digest and survives a restart, which is
//! exactly the gap Plan 1.2 closed for this.
//!
//! ## Nothing here runs on the event loop
//!
//! [`resolve_offer`] reaches the network and [`run_install`] downloads and
//! hashes hundreds of megabytes. Both are `std::thread::spawn`, both post back
//! over an `mpsc` and wake the loop through the proxy — the shape
//! `import_content` already uses, so there is one way a worker talks to this
//! window rather than two.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

use koru_voice::{EngineId, ModelTier};
use koru_voice_consent::{Granted, Terms};
use koru_voice_models::{
    CatalogEntry, CatalogSource, DiskModelStore, InstallProgress, InstallStage, ModelError,
    ModelErrorCode, ModelStore, TrustPolicy,
};

const COMPACT_MODEL_ID: &str = "whisper-base";
const HIGH_ACCURACY_MODEL_ID: &str = "nemotron-3.5-streaming-0.6b-160ms";

/// **What the consent step puts on the screen.**
///
/// The terms and nothing else. A view is handed this rather than a
/// `CatalogEntry` because a window has no business knowing a model id, a
/// digest or a URL — and because what a person agrees to has to be exactly
/// what is recorded afterwards, which is easiest to guarantee when they are
/// the same value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VoiceOffer {
    pub terms: Vec<Terms>,
}

impl VoiceOffer {
    pub fn megabytes(&self) -> u64 {
        koru_voice_consent::megabytes(self.terms.iter().map(|terms| terms.download_bytes).sum())
    }

    /// **The consent step's body, as a list**: a lead line, one line per
    /// model — its name, size, publisher and licence — and a closing line
    /// about the microphone and where the models run, separated by `\n`.
    ///
    /// Every line is assembled from translated templates; the names and the
    /// credits are the signed descriptor's own text and are not translated.
    /// A model whose attribution carries further credits after its first `;`
    /// gets them on an indented line under its own, so everything the
    /// attribution says is still on the screen. Display only: what is
    /// recorded is still exactly [`Self::terms`].
    pub fn consent_body(&self, lang: koru_i18n::Lang) -> String {
        use koru_i18n::{Key, fill, t};
        let mut lines = vec![t(lang, Key::VoiceConsentLead).to_owned(), String::new()];
        for terms in &self.terms {
            let size = terms.megabytes().to_string();
            match Credit::of(&terms.attribution, &terms.licence) {
                Some(credit) => {
                    lines.push(format!(
                        "\u{2022} {}",
                        fill(
                            t(lang, Key::VoiceConsentItem),
                            &[credit.model, &size, credit.publisher, &terms.licence],
                        )
                    ));
                    if let Some(further) = credit.further {
                        lines.push(format!("   {further}"));
                    }
                }
                // Not the usual shape: the whole attribution is the line,
                // with the size, and the licence when it does not name it.
                None => {
                    let mut line = fill(
                        t(lang, Key::VoiceConsentItemPlain),
                        &[&terms.attribution, &size],
                    );
                    if !terms.attribution.contains(terms.licence.as_str()) {
                        line = fill(
                            t(lang, Key::VoiceConsentWithLicence),
                            &[&line, &terms.licence],
                        );
                    }
                    lines.push(format!("\u{2022} {line}"));
                }
            }
        }
        lines.push(String::new());
        lines.push(t(lang, Key::VoiceConsentClosing).to_owned());
        lines.join("\n")
    }
}

/// **An attribution read the way the catalog writes it**:
/// `"<Publisher>, <Model name> (<licence>)[; further credit]"`, for example
/// `"OpenAI, Whisper base (MIT); GGML quantisation published by the
/// whisper.cpp project"`.
///
/// Conservative on purpose, because a credit misread is a credit misstated:
/// the first segment must end in the offer's own licence in parentheses, and
/// what is before it must split at exactly one comma into a publisher and a
/// model name, neither empty. Anything else is `None`, and the caller shows
/// the whole attribution as it was signed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Credit<'a> {
    pub publisher: &'a str,
    pub model: &'a str,
    /// What follows the first `;`, trimmed — `None` when there is nothing.
    pub further: Option<&'a str>,
}

impl<'a> Credit<'a> {
    pub fn of(attribution: &'a str, licence: &str) -> Option<Self> {
        let (head, further) = match attribution.split_once(';') {
            Some((head, rest)) => (head.trim(), Some(rest.trim()).filter(|r| !r.is_empty())),
            None => (attribution.trim(), None),
        };
        let (named, stated) = head.strip_suffix(')')?.rsplit_once(" (")?;
        if stated.trim() != licence.trim() {
            return None;
        }
        let (publisher, model) = named.split_once(',')?;
        let (publisher, model) = (publisher.trim(), model.trim());
        if publisher.is_empty() || model.is_empty() || model.contains(',') {
            return None;
        }
        Some(Self {
            publisher,
            model,
            further,
        })
    }
}

/// What a voice worker thread hands back.
pub(crate) enum VoiceMessage {
    /// The catalog answered. `Err` is why there is nothing to offer — an
    /// unreachable catalog, a build with no pinned signing key, a catalog with
    /// no model for this tier.
    Offered(Box<Result<Vec<CatalogEntry>, ModelError>>),
    /// Where the install has got to. Already coalesced by the store to at most
    /// one per 250 ms or per whole percent.
    Progress {
        progress: InstallProgress,
        model: usize,
        models: usize,
    },
    /// The install ended. `Cancelled` is one of the ways.
    Finished(Box<Result<(), ModelError>>),
    /// Every installed model was deleted, or the delete failed. `Ok` carries
    /// how many builds went.
    Removed(Box<Result<usize, ModelError>>),
    /// The worker answered which capture devices it can see. An empty list is
    /// the ordinary answer when no worker is running -- see
    /// [`crate::voice_devices`], which is where the reading lives.
    Devices(Vec<koru_voice::InputDevice>),
}

/// **Resolve the exact Compact and High-accuracy product entries.** Blocking;
/// runs on a worker thread. Entries already installed are omitted unless there
/// is no consent record, in which case their terms still have to be shown.
///
/// The signature is not checked here and that is deliberate: `install` verifies
/// it before a byte is fetched, and verifying twice would put a second opinion
/// about the trust root in a function whose job is to find a row. What *is*
/// checked is that there is a row at all — `Catalog::choose` already skips an
/// entry from a schema this build does not understand, so a model from the
/// future does not stop the ones this build could install.
pub(crate) fn resolve_offer(root: Option<PathBuf>) -> Result<Vec<CatalogEntry>, ModelError> {
    let catalog = CatalogSource::resolve()?.fetch()?;
    // **What each tier weighs, taken while a catalog is in hand.** The
    // Settings section says it, and a number typed into a surface drifts from
    // what actually downloads the first time a model is repackaged --
    // `note_tier_size`'s note is the argument.
    for entry in &catalog.entries {
        if entry.understood() {
            note_tier_size(entry.descriptor.tier, entry.descriptor.size);
        }
    }
    let product = [
        (
            COMPACT_MODEL_ID,
            ModelTier::Compact,
            EngineId::parse("whisper").expect("product engine id"),
        ),
        (
            HIGH_ACCURACY_MODEL_ID,
            ModelTier::HighAccuracy,
            EngineId::parse("sherpa").expect("product engine id"),
        ),
    ];
    let store = DiskModelStore::offline(koru_voice_models::ModelPaths::resolve(root.as_deref())?);
    let installed = store.installed()?;
    let mut entries = Vec::new();
    let mut all_entries = Vec::new();
    for (id, tier, engine) in product {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| {
                entry.understood()
                    && entry.descriptor.id.as_str() == id
                    && entry.descriptor.tier == tier
                    && entry.descriptor.engine == engine
            })
            .cloned()
            .ok_or_else(|| {
                ModelErrorCode::LanguageUnsupported.with(format!(
                    "the catalog lists no supported {id} {} model for {engine}",
                    tier.as_str()
                ))
            })?;
        all_entries.push(entry.clone());
        let present = installed.iter().any(|model| {
            model.id() == &entry.descriptor.id
                && model.version() == &entry.descriptor.version
                && model.descriptor.digest == entry.descriptor.digest
        });
        if !present {
            entries.push(entry);
        }
    }
    // Models can be provisioned outside the UI. If their machine-local
    // consent record is absent, show the real two-model terms and let the
    // installer discover that no bytes need fetching.
    if entries.is_empty() && read_consent(root.as_deref()).is_none() {
        entries = all_entries;
    }
    Ok(entries)
}

/// **How much a model has to prove, chosen here and nowhere else.**
///
/// One expression, and the whole of the MVP's trust decision:
///
/// - **If a key is pinned, `Signed` — always.** The moment
///   `MODEL_SIGNING_KEYS` gains an entry this function stops being able to
///   answer anything else, and the second arm below becomes dead code somebody
///   deletes rather than a switch somebody has to remember to flip.
/// - **Otherwise, the guarded digest-only policy**, which
///   `TrustPolicy::digest_only` refuses outright on a build compiled for
///   `stable`. That refusal falls back to `Signed`, which then refuses the
///   install with `no_trust_root` — the honest failure, and the one a public
///   release must have.
///
/// The reasoning, the guard and the log line are all in
/// [`koru_voice_models::TrustPolicy`]'s own note; what is here is the choice,
/// made once at the only place a store that will fetch is built.
pub(crate) fn trust_policy() -> TrustPolicy {
    if !koru_voice_models::MODEL_SIGNING_KEYS.is_empty() {
        return TrustPolicy::SIGNED;
    }
    TrustPolicy::digest_only().unwrap_or(TrustPolicy::SIGNED)
}

/// **Fetch, verify and publish**, reporting progress and answering
/// cancellation. Blocking; runs on a worker thread.
///
/// `consent` is what makes this callable at all — see the module note; the
/// trust policy is [`trust_policy`]'s, chosen once rather than passed in, so a
/// second caller cannot choose a weaker one.
fn run_install_one(
    root: Option<PathBuf>,
    entry: &CatalogEntry,
    consent: &Granted,
    tx: &Sender<VoiceMessage>,
    cancel: &AtomicBool,
    wake: &dyn Fn(),
    model: usize,
    models: usize,
    completed: u64,
    total: u64,
) -> Result<(), ModelError> {
    let store = DiskModelStore::open_with_trust(root.as_deref(), trust_policy())?;
    let mut report = |progress: InstallProgress| {
        // **Queue, then wake** — the client's rule, and the reason a progress
        // report needs no timer to be seen.
        let _ = tx.send(VoiceMessage::Progress {
            progress: InstallProgress {
                stage: progress.stage,
                received: completed.saturating_add(progress.received.min(entry.descriptor.size)),
                total,
            },
            model,
            models,
        });
        wake();
    };
    store.install(
        entry,
        koru_voice_models::TrustedKeys::pinned(),
        consent,
        &mut report,
        cancel,
    )?;
    Ok(())
}

/// Install every missing product model, retaining each successful install if a
/// later download is cancelled or fails.
pub(crate) fn run_install(
    root: Option<PathBuf>,
    entries: &[CatalogEntry],
    consent: &Granted,
    tx: &Sender<VoiceMessage>,
    cancel: &AtomicBool,
    wake: &dyn Fn(),
) -> Result<(), ModelError> {
    let total: u64 = entries.iter().map(|entry| entry.descriptor.size).sum();
    let existing =
        DiskModelStore::offline(koru_voice_models::ModelPaths::resolve(root.as_deref())?)
            .installed()?;
    let mut completed = 0u64;
    for (index, entry) in entries.iter().enumerate() {
        if existing.iter().any(|model| {
            model.id() == &entry.descriptor.id
                && model.version() == &entry.descriptor.version
                && model.descriptor.digest == entry.descriptor.digest
        }) {
            completed = completed.saturating_add(entry.descriptor.size);
            continue;
        }
        run_install_one(
            root.clone(),
            entry,
            consent,
            tx,
            cancel,
            wake,
            index + 1,
            entries.len(),
            completed,
            total,
        )?;
        completed = completed.saturating_add(entry.descriptor.size);
    }
    Ok(())
}

/// **What a person is told when the catalog could not be read.**
///
/// One sentence per reason, and the reason this function exists at all: every
/// one of these used to arrive as *"No voice model is available to download
/// yet"*, which is true of exactly one of them and actively misleading about
/// the rest. Somebody who set `KORU_VOICE_CATALOG` to a host with nothing on
/// it pressed Install, got that sentence, and had no way to tell it apart from
/// a build whose catalog is simply not published — which is the failure this
/// whole pass is about. *A failure that reaches no surface is the bug*, and a
/// failure that reaches a surface saying nothing actionable is the same bug
/// with a toast on it.
///
/// **Each sentence names the thing to check**, because that is the difference
/// between broken and fixable: a connection, an address, a build, a disk.
///
/// ## English, deliberately
///
/// Every other sentence on this path goes through `koru-i18n`. These do not,
/// for the reason the Settings section's own strings do not (Q-V75): the owner
/// has taken the i18n framework and a full wording pass as their own piece of
/// work before release, and committing twenty rows of translation to copy that
/// pass is going to rewrite costs more than it buys. What is *stored* — the
/// `ModelErrorCode` token — is already language-independent, which is the half
/// that would have been expensive to get wrong. `Remove speech models` above
/// already set this precedent for exactly the same reason.
pub fn catalog_failure(error: &ModelError) -> String {
    match error.code {
        // The network. Named first because it is the most common and the one
        // a person can most often do something about.
        ModelErrorCode::Offline => {
            "Could not reach the model catalog. Check your internet connection and try again."
                .to_owned()
        }
        // The address answered and had nothing to say. This is the one the
        // default URL produces today.
        ModelErrorCode::CatalogUnavailable => {
            "The model catalog is not published at the address this build uses.              If you set KORU_VOICE_CATALOG, check that it points at a catalog file."
                .to_owned()
        }
        // A body arrived and was not a catalog: malformed JSON, a schema this
        // build does not know, an entry that breaks its own rules, a URL that
        // is not HTTPS.
        ModelErrorCode::CatalogInvalid | ModelErrorCode::SchemaUnsupported => {
            "The model catalog could not be read. It is not in a form this build understands, \
             which usually means this build is older than the catalog."
                .to_owned()
        }
        // A catalog we understood, with nothing in it for us.
        ModelErrorCode::LanguageUnsupported => {
            "The model catalog has no speech model this build can install.".to_owned()
        }
        // A build that pins no signing key cannot verify anything, so it may
        // install nothing. Named rather than folded into the sentence above,
        // because it is a property of the *build* and not of the catalog.
        ModelErrorCode::NoTrustRoot => {
            "This build cannot install speech models: it carries no model-signing key.".to_owned()
        }
        // Everything else that can reach here is a fault in what was fetched
        // rather than in reaching it. The code goes in the log beside this.
        _ => format!(
            "The model catalog could not be used ({}). The details are in the log.",
            error.code.as_str()
        ),
    }
}

/// **What a person is told when the download or the install failed.**
///
/// The same reasoning as [`catalog_failure`] and the same split: the four
/// things that go wrong here are a network, a disk, a verification and a
/// build, and *"The voice model could not be installed"* covers all four while
/// answering none of them.
///
/// A cancellation never reaches this — see [`was_cancelled`], which the caller
/// asks first.
pub fn install_failure(error: &ModelError) -> String {
    match error.code {
        ModelErrorCode::Offline | ModelErrorCode::DownloadFailed => {
            "The speech model could not be downloaded.              Check your internet connection and press Install again —              a partial download is kept, so it will carry on rather than start over."
                .to_owned()
        }
        // **The security answers, and they are not one sentence.** A build
        // with no key is a packaging problem; a signature that does not verify
        // is somebody having changed something.
        ModelErrorCode::NoTrustRoot => {
            "This build cannot install speech models: it carries no model-signing key.".to_owned()
        }
        ModelErrorCode::SignatureMalformed
        | ModelErrorCode::SignatureUnknownKey
        | ModelErrorCode::SignatureInvalid => {
            "The speech model was refused: its signature does not match this build's key.              Nothing was installed."
                .to_owned()
        }
        ModelErrorCode::DigestMismatch | ModelErrorCode::SizeMismatch => {
            "The downloaded speech model did not match what the catalog described, \
             so nothing was installed. Press Install again to fetch it afresh."
                .to_owned()
        }
        ModelErrorCode::IdentityConflict => {
            "A different build of this speech model is already installed.              Remove the installed models and try again."
                .to_owned()
        }
        ModelErrorCode::Io | ModelErrorCode::InstallFailed => {
            "The speech model could not be written to disk.              Check that there is room on the drive Koru stores its data on."
                .to_owned()
        }
        ModelErrorCode::ConsentRequired => {
            "The download was not authorised. Press Install again and accept the terms."
                .to_owned()
        }
        _ => format!(
            "The speech model could not be installed ({}). The details are in the log.",
            error.code.as_str()
        ),
    }
}

/// **What a person is told when a set-up gesture could not even be started.**
///
/// Two states, and both used to be a silent `return`: a press while the
/// catalog is already being fetched or a model is being downloaded, and a
/// press on an installation where voice is switched off. Neither is a failure
/// somebody caused, and neither is a reason to say nothing — a button that
/// does nothing at all is indistinguishable from one that is broken.
///
/// `None` means *there is nothing to say and the gesture may proceed*.
pub fn setup_refused(busy: bool, disabled: bool) -> Option<&'static str> {
    if disabled {
        return Some(
            "Voice input is switched off on this installation (KORU_VOICE_DISABLED), \
             so there is nothing to install.",
        );
    }
    if busy {
        return Some("Voice input is already being set up on this installation.");
    }
    None
}

/// Whether an install failure was somebody pressing **Stop**.
///
/// A cancellation is not a failure and must not be reported as one — the toast
/// has already said *Canceling* and is about to close itself, and a second
/// sentence saying the model could not be installed would read as a bug.
pub(crate) fn was_cancelled(error: &ModelError) -> bool {
    error.code == ModelErrorCode::Cancelled
}

/// **What the progress toast says**, and how full its bar is.
///
/// One function, so the toast that is created and the toast that is updated
/// cannot word the same stage two ways. `None` is an indeterminate bar.
///
/// The five stages become four sentences: *Resolving* and *Downloading* are
/// both "downloading" from where somebody is sitting, and `Unpacking` and
/// `Publishing` are both "installing". What is **not** collapsed is
/// `Verifying`, and that is the whole reason `InstallStage` has it: re-hashing
/// 650 MB is minutes on a slow disk, and a bar sitting at 100% for two minutes
/// reads as a hang.
pub(crate) fn progress_line(
    lang: koru_i18n::Lang,
    progress: InstallProgress,
) -> (String, Option<f32>) {
    use koru_i18n::Key;
    match progress.stage {
        InstallStage::Resolving => (koru_i18n::t(lang, Key::VoiceDownloadPreparing).into(), None),
        InstallStage::Downloading => match progress.total {
            0 => (koru_i18n::t(lang, Key::VoiceDownloadPreparing).into(), None),
            total => {
                let fraction = (progress.received.min(total) as f64 / total as f64) as f32;
                let percent = (fraction * 100.0).round() as u8;
                (
                    koru_i18n::fill(
                        koru_i18n::t(lang, Key::VoiceDownloadProgress),
                        &[&percent.to_string()],
                    ),
                    Some(fraction),
                )
            }
        },
        InstallStage::Verifying => (koru_i18n::t(lang, Key::VoiceDownloadChecking).into(), None),
        InstallStage::Unpacking | InstallStage::Publishing => (
            koru_i18n::t(lang, Key::VoiceDownloadInstalling).into(),
            None,
        ),
    }
}

/// **What a tier weighs, as the last descriptor we saw declared it.**
///
/// Two slots, one per [`ModelTier`], each holding megabytes or nothing.
///
/// **A remembered reading, not a literal, and not a fetch.** The Settings
/// section explains what the Quality choice costs, and the honest number is
/// the *signed* `size` from a descriptor — the same field the consent step
/// shows and the same one the install verifies against. Typing `57` into the
/// UI would be right until the first repack and wrong silently afterwards.
///
/// It is filled from two places, both of them descriptors and neither of them
/// a network call made for this: [`installed_models`] records what is on the
/// disk, and [`resolve_offer`] records what the catalog listed when somebody
/// pressed Install. **Nothing here reaches the network to populate it** — a
/// settings page that fetched a catalog in order to draw a sentence would be a
/// page that phoned home when it was opened. Until one of the two has
/// happened, the answer is `None` and the sentence is drawn without a number.
static TIER_MEGABYTES: [std::sync::atomic::AtomicU64; 2] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

fn tier_slot(tier: ModelTier) -> usize {
    ModelTier::ALL.iter().position(|t| *t == tier).unwrap_or(0)
}

/// Record what a descriptor said this tier weighs. Bytes in, megabytes stored,
/// because megabytes is what is shown and rounding once is one answer.
pub fn note_tier_size(tier: ModelTier, bytes: u64) {
    if bytes == 0 {
        return;
    }
    TIER_MEGABYTES[tier_slot(tier)].store(
        koru_voice_consent::megabytes(bytes),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// What this tier weighs, in megabytes, or `None` if no descriptor has said.
pub fn tier_megabytes(tier: ModelTier) -> Option<u64> {
    match TIER_MEGABYTES[tier_slot(tier)].load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        megabytes => Some(megabytes),
    }
}

/// **What the settings section may say about the models on this machine.**
///
/// Counts and bytes, and nothing that could be mistaken for a stored flag —
/// see [`installed_models`], which is the only way to obtain one.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct VoiceModels {
    /// Complete, verified model builds in the cache.
    pub builds: usize,
    /// What they occupy, as the *signed descriptors* declare it. The archive's
    /// length rather than a directory walk: the number a person agreed to
    /// download is the number they should be shown afterwards, and a `du` over
    /// unpacked weights is a different figure that nobody promised them.
    pub bytes: u64,
    /// Availability by [`ModelTier::ALL`] index.
    pub tiers: [bool; 2],
}

impl VoiceModels {
    pub fn installed(&self) -> bool {
        self.builds > 0
    }

    pub fn complete(&self) -> bool {
        self.tiers.into_iter().all(|installed| installed)
    }
}

/// **Ask the disk what is installed. Never a stored answer.**
///
/// [`koru_voice_models::anything_installed`]'s note is the contract and this is
/// its caller: installed state is *derived* from the verified cache every time,
/// because a flag beside the consent record would answer *installed* for a
/// cache another channel had pruned and nothing would fail until a dictation
/// did.
///
/// **An offline store**, which is what makes this safe to ask from a settings
/// page: [`DiskModelStore::offline`] has no source, so reading what is here
/// cannot reach the network, cannot construct an HTTP client, and cannot be
/// turned into a download by a later edit. It reads directories, so it is the
/// world in [PRINCIPLES](../../../guide/contributing/PRINCIPLES.md)'s sense — a caller asks
/// it when a surface is built or when something has changed, never from a
/// frame.
///
/// A store that will not open at all answers "nothing here", the same reading
/// the tray already takes: *nothing usable* is what a person can act on, and
/// the specific failure belongs in the log beside it.
pub fn installed_models() -> VoiceModels {
    if cfg!(feature = "no-model-reader") {
        return VoiceModels::default();
    }
    let paths =
        match koru_voice_models::ModelPaths::resolve(crate::cli::root_from_args().as_deref()) {
            Ok(paths) => paths,
            Err(error) => {
                tracing::debug!(
                    target: "koru::voice",
                    error = %error,
                    "no speech-model cache could be resolved on this machine"
                );
                return VoiceModels::default();
            }
        };
    let store = DiskModelStore::offline(paths);
    match store.installed() {
        Ok(models) => {
            // The same signed field, from the other of its two sources -- see
            // [`TIER_MEGABYTES`]. A machine with a model already installed can
            // therefore say what the tiers weigh without ever having fetched a
            // catalog.
            for model in &models {
                note_tier_size(model.descriptor.tier, model.descriptor.size);
            }
            VoiceModels {
                builds: models.len(),
                bytes: models.iter().map(|m| m.descriptor.size).sum(),
                tiers: [COMPACT_MODEL_ID, HIGH_ACCURACY_MODEL_ID]
                    .map(|id| models.iter().any(|model| model.id().as_str() == id)),
            }
        }
        Err(error) => {
            tracing::debug!(
                target: "koru::voice",
                error = %error,
                "the speech-model cache could not be read"
            );
            VoiceModels::default()
        }
    }
}

/// **Delete every installed speech model.** Blocking; runs on a worker thread.
///
/// Answers how many builds went. `Leased` is not swallowed: a build another
/// process has loaded must be reported rather than silently kept, because
/// *Remove* is an instruction and an instruction that quietly does nothing is
/// the failure [`ModelStore::prune`] exists to be distinguished from.
pub(crate) fn remove_all_models(root: Option<PathBuf>) -> Result<usize, ModelError> {
    let store = DiskModelStore::offline(koru_voice_models::ModelPaths::resolve(root.as_deref())?);
    let installed = store.installed()?;
    let mut gone = 0;
    for model in &installed {
        store.remove(model.id(), model.version())?;
        gone += 1;
    }
    Ok(gone)
}

/// **The consent record's path**, and the token it holds.
///
/// An explicit channel root redirects the record under its `agent/` sibling,
/// exactly where the rooted Agent reads it. Without a root, `None` from
/// `voice_consent_path` — a machine with no local data directory — reads as
/// *not consented*, the only safe answer for an unreadable state.
fn consent_path(root: Option<&Path>) -> Option<PathBuf> {
    root.map(|root| root.join("agent").join(koru_voice_consent::CONSENT_FILE))
        .or_else(koru_window::voice_consent_path)
}

pub(crate) fn read_consent(root: Option<&Path>) -> Option<Granted> {
    let path = consent_path(root)?;
    match koru_voice_consent::read(&path) {
        koru_voice_consent::Consent::Granted(granted) => Some(granted),
        koru_voice_consent::Consent::Absent(reason) => {
            tracing::debug!(
                target: "koru::voice",
                reason = reason.as_str(),
                "voice input has not been agreed to on this machine"
            );
            None
        }
    }
}

/// **Record that somebody agreed**, and answer the token to hold.
///
/// A record that will not write is still consent *for this session*: the person
/// said yes, and refusing to act on it because a disk is full would be a worse
/// answer than asking again next time. The failure is logged rather than shown,
/// because there is nothing they can do about it in the middle of a sentence.
pub(crate) fn record_consent(root: Option<&Path>, terms: Vec<Terms>) -> Granted {
    let now = koru_voice_consent::now_unix();
    let mut combined: Vec<Terms> = read_consent(root)
        .map(|granted| granted.all_terms().cloned().collect())
        .unwrap_or_default();
    for term in terms {
        if let Some(existing) = combined
            .iter_mut()
            .find(|existing| existing.licence == term.licence)
        {
            if term.download_bytes > existing.download_bytes {
                *existing = term;
            }
        } else {
            combined.push(term);
        }
    }
    match consent_path(root) {
        Some(path) => match koru_voice_consent::grant_all(&path, combined.clone(), now) {
            Ok(granted) => granted,
            Err(error) => {
                tracing::warn!(
                    target: "koru::voice",
                    error = %error,
                    "voice consent could not be written; it will be asked for again"
                );
                Granted::recorded_all(combined, now)
            }
        },
        None => Granted::recorded_all(combined, now),
    }
}

/// **Which language these sentences are read in**, asked of the OS once.
///
/// `apps/koru`'s `format::lang()` is the container's answer and is not
/// reachable from here — the shell is below it. Asking `Lang::for_system` on
/// every call would be a round trip to the OS on the progress path, several
/// times a second for the length of a download, which is exactly the shape of
/// the *cheap-looking getter* the runbook warns about; a locale cannot change
/// under a running process, so it is resolved once.
/// Moved to `crate::accessibility::ui_lang` when a second surface in this
/// crate needed the same one-time answer; a second `OnceLock` would have been
/// a second chance to disagree about what language this shell speaks.
pub(crate) fn lang() -> koru_i18n::Lang {
    crate::accessibility::ui_lang()
}

/// **How long the accepted Stop stays on screen**, before the toast closes.
///
/// Three seconds, and it is DESIGN's number rather than this module's:
/// *"Stop requests cooperative cancellation; accepting it removes the action
/// immediately, changes the stage to Canceling, and closes the toast after
/// three seconds."* The content import holds its own for the same span, and
/// the reason is the same — a worker that reaches its next checkpoint
/// instantly would otherwise make the acknowledgement a flicker.
pub(crate) const CANCEL_HOLD: std::time::Duration = std::time::Duration::from_secs(3);

/// **Everything the window is holding while voice is being set up.**
///
/// One field on `App` rather than eight, because they are one situation: a
/// catalog answer is in flight, or a consent step is open on an entry, or a
/// download is running and can be stopped. Keeping them apart is how two of
/// them come to disagree about whether anything is happening.
pub(crate) struct VoiceSetup {
    pub tx: Sender<VoiceMessage>,
    pub rx: std::sync::mpsc::Receiver<VoiceMessage>,
    /// `--root`, when one was named — the model cache under it rather than the
    /// machine's. Held rather than re-read for the reason `store_root` is.
    pub root: Option<PathBuf>,
    /// A catalog answer is in flight, or a consent step is open. Either way a
    /// second press must not start a second one.
    pub offering: bool,
    /// What the open consent step is about, kept until it is answered.
    pub entries: Option<Vec<CatalogEntry>>,
    /// The window that asked, so the toast lands where the gesture was.
    pub source: Option<winit::window::WindowId>,
    /// The running install's cancel flag, polled between chunks.
    pub cancel: Option<Arc<AtomicBool>>,
    /// When the accepted Stop stops being shown. See [`CANCEL_HOLD`].
    pub cancel_hide_due: Option<std::time::Instant>,
    pub installing: bool,
    /// A device enumeration is in flight. **Not part of [`busy`]**: it opens
    /// nothing, downloads nothing and touches no cache, so a person pressing
    /// Install while a picker is filling itself must not be turned away.
    ///
    /// [`busy`]: VoiceSetup::busy
    pub asking_devices: bool,
    /// A delete of every installed model is running. Shares [`busy`] with the
    /// other two: there is one cache, and offering a download while it is
    /// being emptied is two answers to one question.
    ///
    /// [`busy`]: VoiceSetup::busy
    pub removing: bool,
}

impl VoiceSetup {
    pub fn new(root: Option<PathBuf>) -> VoiceSetup {
        let (tx, rx) = std::sync::mpsc::channel();
        VoiceSetup {
            tx,
            rx,
            root,
            offering: false,
            entries: None,
            source: None,
            cancel: None,
            cancel_hide_due: None,
            installing: false,
            asking_devices: false,
            removing: false,
        }
    }

    /// Whether anything is already in flight. A second press while a catalog is
    /// being fetched or a model is downloading must not start a second of
    /// either — there is one microphone and one cache.
    pub fn busy(&self) -> bool {
        self.offering || self.installing || self.removing
    }

    /// Back to nothing in flight, keeping only what a later pass still needs.
    pub fn settle(&mut self) {
        self.offering = false;
        self.installing = false;
        self.removing = false;
        self.entries = None;
        self.cancel = None;
    }
}

#[cfg(test)]
mod tests;
