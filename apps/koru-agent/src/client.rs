// SPDX-License-Identifier: MPL-2.0

//! Talking to a *running* agent, from the command line.
//!
//! `status`, `open` and `stop` are clients of an agent, not modes of one. None
//! reads a pid file or a port: you cannot connect to a pipe nobody is listening
//! on, so liveness answers itself and there is nothing on disk to go stale
//! ([ADR-84](../../../docs/decisions/ADR-076-100.md#adr-84-rendezvous--a-local-endpoint-is-how-a-store-is-found-and-the-only-long-lived-credential-is-gone)).
//!
//! **Nothing here starts an agent.** `status` on a machine with none is an
//! answer — "not running" — and starting one to be able to say so would make
//! the question change the world it is asking about. `stop` is the same in
//! reverse.
//!
//! ## This is the third handshake in the tree, and the second client half
//!
//! `apps/koru-agent/src/rendezvous.rs` has the server side, and
//! `apps/koru/src/agent.rs` has a client side of its own. They cannot share
//! one: the shell may not link `koru-agent`, which is a windowed application
//! carrying winit, vello and the search surface, and the piece that would move
//! down into `koru-agent-wire` needs `koru-ipc`, which would stop that crate
//! being the serde-only leaf both sides depend on. Recorded in
//! [TECHDEBT](../../../docs/contributing/techdebt/README.md) rather than solved by a crate that
//! exists only to hold it.

use std::io::Write;

use koru_agent_wire::{
    AgentGreeting, FRAME_VERSION, FromAgent, FromShell, PROTOCOL_VERSION, Request, Response,
    ShellHello, UpdateChoice,
};

use crate::Paths;

/// How long to wait for an agent to answer. Generous for a handshake and short
/// enough that a wedged agent does not hold a terminal.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Print what a running agent is and what it is holding.
pub fn status(paths: &Paths) -> Result<(), String> {
    let Some(stream) = dial(paths)? else {
        println!("koru-agent is not running");
        println!("{}", gateway_status(paths));
        println!("{}", delegate_status(paths));
        return Ok(());
    };
    match ask(&stream, Request::Status)? {
        Response::Status {
            agent_version,
            device_id,
            instance_id,
            shells,
            pending_launches,
            owns_the_machine,
            sync,
            reminders,
        } => {
            println!("running, version {agent_version}");
            println!("  device:    {device_id}");
            println!("  instance:  {instance_id}");
            println!(
                "  sync:      {}",
                if sync.sync_ready {
                    format!(
                        "ready, endpoint {}, {} trusted device(s), authorization generation {}",
                        sync.endpoint_id.as_deref().unwrap_or("unavailable"),
                        sync.trusted_device_count,
                        sync.authorization_generation
                    )
                } else {
                    format!(
                        "unavailable ({})",
                        sync.sync_error.as_deref().unwrap_or("not_initialized")
                    )
                }
            );
            // **Counts, not contents.** Which workspace a window has open is a
            // person's, and a status line gets pasted into bug reports.
            println!(
                "  shells:    {shells} connected, {pending_launches} target(s) waiting for a window"
            );
            println!(
                "  shortcut:  {}",
                if owns_the_machine {
                    "held by this agent"
                } else {
                    "not held - this launch did not opt into global input"
                }
            );
            println!(
                "  reminders: {}; {} pending, {} registered, {} failed; cursor {}; next deadline {}; last successful sync {}; failure {}",
                if reminders.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                reminders.pending,
                reminders.registered,
                reminders.failed,
                reminders
                    .cursor
                    .map_or_else(|| "none".into(), |value| value.to_string()),
                reminders
                    .next_deadline_ms
                    .map_or_else(|| "none".into(), |value| value.to_string()),
                reminders
                    .last_successful_sync_ms
                    .map_or_else(|| "none".into(), |value| value.to_string()),
                reminders.failure_category.as_deref().unwrap_or("none")
            );
            println!("{}", gateway_status(paths));
            println!("{}", delegate_status(paths));
            println!("{}", routing_log_status(paths));
            Ok(())
        }
        Response::Failed { code, message } => Err(format!("{code}: {message}")),
        Response::Pairing { .. } => Err("the agent answered status with pairing state".into()),
        Response::LinkedDevices { .. } => {
            Err("the agent answered status with linked devices".into())
        }
        Response::DeviceHistory { .. } => {
            Err("the agent answered status with device history".into())
        }
        Response::Account { .. } => Err("the agent answered status with account state".into()),
        Response::AccountQueued { .. } => {
            Err("the agent answered status with an account operation".into())
        }
        Response::LocalProfileRemoved { .. } => {
            Err("the agent answered status with a local Profile operation".into())
        }
        Response::LocalProfileAccountBound { .. } => {
            Err("the agent answered status with a Profile account operation".into())
        }
        Response::FirstPartyGrant { .. } | Response::FirstPartyGrantValidity { .. } => {
            Err("the agent answered status with a first-party grant".into())
        }
        Response::Ok => Err("the agent answered a status question with `ok`".into()),
        Response::NotificationControl { .. } => {
            Err("the agent answered status with notification control state".into())
        }
        Response::ReminderStatus { .. } => {
            Err("the agent answered status with reminder state only".into())
        }
        Response::WorkspaceOpened { .. } => {
            Err("the agent answered status with a workspace launch".into())
        }
    }
}

