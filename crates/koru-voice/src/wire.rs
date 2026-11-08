// SPDX-License-Identifier: MPL-2.0

//! **What crosses between a client and the voice worker, and the two numbers
//! that say how to read it.**
//!
//! ```text
//! client -> ClientHello   { product, version, protocol_version }
//! worker -> WorkerGreeting::Welcome { worker_version, protocol_version }
//!         | WorkerGreeting::Refused { what, ours, theirs, message } -> client closes
//! ...then, until the socket closes:
//! client -> Request::Begin | Finish | Cancel | Devices | Meter | MeterStop
//! worker -> Event::State | Partial | Final | Language | Level | Ended | Refused
//!         | Event::Devices                       <- the answer to Devices
//!         | Event::Level                         <- while a Meter runs
//! ```
//!
//! ## A message is a value before it is bytes
//!
//! On the desktop these travel a `koru-ipc` frame. On a phone there is no
//! socket and no worker process at all — the same [`Session`](crate::Session)
//! runs in the app, and a shim consumes the same [`Event`] values directly.
//! That is why the crate charter says it links no socket: these are shapes, and
//! `koru_ipc::frame` is what turns one into bytes.
//!
//! ## Two versions live at two layers, and they are not the same number
//!
//! [`PROTOCOL_VERSION`] is **what these messages mean**. [`FRAME_VERSION`] is
//! **how they are wrapped** — `KORU`, a version byte, a length, a body — and it
//! is handed to `koru_ipc::frame`, which takes its version as a parameter and
//! holds no constant of its own. `koru-agent-wire` carries the same pair for
//! the same reason, and the difference matters at exactly one moment:
//!
//! - an unknown **frame** version means *"this is not something I can read at
//!   all"*, and the only honest answer is to close the connection — a refusal
//!   is itself a frame, and writing one in a format the peer has just said it
//!   does not share is a promise that cannot be kept;
//! - an unknown **protocol** version means *"I can read you, and I do not speak
//!   this"*, which is a sentence: [`WorkerGreeting::Refused`], naming both
//!   numbers, because "incompatible" alone sends somebody to read two
//!   changelogs to find out which end is behind.
//!
//! ## Audio never appears here
//!
//! Not in any variant, and not by omission. The worker opens the microphone
//! itself and sends back transcripts and a coarse [`LevelBucket`]; PCM never
//! crosses this boundary. That is the strongest form of the privacy story and
//! it is also why `koru_ipc::frame`'s 16 MiB cap is never a question here — the
//! largest message this wire can carry is a sentence.

use crate::engine::{ModelId, SessionId};
use crate::error::ErrorCode;
use crate::input::{InputChoice, InputDevice};
use crate::language::{Requested, VoiceLanguage};
use crate::session::{EndReason, State};
use crate::tier::ModelTier;
use crate::transcript::Transcript;
use serde::{Deserialize, Serialize};

/// **What these messages mean.** Bump when a shape changes in a way an older
/// peer cannot read.
///
/// These enums are internally tagged, so an older peer meeting an unknown
/// `kind` fails the whole body rather than skipping a field — which is exactly
/// why the handshake exists and why this number is checked there rather than
/// hoped about later.
///
/// 1 is the first: begin, finish and cancel; state, partial, final, language,
/// level, ended and refused.
///
/// **2 adds asking which microphones there are, and choosing one.**
/// `Request::Devices` / `Event::Devices`, and `Begin.input`. Both additions
/// are `#[serde(default)]` or new variants, so a version-1 *message* still
/// reads — but the number moves anyway, because a version-1 worker meeting a
/// `Devices` request answers nothing at all, and a client waiting for a list
/// that will never come is exactly the silent failure the handshake exists to
/// turn into a sentence.
///
/// **3 carries the person's quality tier on every `Begin`.**
///
/// **4 adds a level meter that recognises nothing.** `Request::Meter` /
/// `Request::MeterStop`, answered by `Event::Level` under the meter's own
/// correlation pair. New variants again, so a version-3 worker would close
/// the connection on the first one rather than answer; the handshake turns
/// that into a refusal, and a client then simply has no meter.
pub const PROTOCOL_VERSION: u32 = 4;

/// The frame version this stream stamps, handed to `koru_ipc::frame`.
///
/// Separate from [`PROTOCOL_VERSION`] on purpose — see the module note. It
/// changes only if the *framing* changes, which is a thing that has never
/// happened in this product and would break every local endpoint at once.
pub const FRAME_VERSION: u8 = 1;

