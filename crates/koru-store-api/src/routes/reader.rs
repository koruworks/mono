// SPDX-License-Identifier: MPL-2.0

//! `/v1/readings/reader` — **the reader that finds words in pictures, on this
//! computer**: whether it is here, what downloading it would mean, the
//! download's progress, and the things a person can say about it — turn it
//! on, stop, turn it off and on again, remove it, not now (OCR TECHNICAL D16,
//! §10; OCR Plan 3.5 for off and on).
//!
//! ## Why the daemon answers through a trait
//!
//! The installer runs in the Store daemon, beside the coordinator that uses
//! the model, and links the model store; this crate links neither, the way
//! [`routes::search`](crate::routes::search) links no index. The daemon
//! implements [`ReaderService`] and hands it down once at start-up.
//!
//! ## Nothing here waits
//!
//! Every method answers from memory. The installer's own thread fetches the
//! catalog, writes the consent record and the *Not now* flag, downloads,
//! verifies and removes; a request only reads what it last published, or
//! hands it a command. So none of these goes through `spawn_blocking`, and a
//! download of any size never holds a request — or a window — open.
//!
//! ## Scopes
//!
//! Reading the state is `read_documents`. Everything that changes it is
//! `edit_documents`: a credential that may not change a document may not
//! download a model onto the machine, delete one, or answer an offer for the
//! person — the same reasoning `POST /v1/search/navigated` gives for a write
//! that touches no document.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, State};
use koru_store_core::errors::{Code, Error};
use koru_store_core::model::Scope;
use koru_store_wire::reader::{ImportFinished, InstallReader, ReaderState};

use crate::auth::Caller;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// **What this crate needs from the daemon's installer.** Every method answers
/// from memory and never blocks on the disk, the network, a worker or the
/// Store lock.
pub trait ReaderService: Send + Sync + 'static {
    /// The reader's state, as the installer and the coordinator last
    /// published it.
    fn state(&self) -> ReaderState;
    /// **Turn on.** Refused `reader_unavailable` where no reader can be used,
    /// and `reader_terms_changed` when `request.terms` are not what the Store
    /// would now install. Otherwise the installer records the consent and
    /// downloads; progress is in [`ReaderService::state`].
    fn install(&self, request: &InstallReader) -> Result<ReaderState, Error>;
    /// **Stop** a download. A partial download is kept, so the next install
    /// resumes it. Answering with nothing to stop is not an error.
    fn stop(&self) -> ReaderState;
    /// **Remove** the model from disk. Readings already made stay.
    fn remove(&self) -> Result<ReaderState, Error>;
    /// **Turn off** (OCR Plan 3.5): reading stops and the download stays.
    /// Nothing is recognized, nothing is offered, and readings already made
    /// stay findable. Machine-local. Answering with nothing installed is not
    /// an error: there is nothing to turn off.
    fn turn_off(&self) -> Result<ReaderState, Error>;
    /// **Turn on** a reader that is downloaded and off: no download, and what
    /// waited for it is queued as after an install. Answering when it is
    /// already on, or not downloaded, is not an error.
    fn turn_on(&self) -> Result<ReaderState, Error>;
    /// **Not now.** Search answers carry no offer until an import brings a
    /// picture or a scanned PDF.
    fn decline(&self) -> Result<ReaderState, Error>;
    /// An import finished, having brought this many pictures and scanned
    /// PDFs. A non-zero count clears a *Not now*.
    fn import_finished(&self, report: &ImportFinished) -> Result<ReaderState, Error>;
}

/// The installer, or an honest refusal. A host that runs none — a phone, the
/// integration host — answers `reader_unavailable`, the same code a desktop
/// that cannot use one answers.
fn service(state: &AppState) -> ApiResult<Arc<dyn ReaderService>> {
    state.reader_service().ok_or_else(|| {
        ApiError::new(
            Code::ReaderUnavailable,
            "this store runs no reader installer",
        )
    })
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::ReadDocuments)?;
    Ok(Json(service(&state)?.state()))
}

pub async fn install(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(request): Json<InstallReader>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::EditDocuments)?;
    Ok(Json(service(&state)?.install(&request)?))
}

pub async fn stop(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::EditDocuments)?;
    Ok(Json(service(&state)?.stop()))
}

pub async fn remove(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::EditDocuments)?;
    Ok(Json(service(&state)?.remove()?))
}

pub async fn turn_off(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::EditDocuments)?;
    Ok(Json(service(&state)?.turn_off()?))
}

pub async fn turn_on(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::EditDocuments)?;
    Ok(Json(service(&state)?.turn_on()?))
}

pub async fn decline(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::EditDocuments)?;
    Ok(Json(service(&state)?.decline()?))
}

pub async fn imported(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<Caller>,
    Json(report): Json<ImportFinished>,
) -> ApiResult<Json<ReaderState>> {
    caller.require(Scope::EditDocuments)?;
    Ok(Json(service(&state)?.import_finished(&report)?))
}