/// Keep the routing log findable without putting the resolved channel root in
/// a status transcript that will commonly be pasted into a bug report.
fn routing_log_status(paths: &Paths) -> String {
    format!("  route log: {}", paths.routing_log_label())
}

/// Whether the public API is being served on this root.
///
/// **Probed, not asked**, and that is the whole reason this line exists here
/// rather than as a field of the agent's `Status` answer. A diagnostic must not
/// depend on the health of what it diagnoses: an Agent whose supervision has
/// wedged would answer "gateway: ready" from a cached record, and an Agent that
/// is not running at all would answer nothing — which is exactly when somebody
/// wants to know whether a gateway is still sitting on the rendezvous. So this
/// dials the gateway's own root-qualified rendezvous, the way any client would,
/// and it is printed whether or not an agent answered above.
///
/// `probe_at` sends no hello, so a status line cannot make a gateway mint a
/// credential that is thrown away one line later.
///
/// This replaced `koru-api-gateway status`, which was deleted with the
/// standalone executable. It is deliberately **not** a new verb: the gateway's
/// three one-shot commands became no Agent command, and this is one line of an
/// answer that already existed.
fn gateway_status(paths: &Paths) -> String {
    match koru_api_discovery::probe_at(crate::segment(), paths.root()) {
        Ok(()) => "  gateway:   serving".into(),
        Err(koru_api_discovery::DiscoveryError::NotRunning(_)) => "  gateway:   not serving".into(),
        Err(error) => format!("  gateway:   unreachable ({error})"),
    }
}

/// Whether the delegate worker answers on this root — probed, for the
/// reason [`gateway_status`] gives: a diagnostic must not depend on the
/// health of what it diagnoses.
fn delegate_status(paths: &Paths) -> String {
    match crate::delegate_endpoint(paths) {
        Ok(endpoint) if koru_ipc::probe(&endpoint) => "  delegate:  serving".into(),
        Ok(_) => "  delegate:  not serving".into(),
        Err(error) => format!("  delegate:  unreachable ({error})"),
    }
}

/// Ask the running Agent to put a Koru workspace window in front.
///
/// The Agent performs the launch rather than this short-lived CLI process so
/// the child is recorded as first-party before it asks for lifecycle scopes.
pub fn open_workspace(
    paths: &Paths,
    profile: Option<&str>,
    inherit_stdio: bool,
) -> Result<(), String> {
    let Some(stream) = dial(paths)? else {
        return Err("koru-agent is not running".into());
    };
    match ask(
        &stream,
        Request::OpenWorkspace {
            profile: profile.map(str::to_owned),
            inherit_stdio,
        },
    )? {
        Response::WorkspaceOpened { pid: Some(pid) } => {
            println!("opened Koru, pid {pid}");
            Ok(())
        }
        Response::WorkspaceOpened { pid: None } => {
            println!("restored existing Koru window");
            Ok(())
        }
        Response::Failed { code, message } => Err(format!("{code}: {message}")),
        _ => Err("the agent answered open with an unrelated response".into()),
    }
}