/// A coarse input level, for a meter.
///
/// **A bucket, never a float**, and that is a privacy decision rather than a
/// compression one: a per-frame amplitude is a signal, and a signal is
/// something an attacker with the transcript stream can do more with than a
/// meter needs. Nine steps is all a meter draws, it matches
/// `koru_events::FieldClass::BucketedNumeric`, and it keeps a signal-shaped
/// number off the wire.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(transparent)]
pub struct LevelBucket(u8);

impl LevelBucket {
    /// Silence.
    pub const SILENT: LevelBucket = LevelBucket(0);
    /// The loudest bucket.
    pub const MAX: u8 = 8;

    /// Clamp into `0..=8`.
    ///
    /// **Clamping rather than refusing**, deliberately: a meter must never be a
    /// reason a dictation fails. A host that computes 9 from a clipped sample
    /// gets a full bar and the person keeps talking.
    pub fn new(step: u8) -> LevelBucket {
        LevelBucket(step.min(LevelBucket::MAX))
    }

    pub fn step(self) -> u8 {
        self.0
    }
}

impl<'de> Deserialize<'de> for LevelBucket {
    /// Clamped on the way in as well, for the same reason: a peer that sends 200
    /// has a bug, and dropping the whole session over a meter would be a worse
    /// one.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<LevelBucket, D::Error> {
        u8::deserialize(d).map(LevelBucket::new)
    }
}

/// What a client says first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    /// Which program is calling — `koru`, a bench, a test harness. A label for
    /// the worker's logs, never a person and never a device.
    pub product: String,
    pub version: String,
    pub protocol_version: u32,
}

/// The worker's answer, and the only place two versions are compared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerGreeting {
    Welcome {
        worker_version: String,
        protocol_version: u32,
    },
    /// Both numbers, always. See the module note on why "incompatible" alone is
    /// not an answer.
    Refused {
        what: String,
        ours: u32,
        theirs: u32,
        message: String,
    },
}

/// What a client asks the worker to do.
///
/// Three verbs, because there are three things a person can do to a dictation:
/// start it, end it, and throw it away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Begin one dictation. Refused with [`ErrorCode::Busy`] while another is
    /// live on this installation.
    Begin {
        session: SessionId,
        generation: u64,
        language: Requested,
        /// The quality preference. The worker resolves it to an installed
        /// model; clients do not choose catalog ids in the ordinary path.
        #[serde(default)]
        tier: ModelTier,
        /// **`None` means the worker chooses**, from the language, the
        /// installed catalog and the person's quality preference — which is the
        /// ordinary case, because a window knows about languages and has no
        /// business knowing model ids. A bench or a test pins one.
        #[serde(default)]
        model: Option<ModelId>,
        /// **Which microphone**, by name — see [`crate::input`] on why it is a
        /// name and never a position in a list.
        ///
        /// `#[serde(default)]` is [`InputChoice::SystemDefault`], so a client
        /// built before this field existed asks for the host's own device,
        /// which is what it always got.
        #[serde(default)]
        input: InputChoice,
    },
    /// Stop capturing and produce the final result. The composition stays
    /// provisional until the matching [`Event::Final`] arrives; a key going up
    /// is not a reason to commit the last partial.
    Finish { session: SessionId, generation: u64 },
    /// Throw it away. No final, no text, no trace.
    Cancel { session: SessionId, generation: u64 },
    /// **Which capture devices can this worker see?** Answered with
    /// [`Event::Devices`], and it starts nothing: no model is loaded, no
    /// device is opened, and the session machine is not touched.
    ///
    /// **`session` and `generation` are a correlation id here, not a
    /// dictation.** They are carried because every message on this wire is,
    /// which is what keeps [`Event::session`] and [`Event::generation`] total
    /// and keeps a late answer from an abandoned question attributable. A
    /// client mints them the same way it mints a dictation's and never arms a
    /// session on them — see `koru-voice-client`'s `devices`.
    Devices { session: SessionId, generation: u64 },
    /// **Open this microphone for a level meter, and nothing else.** Answered
    /// with [`Event::Level`] under this request's own `session` and
    /// `generation` for as long as the meter runs: no model is resolved or
    /// loaded, no engine is touched, no session is armed and no audio is kept
    /// — the samples are reduced to a [`LevelBucket`] and dropped. So it works
    /// on an installation with no model at all.
    ///
    /// **One meter per worker, and a dictation wins.** A second `Meter`, from
    /// this client or another, replaces the first, which is how a picker
    /// switches devices. A `Meter` while a dictation runs is refused with
    /// [`ErrorCode::Busy`], and a `Begin` the worker accepts stops the meter
    /// before it opens the device. A device that will not open is
    /// [`Event::Refused`] with the capture's own code.
    ///
    /// Like [`Request::Devices`], `session` and `generation` are a correlation
    /// id, never a dictation.
    Meter {
        session: SessionId,
        generation: u64,
        /// Which microphone, resolved exactly as a dictation's is.
        #[serde(default)]
        input: InputChoice,
    },
    /// Close the meter this `session` opened. Stopping a meter that has
    /// already stopped — replaced, pre-empted by a dictation, or never
    /// opened — answers nothing and changes nothing.
    MeterStop { session: SessionId, generation: u64 },
}

