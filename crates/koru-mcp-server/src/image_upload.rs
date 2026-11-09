// SPDX-License-Identifier: MPL-2.0

//! Scoped image bytes, then a verified reference, then a document edit.
//!
//! The model never carries the file. [`ImageUploads::create`] mints an opaque
//! `upload_id` and a token. The uploader `PUT`s the raw bytes to
//! `{origin}/v1/image-uploads/{upload_id}` with `Authorization: Upload <token>`.
//! [`ImageUploads::complete`] sniffs them (PNG, JPEG, GIF or WebP, at most 8 MiB)
//! and stores them through [`ImageBlobStore`], which is the workspace blob
//! store the host already has. [`ImageUploads::verified`] is what `add_image`
//! and `replace_image` accept. A hash the caller invented is not a reference.
//!
//! The token is not a store credential and not an account bearer. It is bound
//! to one upload, one workspace, one tenant and one principal, and it dies at
//! `expires_at`. The same bytes may be `PUT` again until then; different bytes
//! are `upload_conflict`. Another workspace, or another principal, cannot place
//! the reference.
//!
//! This module does not fetch URLs. The hosted worker in the private repository
//! mounts the same `PUT` and implements [`ImageBlobStore`] with the existing
//! Cloud blob control and object planes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_core::{Content, Kind, Node, WorkspaceDoc};
#[cfg(test)]
use koru_core::{NodeId, Parent};
#[cfg(test)]
use koru_store_core::model::BlobHash;
use koru_store_core::model::{Blob, Timestamp};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{ToolError, after_index, node, parent, revision, validate_base};

/// How long an upload can be completed and placed. Ten minutes: long enough
/// for an 8 MiB body on a slow link, short enough that a leaked token is not
/// useful for the rest of the day.
pub const IMAGE_UPLOAD_TTL_MS: i64 = 10 * 60 * 1000;

/// `Authorization` scheme on the byte route. Deliberately not `Bearer`: an
/// account token and an upload token must not be interchangeable.
pub const UPLOAD_AUTHORIZATION_SCHEME: &str = "Upload";

pub const UPLOAD_PATH_PREFIX: &str = "/v1/image-uploads/";

const MAX_CAPTION_CHARS: usize = 500;
const MAX_FILENAME_BYTES: usize = 255;
const MAX_HEADER_BYTES: usize = 8 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);
const BODY_TIMEOUT: Duration = Duration::from_secs(60);
/// Bounded even when a caller mints uploads continuously without completing
/// them. Expired grants are retired at the next creation.
const MAX_LIVE_UPLOADS: usize = 4096;

/// Who an upload belongs to. All three are compared on place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadScope {
    pub workspace_id: String,
    pub tenant: String,
    pub principal: String,
}

/// What `create_image_upload` hands the uploader once. `token` is not stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageUploadGrant {
    pub upload_id: String,
    pub token: String,
    pub expires_at_ms: i64,
    /// Absolute origin (`https://mcp.example`) when the minter knows where the
    /// `PUT` is served. The desktop bridge fills this from its loopback
    /// listener instead.
    pub url_origin: Option<String>,
}

/// A completed upload the document tools can name. The hash is the blob
/// store's, not one the caller sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedImage {
    pub upload_id: String,
    pub hash: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub filename: Option<String>,
    pub expires_at_ms: i64,
}

#[derive(Clone)]
struct Record {
    token_hash: String,
    scope: UploadScope,
    filename: Option<String>,
    expires_at_ms: i64,
    verified: Option<VerifiedImage>,
    uploading: bool,
}

/// In-memory uploads. A restart drops them; the TTL is shorter than a process
/// is expected to live, and a persisted token would outlive the secret that
/// should have died with it.
#[derive(Clone, Default)]
pub struct ImageUploads {
    inner: Arc<Mutex<HashMap<String, Record>>>,
}