/// Ask Agent to mint the invitation and render it without exposing the device
/// signing or endpoint keys to this short-lived client process.
pub fn pair(paths: &Paths) -> Result<(), String> {
    let Some(stream) = dial(paths)? else {
        return Err("koru-agent is not running".into());
    };
    match ask(&stream, Request::StartPairing)? {
        Response::Pairing { view } => {
            let payload = view
                .qr_payload
                .as_deref()
                .ok_or_else(|| format!("pairing is {:?}", view.state))?;
            let code = qrcode::QrCode::new(payload.as_bytes())
                .map_err(|error| format!("could not render the pairing QR: {error}"))?;
            let image = code
                .render::<qrcode::render::unicode::Dense1x2>()
                .dark_color(qrcode::render::unicode::Dense1x2::Light)
                .light_color(qrcode::render::unicode::Dense1x2::Dark)
                .build();
            println!(
                "Scan with Koru on the other device. Expires in {} seconds.",
                view.expires_in_seconds
            );
            println!("{image}");
            Ok(())
        }
        Response::Failed { code, message } => Err(format!("{code}: {message}")),
        _ => Err("the agent answered pairing with an unrelated response".into()),
    }
}

pub fn cancel_pairing(paths: &Paths) -> Result<(), String> {
    let Some(stream) = dial(paths)? else {
        return Err("koru-agent is not running".into());
    };
    match ask(&stream, Request::CancelPairing)? {
        Response::Pairing { .. } | Response::Ok => {
            println!("pairing invitation cancelled");
            Ok(())
        }
        Response::Failed { code, message } => Err(format!("{code}: {message}")),
        _ => Err("the agent answered cancellation with an unrelated response".into()),
    }
}

/// Ask a running agent to stop.
pub fn stop(paths: &Paths) -> Result<(), String> {
    let Some(stream) = dial(paths)? else {
        println!("koru-agent is not running");
        return Ok(());
    };
    // **Sent, then the socket is dropped. There is nothing to wait for.** An
    // agent that acknowledged a shutdown would have to still be running to send
    // the acknowledgement, so the reply would be about a moment that had
    // already passed. The next `status` is the honest way to check.
    send(&stream, Request::Shutdown)?;
    println!("asked koru-agent to stop");
    Ok(())
}

/// Accept one of the two consent steps in a local update rehearsal.
///
/// This is deliberately a client action rather than an Agent bypass. The
/// running packaged Agent still validates the current stage and drives exactly
/// the same download, verification, handoff, and updater transaction as an
/// in-window button. `main` confines the verbs that reach here to an explicit
/// root, so an unattended gate cannot consent to an installed-channel update.
pub fn accept_rehearsal(paths: &Paths, choice: UpdateChoice) -> Result<(), String> {
    let Some(stream) = dial(paths)? else {
        return Err("the rehearsal Agent is not running".into());
    };
    match ask(&stream, Request::UpdateChoice { choice })? {
        Response::Ok => {
            println!("accepted the {choice:?} step of the rooted update rehearsal");
            Ok(())
        }
        Response::Failed { code, message } => Err(format!("{code}: {message}")),
        _ => Err("the agent answered update consent with an unrelated response".into()),
    }
}

/// Hand a `koru://` link to the running agent.
///
/// `Ok(false)` means there was nobody to hand it to, which is not a failure --
/// it is the cold-start case, and the caller becomes the agent instead of
/// spawning a second one and racing it.
pub fn open_link(paths: &Paths, link: koru_link::KoruLink) -> Result<bool, String> {
    let Some(stream) = dial(paths)? else {
        return Ok(false);
    };
    let request = match link {
        koru_link::KoruLink::Document { document, node } => Request::OpenLink {
            document: document.to_string(),
            node: node.map(|node| node.to_string()),
        },
        koru_link::KoruLink::Chat { chat, block } => Request::OpenChatLink {
            chat: chat.to_string(),
            block: block.map(|block| block.to_string()),
            profile: None,
        },
        // A bare node names no document, so there is nothing the catalog could
        // place. Refused here rather than sent for the agent to refuse.
        koru_link::KoruLink::Node(_)
        | koru_link::KoruLink::Skill { .. }
        | koru_link::KoruLink::Command { .. } => {
            return Err("a Koru link must name a document or chat".to_owned());
        }
    };
    send(&stream, request)?;
    Ok(true)
}