/// What the worker says back.
///
/// Every variant carries the session and its generation, stamped in one place
/// by [`Session`](crate::Session) — see that module's note on why the queue
/// exists at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// The session moved. **Never reports [`State::Idle`]** — the return to
    /// idle is [`Event::Ended`], which carries the reason a bare state cannot.
    /// `a_state_event_never_reports_idle` holds it.
    State {
        session: SessionId,
        generation: u64,
        state: State,
    },
    /// The current best guess, which will be revised. **Never enters Loro**: a
    /// recogniser rewrites itself several times a second, and writing each
    /// revision would generate undo history, sync traffic and visible
    /// collaborative churn for text nobody has finished saying. The same
    /// semantic model as keyboard IME composition.
    Partial {
        session: SessionId,
        generation: u64,
        text: Transcript,
    },
    /// The result. The only text a host may commit, and only when the
    /// [`Event::Ended`] that follows says [`EndReason::Completed`].
    Final {
        session: SessionId,
        generation: u64,
        text: Transcript,
    },
    /// The answer to [`Requested::Auto`]. May arrive more than once; the last
    /// one wins.
    Language {
        session: SessionId,
        generation: u64,
        language: VoiceLanguage,
    },
    /// A coarse input level, for a meter. Sent while a dictation is
    /// listening, and while a [`Request::Meter`] runs — then under the
    /// meter's own correlation pair, and only when the bucket moves.
    Level {
        session: SessionId,
        generation: u64,
        bucket: LevelBucket,
    },
    /// The session is over, and why. Always the last event of a session.
    Ended {
        session: SessionId,
        generation: u64,
        reason: EndReason,
    },
    /// The answer to [`Request::Devices`]: every capture device the worker
    /// can see, in the host's own order, with the one the host would open
    /// flagged.
    ///
    /// **This is the one message on this wire that carries a device name.**
    /// `apps/koru-voice`'s `capture` module keeps names out of every
    /// diagnostic on the grounds that a name is a person's hardware; a picker
    /// cannot be drawn out of categories, so the name travels here and the
    /// rule that survives is the one about writing it down. The list is
    /// bounded by how many microphones are plugged into a machine, so it is
    /// nowhere near `koru_ipc::frame`'s cap.
    Devices {
        session: SessionId,
        generation: u64,
        devices: Vec<InputDevice>,
    },
    /// A request the worker would not act on at all, so no session started and
    /// nothing will end. `code` is branched on; `message` is untranslated
    /// developer text a client must not show a person.
    Refused {
        session: SessionId,
        generation: u64,
        code: ErrorCode,
        message: String,
    },
}

impl Event {
    /// The session this is about.
    pub fn session(&self) -> SessionId {
        match self {
            Event::State { session, .. }
            | Event::Partial { session, .. }
            | Event::Final { session, .. }
            | Event::Language { session, .. }
            | Event::Level { session, .. }
            | Event::Ended { session, .. }
            | Event::Devices { session, .. }
            | Event::Refused { session, .. } => *session,
        }
    }

    /// The generation this is about. A client compares it to its own and drops
    /// what does not match, which is the same rule the worker applies on the
    /// way out — belt and braces across a socket that can reorder nothing but
    /// can certainly deliver late.
    pub fn generation(&self) -> u64 {
        match self {
            Event::State { generation, .. }
            | Event::Partial { generation, .. }
            | Event::Final { generation, .. }
            | Event::Language { generation, .. }
            | Event::Level { generation, .. }
            | Event::Ended { generation, .. }
            | Event::Devices { generation, .. }
            | Event::Refused { generation, .. } => *generation,
        }
    }
}

#[cfg(test)]
mod tests;