impl ImageUploads {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &self,
        scope: UploadScope,
        filename: Option<String>,
        now_ms: i64,
    ) -> Result<ImageUploadGrant, ToolError> {
        let filename = filename
            .as_deref()
            .map(check_filename)
            .transpose()?
            .map(str::to_owned);
        let upload_id = uuid::Uuid::new_v4().to_string();
        let token = mint_token();
        let expires_at_ms = now_ms.saturating_add(IMAGE_UPLOAD_TTL_MS);
        let mut records = self.lock();
        records.retain(|_, record| now_ms < record.expires_at_ms);
        if records.len() >= MAX_LIVE_UPLOADS {
            return Err(ToolError {
                code: "image_upload_busy".into(),
                message: "too many image uploads are pending; retry shortly".into(),
                details: None,
            });
        }
        records.insert(
            upload_id.clone(),
            Record {
                token_hash: token_hash(&token),
                scope,
                filename,
                expires_at_ms,
                verified: None,
                uploading: false,
            },
        );
        Ok(ImageUploadGrant {
            upload_id,
            token,
            expires_at_ms,
            url_origin: None,
        })
    }

    /// Sniff `bytes`, store them, and remember the store's hash.
    ///
    /// The same bytes again return the same reference. Different bytes after
    /// a success are `upload_conflict` and are not stored.
    pub fn complete(
        &self,
        upload_id: &str,
        token: &str,
        bytes: &[u8],
        now_ms: i64,
        store: &dyn ImageBlobStore,
    ) -> Result<VerifiedImage, ToolError> {
        let (scope, filename, expires_at_ms) = {
            let mut guard = self.lock();
            let record = guard.get_mut(upload_id).ok_or_else(|| upload_not_found())?;
            if !tokens_match(&record.token_hash, token) {
                return Err(ToolError::invalid("the upload token was not recognised"));
            }
            if now_ms >= record.expires_at_ms {
                return Err(upload_expired());
            }
            if let Some(done) = &record.verified {
                return if content_hash(bytes) == done.hash {
                    Ok(done.clone())
                } else {
                    Err(upload_conflict())
                };
            }
            if record.uploading {
                return Err(ToolError {
                    code: "image_upload_busy".into(),
                    message: "this image is already uploading".into(),
                    details: None,
                });
            }
            record.uploading = true;
            (
                record.scope.clone(),
                record.filename.clone(),
                record.expires_at_ms,
            )
        };

        let result = (|| {
            let sniffed = crate::sniff_image(bytes)?;
            let blob = store.put_image(
                &scope.workspace_id,
                bytes.to_vec(),
                sniffed.mime,
                filename.as_deref(),
            )?;
            Ok::<_, ToolError>(VerifiedImage {
                upload_id: upload_id.to_owned(),
                hash: blob.hash.as_str().to_owned(),
                mime: sniffed.mime.to_owned(),
                width: sniffed.width,
                height: sniffed.height,
                size: blob.size,
                filename,
                expires_at_ms,
            })
        })();

        let mut guard = self.lock();
        let Some(record) = guard.get_mut(upload_id) else {
            return Err(upload_not_found());
        };
        record.uploading = false;
        let verified = result?;
        if now_ms >= record.expires_at_ms {
            return Err(upload_expired());
        }
        if let Some(done) = &record.verified {
            return if done.hash == verified.hash {
                Ok(done.clone())
            } else {
                Err(upload_conflict())
            };
        }
        record.verified = Some(verified.clone());
        Ok(verified)
    }

    /// Check the opaque byte grant before a hosted route borrows the original
    /// MCP token. The upload token never authorizes Cloud access by itself.
    pub fn scope(
        &self,
        upload_id: &str,
        token: &str,
        now_ms: i64,
    ) -> Result<UploadScope, ToolError> {
        let guard = self.lock();
        let record = guard.get(upload_id).ok_or_else(upload_not_found)?;
        if !tokens_match(&record.token_hash, token) {
            return Err(ToolError::invalid("the upload token was not recognised"));
        }
        if now_ms >= record.expires_at_ms {
            return Err(upload_expired());
        }
        Ok(record.scope.clone())
    }

    /// The reference `add_image` / `replace_image` may use.
    pub fn verified(
        &self,
        scope: &UploadScope,
        upload_id: &str,
        now_ms: i64,
    ) -> Result<VerifiedImage, ToolError> {
        let guard = self.lock();
        let Some(record) = guard.get(upload_id) else {
            return Err(upload_not_found());
        };
        if record.scope.principal != scope.principal || record.scope.tenant != scope.tenant {
            return Err(ToolError::permission_denied(
                "this image upload belongs to another principal",
            ));
        }
        if record.scope.workspace_id != scope.workspace_id {
            return Err(upload_workspace_mismatch());
        }
        if now_ms >= record.expires_at_ms {
            return Err(upload_expired());
        }
        record.verified.clone().ok_or_else(upload_not_complete)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Record>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Hosted image edits call this before they write. A device has none: claims
/// are a hosted lease, and a local bridge must not refuse for lack of one.
/// The hosted worker installs the check it already runs for `edit_document`.
/// Until it does, [`crate::claims_gate`] fails closed.
pub trait DocumentClaims: Send + Sync {
    fn enforce(
        &self,
        workspace_id: &str,
        document_id: &str,
        claim_ids: &[String],
    ) -> Result<(), crate::ToolError>;
}

/// Where completed bytes go. The desktop bridge uses the local blob store.
/// The hosted worker uses Cloud blob storage. Both return the hash the store
/// assigned.
pub trait ImageBlobStore: Send + Sync {
    fn put_image(
        &self,
        workspace_id: &str,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<Blob, ToolError>;
}

/// A blob whose address is the BLAKE3 of its bytes, `b3:<hex>`. Tests use this
/// as a stand-in store. It does not invent a second hash function.
#[cfg(test)]
fn content_addressed_blob(bytes: &[u8], mime: &str, filename: Option<&str>) -> Blob {
    Blob {
        hash: BlobHash::from_digest("b3", &content_digest(bytes)).expect("blake3 hex"),
        size: bytes.len() as u64,
        mime: mime.to_owned(),
        filename: filename.map(str::to_owned),
        created_at: Timestamp::from_millis(0),
        attributes: Default::default(),
    }
}

fn content_digest(bytes: &[u8]) -> String {
    koru_store_core::lifecycle::checksum(bytes)
}

fn content_hash(bytes: &[u8]) -> String {
    format!("b3:{}", content_digest(bytes))
}

fn mint_token() -> String {
    // Two v4 UUIDs, hex, no separators: 64 chars, not a UUID a caller could
    // confuse with `upload_id`.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn token_hash(token: &str) -> String {
    content_digest(token.as_bytes())
}

fn tokens_match(stored_hash: &str, token: &str) -> bool {
    let got = token_hash(token);
    if stored_hash.len() != got.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in stored_hash.bytes().zip(got.bytes()) {
        diff |= left ^ right;
    }
    diff == 0
}

pub fn upload_url(origin: &str, upload_id: &str) -> String {
    format!(
        "{}{UPLOAD_PATH_PREFIX}{upload_id}",
        origin.trim_end_matches('/')
    )
}

pub fn image_upload_unavailable() -> ToolError {
    ToolError {
        code: "image_upload_unavailable".into(),
        message: "this Koru host has no image upload endpoint; bytes are not stored and the document is unchanged".into(),
        details: None,
    }
}

fn upload_not_found() -> ToolError {
    ToolError {
        code: "upload_not_found".into(),
        message: "no image upload has that id".into(),
        details: None,
    }
}

fn upload_expired() -> ToolError {
    ToolError {
        code: "upload_expired".into(),
        message: "the image upload has expired; create another and PUT the bytes again".into(),
        details: None,
    }
}

fn upload_not_complete() -> ToolError {
    ToolError {
        code: "upload_not_complete".into(),
        message: "the image upload has no bytes yet; PUT the file before placing it".into(),
        details: None,
    }
}

fn upload_conflict() -> ToolError {
    ToolError {
        code: "upload_conflict".into(),
        message: "this upload already holds different bytes".into(),
        details: None,
    }
}

fn upload_workspace_mismatch() -> ToolError {
    ToolError {
        code: "upload_workspace_mismatch".into(),
        message: "this image upload belongs to another workspace".into(),
        details: None,
    }
}

pub fn claims_unavailable() -> ToolError {
    ToolError {
        code: "claims_unavailable".into(),
        message: "this hosted image edit has no claim check installed; nothing was written".into(),
        details: None,
    }
}

pub fn duplicate_node(node_id: &str) -> ToolError {
    ToolError {
        code: "duplicate_node_id".into(),
        message: format!(
            "node {node_id} already exists and is not this image; read_document and do not insert it again"
        ),
        details: Some(serde_json::json!({ "node_id": node_id })),
    }
}

/// `caption` is the image node's `metadata.name`, the label a view shows
/// instead of "Image N". It is not the blob's filename.
pub fn check_caption(caption: Option<&str>) -> Result<(), ToolError> {
    let Some(caption) = caption else {
        return Ok(());
    };
    if caption.is_empty() || caption.chars().count() > MAX_CAPTION_CHARS || caption.contains('\n') {
        return Err(ToolError::invalid(
            "caption must be 1 to 500 characters and a single line",
        ));
    }
    Ok(())
}

fn check_filename(name: &str) -> Result<&str, ToolError> {
    if name.is_empty()
        || name.len() > MAX_FILENAME_BYTES
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(ToolError::invalid(
            "name must be a single path-free line of at most 255 bytes",
        ));
    }
    Ok(name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaceDecision {
    /// The node already shows this image. Do not write, even if `base_revision`
    /// is stale: a lost response must not insert a second copy.
    Replay,
    Write,
}

pub(crate) fn decide_insert(
    doc: &WorkspaceDoc,
    base_revision: &str,
    node_id: &str,
    image: &VerifiedImage,
    caption: Option<&str>,
) -> Result<PlaceDecision, ToolError> {
    check_caption(caption)?;
    match existing(doc, node_id)? {
        None => {
            validate_base(doc, base_revision)?;
            Ok(PlaceDecision::Write)
        }
        Some(node) if image_matches(&node, image, caption) => Ok(PlaceDecision::Replay),
        Some(_) => Err(duplicate_node(node_id)),
    }
}

pub(crate) fn decide_replace(
    doc: &WorkspaceDoc,
    base_revision: &str,
    node_id: &str,
    image: &VerifiedImage,
    caption: Option<&str>,
) -> Result<PlaceDecision, ToolError> {
    check_caption(caption)?;
    let Some(node) = existing(doc, node_id)? else {
        return Err(crate::ToolError::from(koru_core::StoreError::NoSuchNode(
            node(node_id)?,
        )));
    };
    if node.kind != Kind::Image {
        return Err(ToolError::invalid(
            "replace_image target must be an image block",
        ));
    }
    if image_matches(&node, image, caption) {
        return Ok(PlaceDecision::Replay);
    }
    validate_base(doc, base_revision)?;
    Ok(PlaceDecision::Write)
}

fn existing(doc: &WorkspaceDoc, node_id: &str) -> Result<Option<koru_core::Node>, ToolError> {
    let id = node(node_id)?;
    if doc.kind_of(id).is_none() {
        return Ok(None);
    }
    Ok(Some(doc.read(id)?))
}

/// Hash, type and pixel size identify the picture. A caption is compared only
/// when this call sent one, so a retry that omits it still sees the image it
/// already placed.
fn image_matches(node: &koru_core::Node, image: &VerifiedImage, caption: Option<&str>) -> bool {
    let Content::Blob {
        hash,
        mime,
        width,
        height,
        ..
    } = &node.content
    else {
        return false;
    };
    if node.kind != Kind::Image || hash.0 != image.hash || mime != &image.mime {
        return false;
    }
    if *width != Some(image.width) || *height != Some(image.height) {
        return false;
    }
    match caption {
        None => true,
        Some(caption) => node.meta.name.as_deref() == Some(caption),
    }
}

pub(crate) fn write_insert(
    doc: &mut WorkspaceDoc,
    node_id: &str,
    parent_id: &str,
    after_node_id: Option<&str>,
    image: &VerifiedImage,
    caption: Option<&str>,
) -> Result<(), ToolError> {
    check_caption(caption)?;
    if doc.kind_of(node(node_id)?).is_some() {
        return Err(duplicate_node(node_id));
    }
    let mut value = Node::image(
        koru_core::BlobHash(image.hash.clone()),
        image.mime.clone(),
        Some(image.width),
        Some(image.height),
    )?;
    value.id = node(node_id)?;
    let parent = parent(parent_id)?;
    let index = after_index(doc, parent, after_node_id)?;
    let id = doc.insert(parent, value)?;
    doc.move_node(id, parent, index)?;
    if let Some(caption) = caption {
        doc.set_name(id, Some(caption))?;
    }
    Ok(())
}

pub(crate) fn write_replace(
    doc: &mut WorkspaceDoc,
    node_id: &str,
    image: &VerifiedImage,
    caption: Option<&str>,
) -> Result<(), ToolError> {
    check_caption(caption)?;
    let id = node(node_id)?;
    let current = doc.read(id)?;
    if current.kind != Kind::Image {
        return Err(ToolError::invalid(
            "replace_image target must be an image block",
        ));
    }
    doc.replace_image_bytes(
        id,
        koru_core::BlobHash(image.hash.clone()),
        &image.mime,
        image.width,
        image.height,
    )?;
    if let Some(caption) = caption {
        doc.set_name(id, Some(caption))?;
    }
    Ok(())
}

pub(crate) fn image_result(
    doc: &WorkspaceDoc,
    node_id: &str,
    image: &VerifiedImage,
    replayed: bool,
) -> serde_json::Value {
    let name = node(node_id)
        .ok()
        .and_then(|id| doc.read(id).ok())
        .and_then(|node| node.meta.name);
    serde_json::json!({
        "revision": revision(doc),
        "node_id": node_id,
        "hash": image.hash,
        "mime": image.mime,
        "width": image.width,
        "height": image.height,
        "size": image.size,
        "caption": name,
        "replayed": replayed,
    })
}

/// Bind `127.0.0.1:0`. The desktop bridge is the only process that should
/// call this. A hosted worker terminates its own public listener and calls
/// [`ImageUploads::complete`] from that route; it must not open this socket
/// on a public address.
pub fn bind_loopback() -> std::io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

pub fn spawn_upload_server(
    listener: std::net::TcpListener,
    uploads: ImageUploads,
    store: Arc<dyn ImageBlobStore>,
) -> std::io::Result<std::net::SocketAddr> {
    let addr = listener.local_addr()?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let uploads = uploads.clone();
                    let store = store.clone();
                    tokio::spawn(async move {
                        if let Err(error) = serve_one(socket, uploads, store).await {
                            tracing::debug!(error = %error, "image upload connection closed");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "image upload listener stopped accepting");
                    break;
                }
            }
        }
    });
    Ok(addr)
}