/// Forward a native gesture to the existing singleton without exposing the
/// ledger-resolved Store target on this process's command line.
pub fn notification_activation(
    paths: &Paths,
    activation: &koru_native_notifications::Activation,
) -> Result<bool, ActivationForwardError> {
    let Some(stream) = dial(paths).map_err(ActivationForwardError::Other)? else {
        return Ok(false);
    };
    match ask(
        &stream,
        Request::ReminderActivation {
            envelope: activation.encode_opaque(),
        },
    )
    .map_err(ActivationForwardError::Other)?
    {
        Response::Ok => Ok(true),
        Response::Failed { code, .. } if code == "notification_activation_busy" => {
            Err(ActivationForwardError::Busy)
        }
        Response::Failed { code, message } => {
            Err(ActivationForwardError::Other(format!("{code}: {message}")))
        }
        _ => Err(ActivationForwardError::Other(
            "the agent answered notification activation with an unrelated response".into(),
        )),
    }
}

#[derive(Debug)]
pub enum ActivationForwardError {
    Busy,
    Other(String),
}

/// Connect, or answer that there is nobody there.
///
/// `Ok(None)` is "not running" and `Err` is "running, and something is wrong" —
/// two different answers that a single `Result` would flatten into one.
fn dial(paths: &Paths) -> Result<Option<koru_ipc::Stream>, String> {
    let endpoint = crate::endpoint(paths).map_err(|e| e.to_string())?;
    let Ok(stream) = koru_ipc::connect(&endpoint) else {
        return Ok(None);
    };
    koru_ipc::set_timeouts(&stream, Some(TIMEOUT));
    handshake(&stream)?;
    Ok(Some(stream))
}

fn handshake(stream: &koru_ipc::Stream) -> Result<(), String> {
    let mut write = stream;
    let hello = ShellHello {
        // **What is calling, never who.** A label for the agent's log.
        product: "koru-agent-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
    };
    koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &hello)
        .and_then(|()| write.flush())
        .map_err(|e| format!("could not greet the agent: {e}"))?;

    let mut read = stream;
    let (version, body) =
        koru_ipc::frame::read_raw(&mut read).map_err(|e| format!("the agent said nothing: {e}"))?;
    if version != FRAME_VERSION {
        // Closed without a reply: writing a refusal in a format the peer has
        // just said it does not share is a promise we cannot keep.
        return Err(format!(
            "the agent frames its messages as version {version}; this build reads {FRAME_VERSION}"
        ));
    }
    match koru_ipc::frame::decode::<AgentGreeting>(&body)
        .map_err(|e| format!("the agent's greeting could not be read: {e}"))?
    {
        AgentGreeting::Welcome(_) => Ok(()),
        AgentGreeting::Refused {
            what, ours, theirs, ..
        } => Err(format!(
            "the agent refused this build: {what} - it speaks {ours}, this speaks {theirs}"
        )),
    }
}

fn send(stream: &koru_ipc::Stream, body: Request) -> Result<(), String> {
    let mut write = stream;
    koru_ipc::frame::write_frame(
        &mut write,
        FRAME_VERSION,
        &FromShell::Request { id: 1, body },
    )
    .and_then(|()| write.flush())
    .map_err(|e| format!("could not ask the agent: {e}"))
}

/// Send one request and read until its answer arrives.
///
/// **Pushes are skipped rather than treated as the answer.** The agent may say
/// something unasked-for at any moment — that is what the correlation id is
/// for — and a client that took the first frame back would print a `Restore` as
/// though it were a status.
fn ask(stream: &koru_ipc::Stream, body: Request) -> Result<Response, String> {
    send(stream, body)?;
    let mut read = stream;
    // Bounded: a stream that only ever pushes would otherwise hold the terminal
    // until the timeout, once per frame, forever.
    for _ in 0..16 {
        let (version, raw) = koru_ipc::frame::read_raw(&mut read)
            .map_err(|e| format!("the agent stopped answering: {e}"))?;
        if version != FRAME_VERSION {
            return Err(format!("the agent framed a reply as version {version}"));
        }
        match koru_ipc::frame::decode::<FromAgent>(&raw)
            .map_err(|e| format!("the agent's answer could not be read: {e}"))?
        {
            FromAgent::Response { id: 1, body } => return Ok(body),
            FromAgent::Response { .. } | FromAgent::Push { .. } => continue,
        }
    }
    Err("the agent never answered the question it was asked".into())
}

#[cfg(test)]
mod tests;