async fn serve_one(
    mut socket: tokio::net::TcpStream,
    uploads: ImageUploads,
    store: Arc<dyn ImageBlobStore>,
) -> Result<(), String> {
    let (method, path, token, body) = read_request(&mut socket).await?;
    let result = if method != "PUT" {
        Err(ToolError::invalid("image upload accepts PUT only"))
    } else {
        match path.strip_prefix(UPLOAD_PATH_PREFIX) {
            Some(id) if uuid::Uuid::parse_str(id).is_ok() => {
                uploads.complete(id, &token, &body, Timestamp::now().millis(), store.as_ref())
            }
            _ => Err(upload_not_found()),
        }
    };
    let (status, payload) = match result {
        Ok(image) => (
            200,
            serde_json::json!({
                "upload_id": image.upload_id,
                "hash": image.hash,
                "mime": image.mime,
                "width": image.width,
                "height": image.height,
                "size": image.size,
                "expires_at": image.expires_at_ms,
            }),
        ),
        Err(error) => (
            http_status(&error.code),
            serde_json::json!({ "code": error.code, "message": error.message }),
        ),
    };
    let bytes = serde_json::to_vec(&payload).map_err(|error| error.to_string())?;
    let header = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        bytes.len()
    );
    socket
        .write_all(header.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    socket
        .write_all(&bytes)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn http_status(code: &str) -> u16 {
    match code {
        "upload_not_found" => 404,
        "upload_conflict" => 409,
        "upload_expired" | "invalid_request" => 400,
        _ => 400,
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Bad Request",
    }
}

async fn read_request(
    socket: &mut tokio::net::TcpStream,
) -> Result<(String, String, String, Vec<u8>), String> {
    let mut buf = Vec::new();
    let header_end = loop {
        if buf.len() > MAX_HEADER_BYTES {
            return Err("headers are too large".into());
        }
        let mut tmp = [0u8; 1024];
        let n = tokio::time::timeout(HEADER_TIMEOUT, socket.read(&mut tmp))
            .await
            .map_err(|_| "timed out reading headers".to_owned())?
            .map_err(|error| error.to_string())?;
        if n == 0 {
            return Err("connection closed before headers".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
    };
    let header = std::str::from_utf8(&buf[..header_end]).map_err(|_| "headers are not utf-8")?;
    let mut lines = header.split("\r\n");
    let request = lines.next().unwrap_or("");
    let mut parts = request.split(' ');
    let method = parts.next().unwrap_or("").to_owned();
    let path = parts.next().unwrap_or("").to_owned();
    let mut content_length = None;
    let mut token = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "content-length is not a number".to_owned())?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("chunked upload is not accepted".into());
        } else if name.eq_ignore_ascii_case("authorization") {
            token = value
                .trim()
                .strip_prefix(UPLOAD_AUTHORIZATION_SCHEME)
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(str::to_owned);
        }
    }
    let Some(token) = token else {
        return Err("missing upload authorization".into());
    };
    let Some(content_length) = content_length else {
        return Err("content-length is required".into());
    };
    if content_length > crate::MAX_IMAGE_BYTES {
        return Err("image is larger than 8 MiB".into());
    }
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut tmp = [0u8; 8192];
        let n = tokio::time::timeout(BODY_TIMEOUT, socket.read(&mut tmp))
            .await
            .map_err(|_| "timed out reading the image".to_owned())?
            .map_err(|error| error.to_string())?;
        if n == 0 {
            return Err("connection closed before the image arrived".into());
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    Ok((method, path, token, body))
}

/// `Authorization` value the tool result tells the uploader to send.
pub fn authorization_header(token: &str) -> String {
    format!("{UPLOAD_AUTHORIZATION_SCHEME} {token}")
}

/// Encode a grant the way `create_image_upload` answers. `origin` is the
/// scheme and host the uploader can reach, with no path.
pub fn grant_json(grant: &ImageUploadGrant, origin: &str) -> serde_json::Value {
    serde_json::json!({
        "upload_id": grant.upload_id,
        "expires_at": grant.expires_at_ms,
        "upload": {
            "method": "PUT",
            "url": upload_url(origin, &grant.upload_id),
            "headers": { "authorization": authorization_header(&grant.token) },
        },
    })
}

/// Test helper: standard base64 of a PNG, only for tests that must show the
/// tool schema does not ask for it. Not a transport.
#[cfg(test)]
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::new(width, height)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("png");
    bytes
}

#[cfg(test)]
struct MapStore;

#[cfg(test)]
impl ImageBlobStore for MapStore {
    fn put_image(
        &self,
        _workspace_id: &str,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<Blob, ToolError> {
        Ok(content_addressed_blob(&bytes, mime, filename))
    }
}

#[cfg(test)]
fn scope(workspace: &str) -> UploadScope {
    UploadScope {
        workspace_id: workspace.into(),
        tenant: "tenant-a".into(),
        principal: "principal-a".into(),
    }
}

#[cfg(test)]
fn story() -> (WorkspaceDoc, NodeId) {
    let mut doc = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "test").unwrap();
    let root = doc
        .insert(Parent::Root, Node::document("Story").unwrap())
        .unwrap();
    (doc, root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_grant_allows_only_one_in_flight_put() {
        use std::sync::Barrier;
        struct SlowStore(Arc<Barrier>, Arc<Barrier>);
        impl ImageBlobStore for SlowStore {
            fn put_image(
                &self,
                _: &str,
                bytes: Vec<u8>,
                mime: &str,
                filename: Option<&str>,
            ) -> Result<Blob, ToolError> {
                self.0.wait();
                self.1.wait();
                Ok(content_addressed_blob(&bytes, mime, filename))
            }
        }
        let uploads = ImageUploads::new();
        let grant = uploads.create(scope("ws-1"), None, 1).unwrap();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let store = SlowStore(Arc::clone(&entered), Arc::clone(&release));
        let started = uploads.clone();
        let id = grant.upload_id.clone();
        let token = grant.token.clone();
        let bytes = png(1, 1);
        let pending = bytes.clone();
        let worker = std::thread::spawn(move || started.complete(&id, &token, &pending, 1, &store));
        entered.wait();
        assert_eq!(
            uploads
                .complete(&grant.upload_id, &grant.token, &bytes, 1, &MapStore)
                .unwrap_err()
                .code,
            "image_upload_busy"
        );
        release.wait();
        assert!(worker.join().unwrap().is_ok());
        assert!(
            uploads
                .verified(&scope("ws-1"), &grant.upload_id, 1)
                .is_ok()
        );
    }

    #[test]
    fn pending_uploads_have_a_bound_and_expired_grants_free_capacity() {
        let uploads = ImageUploads::new();
        for _ in 0..MAX_LIVE_UPLOADS {
            uploads.create(scope("ws-1"), None, 1).unwrap();
        }
        assert_eq!(
            uploads.create(scope("ws-1"), None, 1).unwrap_err().code,
            "image_upload_busy"
        );
        uploads
            .create(scope("ws-1"), None, 1 + IMAGE_UPLOAD_TTL_MS)
            .unwrap();
        assert_eq!(uploads.lock().len(), 1);
    }

    #[test]
    fn a_completed_upload_verifies_and_a_repeat_of_the_same_bytes_does_not_conflict() {
        let uploads = ImageUploads::new();
        let now = 1_000;
        let grant = uploads
            .create(scope("ws-1"), Some("library.png".into()), now)
            .unwrap();
        let bytes = png(2, 3);
        let first = uploads
            .complete(&grant.upload_id, &grant.token, &bytes, now, &MapStore)
            .unwrap();
        assert_eq!(first.mime, "image/png");
        assert_eq!(
            (first.width, first.height, first.size),
            (2, 3, bytes.len() as u64)
        );
        assert!(first.hash.starts_with("b3:"));
        let again = uploads
            .complete(&grant.upload_id, &grant.token, &bytes, now, &MapStore)
            .unwrap();
        assert_eq!(again.hash, first.hash);
        let verified = uploads
            .verified(&scope("ws-1"), &grant.upload_id, now)
            .unwrap();
        assert_eq!(verified, first);
    }

    #[test]
    fn an_uncompleted_expired_or_foreign_upload_cannot_be_placed() {
        let uploads = ImageUploads::new();
        let now = 5_000;
        let grant = uploads.create(scope("ws-1"), None, now).unwrap();
        let pending = uploads
            .verified(&scope("ws-1"), &grant.upload_id, now)
            .unwrap_err();
        assert_eq!(pending.code, "upload_not_complete");

        let expired = uploads
            .verified(&scope("ws-1"), &grant.upload_id, now + IMAGE_UPLOAD_TTL_MS)
            .unwrap_err();
        assert_eq!(expired.code, "upload_expired");

        let other_workspace = uploads
            .verified(&scope("ws-2"), &grant.upload_id, now)
            .unwrap_err();
        assert_eq!(other_workspace.code, "upload_workspace_mismatch");

        let mut other = scope("ws-1");
        other.principal = "principal-b".into();
        let other_principal = uploads.verified(&other, &grant.upload_id, now).unwrap_err();
        assert_eq!(other_principal.code, "permission_denied");

        assert_eq!(
            uploads
                .verified(&scope("ws-1"), "missing", now)
                .unwrap_err()
                .code,
            "upload_not_found"
        );
    }

    #[test]
    fn invalid_or_oversized_bytes_are_refused_and_leave_the_upload_incomplete() {
        let uploads = ImageUploads::new();
        let now = 1;
        let grant = uploads.create(scope("ws-1"), None, now).unwrap();
        let svg = uploads
            .complete(
                &grant.upload_id,
                &grant.token,
                b"<svg xmlns='http://www.w3.org/2000/svg'/>",
                now,
                &MapStore,
            )
            .unwrap_err();
        assert_eq!(svg.code, "invalid_request");
        assert_eq!(
            uploads
                .verified(&scope("ws-1"), &grant.upload_id, now)
                .unwrap_err()
                .code,
            "upload_not_complete"
        );

        let big = vec![0u8; crate::MAX_IMAGE_BYTES + 1];
        let oversized = uploads
            .complete(&grant.upload_id, &grant.token, &big, now, &MapStore)
            .unwrap_err();
        assert!(oversized.message.contains("at most"));
    }

    #[test]
    fn different_bytes_after_success_conflict_and_a_bad_token_is_refused() {
        let uploads = ImageUploads::new();
        let now = 1;
        let grant = uploads.create(scope("ws-1"), None, now).unwrap();
        uploads
            .complete(&grant.upload_id, &grant.token, &png(1, 1), now, &MapStore)
            .unwrap();
        let conflict = uploads
            .complete(&grant.upload_id, &grant.token, &png(4, 4), now, &MapStore)
            .unwrap_err();
        assert_eq!(conflict.code, "upload_conflict");
        let bad = uploads
            .complete(
                &grant.upload_id,
                "not-the-token",
                &png(1, 1),
                now,
                &MapStore,
            )
            .unwrap_err();
        assert_eq!(bad.code, "invalid_request");
    }

    #[test]
    fn insert_places_the_image_and_a_retry_does_not_add_another() {
        let (mut doc, root) = story();
        let first = doc
            .insert(
                Parent::Node(root),
                Node::new(Kind::Paragraph, Content::text("Before")).unwrap(),
            )
            .unwrap();
        let base = revision(&doc);
        let image = sample();
        let node_id = NodeId::new().to_storage();
        assert_eq!(
            decide_insert(&doc, &base, &node_id, &image, Some("Library")).unwrap(),
            PlaceDecision::Write
        );
        write_insert(
            &mut doc,
            &node_id,
            &root.to_storage(),
            Some(&first.to_storage()),
            &image,
            Some("Library"),
        )
        .unwrap();
        let stale = base.clone();
        assert_eq!(
            decide_insert(&doc, &stale, &node_id, &image, None).unwrap(),
            PlaceDecision::Replay
        );
        let order = doc.children(Parent::Node(root)).unwrap();
        assert_eq!(order.len(), 2, "retry did not insert a second image");
        let read = doc.read(node(&node_id).unwrap()).unwrap();
        assert_eq!(read.kind, Kind::Image);
        assert_eq!(read.meta.name.as_deref(), Some("Library"));
        let projected = crate::node_json(&read, Parent::Node(root), 1, 1, &[]);
        assert_eq!(projected["content"]["type"], "blob");
        assert_eq!(projected["content"]["hash"], image.hash);
        assert_eq!(projected["content"]["mime"], "image/png");
        assert_eq!(projected["content"]["width"], 2);
        assert_eq!(projected["content"]["height"], 3);
        assert_eq!(projected["metadata"]["name"], "Library");
    }

    #[test]
    fn a_stale_revision_and_a_duplicate_id_do_not_mutate() {
        let (mut doc, root) = story();
        let (foreign, _) = story();
        let base = revision(&foreign);
        let image = sample();
        let node_id = NodeId::new().to_storage();
        let stale = decide_insert(&doc, &base, &node_id, &image, None).unwrap_err();
        assert_eq!(stale.code, "revision_conflict");
        assert!(doc.children(Parent::Node(root)).unwrap().is_empty());

        let fresh = revision(&doc);
        write_insert(&mut doc, &node_id, &root.to_storage(), None, &image, None).unwrap();
        let other = sample_sized(5, 5);
        let duplicate = decide_insert(&doc, &fresh, &node_id, &other, None).unwrap_err();
        assert_eq!(duplicate.code, "duplicate_node_id");
        assert_eq!(doc.children(Parent::Node(root)).unwrap().len(), 1);
        let kept = doc.read(node(&node_id).unwrap()).unwrap();
        let Content::Blob { width, height, .. } = kept.content else {
            panic!("not a blob");
        };
        assert_eq!((width, height), (Some(2), Some(3)));
    }

    #[test]
    fn replace_updates_one_image_and_a_lost_response_does_not_insert() {
        let (mut doc, root) = story();
        let image = sample();
        let node_id = NodeId::new().to_storage();
        write_insert(
            &mut doc,
            &node_id,
            &root.to_storage(),
            None,
            &image,
            Some("Old"),
        )
        .unwrap();
        let base = revision(&doc);
        let next = sample_sized(8, 9);
        assert_eq!(
            decide_replace(&doc, &base, &node_id, &next, Some("New")).unwrap(),
            PlaceDecision::Write
        );
        write_replace(&mut doc, &node_id, &next, Some("New")).unwrap();
        assert_eq!(doc.children(Parent::Node(root)).unwrap().len(), 1);
        let read = doc.read(node(&node_id).unwrap()).unwrap();
        assert_eq!(read.meta.name.as_deref(), Some("New"));
        let Content::Blob {
            hash,
            width,
            height,
            ..
        } = &read.content
        else {
            panic!("not a blob");
        };
        assert_eq!(hash.0, next.hash);
        assert_eq!((*width, *height), (Some(8), Some(9)));

        let stale = base;
        assert_eq!(
            decide_replace(&doc, &stale, &node_id, &next, Some("New")).unwrap(),
            PlaceDecision::Replay
        );
        assert_eq!(doc.children(Parent::Node(root)).unwrap().len(), 1);

        let (foreign, _) = story();
        let moved = decide_replace(
            &doc,
            &revision(&foreign),
            &node_id,
            &sample(),
            Some("Other"),
        )
        .unwrap_err();
        assert_eq!(moved.code, "revision_conflict");
    }

    #[test]
    fn replace_refuses_a_paragraph_and_an_oversized_caption_writes_nothing() {
        let (mut doc, root) = story();
        let paragraph = doc
            .insert(
                Parent::Node(root),
                Node::new(Kind::Paragraph, Content::text("placeholder")).unwrap(),
            )
            .unwrap();
        let base = revision(&doc);
        let refused =
            decide_replace(&doc, &base, &paragraph.to_storage(), &sample(), None).unwrap_err();
        assert_eq!(refused.code, "invalid_request");
        assert_eq!(
            doc.read(paragraph).unwrap().content.as_text(),
            Some("placeholder")
        );
        let bad = decide_insert(
            &doc,
            &base,
            &NodeId::new().to_storage(),
            &sample(),
            Some(&"x".repeat(MAX_CAPTION_CHARS + 1)),
        )
        .unwrap_err();
        assert_eq!(bad.code, "invalid_request");
    }

    #[test]
    fn the_loopback_put_completes_an_upload_without_base64() {
        let uploads = ImageUploads::new();
        let grant = uploads
            .create(scope("ws-1"), None, Timestamp::now().millis())
            .unwrap();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let serving = uploads.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = bind_loopback().unwrap();
                let addr = spawn_upload_server(listener, serving, Arc::new(MapStore)).unwrap();
                tokio::task::yield_now().await;
                addr_tx.send(addr).unwrap();
                std::future::pending::<()>().await;
            });
        });
        let addr = addr_rx.recv().unwrap();
        let bytes = png(2, 2);
        let body = format!(
            "PUT {UPLOAD_PATH_PREFIX}{} HTTP/1.1\r\nhost: 127.0.0.1\r\nauthorization: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            grant.upload_id,
            authorization_header(&grant.token),
            bytes.len()
        );
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        std::io::Write::write_all(&mut stream, body.as_bytes()).unwrap();
        std::io::Write::write_all(&mut stream, &bytes).unwrap();
        let mut response = String::new();
        std::io::Read::read_to_string(&mut stream, &mut response).unwrap();
        assert!(response.contains("200"), "{response}");
        assert!(!response.contains("base64"));
        let verified = uploads
            .verified(&scope("ws-1"), &grant.upload_id, Timestamp::now().millis())
            .unwrap();
        assert_eq!(verified.mime, "image/png");
        assert!(!bytes.is_empty());
    }

    fn sample() -> VerifiedImage {
        sample_sized(2, 3)
    }

    fn sample_sized(width: u32, height: u32) -> VerifiedImage {
        let bytes = png(width, height);
        let blob = content_addressed_blob(&bytes, "image/png", None);
        VerifiedImage {
            upload_id: "upload".into(),
            hash: blob.hash.as_str().to_owned(),
            mime: "image/png".into(),
            width,
            height,
            size: blob.size,
            filename: None,
            expires_at_ms: 10,
        }
    }
}
