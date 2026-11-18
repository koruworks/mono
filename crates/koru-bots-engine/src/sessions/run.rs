// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::AgentTurnId;
use koru_session_runtime::providers::{AgentControl, AgentInput, AgentStart, ManagedProvider};
use std::{sync::atomic::Ordering, time::Duration};

pub(super) fn run(
    start: Start,
    database: Arc<Mutex<BotDatabase>>,
    stop: Arc<AtomicBool>,
    commands: mpsc::Receiver<Control>,
) -> Result<(), Failure> {
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let context = start.port.context(start.thread)?;
    let context_hash = *blake3::hash(&encode(&context)?).as_bytes();
    let previous = database
        .lock()
        .map_err(|_| Error::StorageUnavailable)?
        .latest(start.thread, start.request.provider.as_str())?;
    // Saved provider state is an optional optimization. Invalid or obsolete
    // state rebuilds from engine history; database/context read failures above
    // still fail closed rather than pretending that current content is empty.
    let previous = previous
        .and_then(|row| serde_json::from_slice::<Saved>(&row.resume_state).ok())
        .filter(|saved| saved.version == 1 && saved.inputs.len() <= MAX_SAVED_INPUTS);
    let token = previous.as_ref().and_then(|saved| saved.token.clone());
    let (mut provider, session, resumed) = match launch(&start, token.clone()) {
        Ok((provider, session)) => (provider, session, token.is_some()),
        Err(Failure::Provider(_)) if token.is_some() && !stop.load(Ordering::Acquire) => {
            // launch confirmed the failed process ended. New session identity
            // requires a newly minted credential; no second input was sent.
            let (provider, session) = launch(&start, None)?;
            (provider, session, false)
        }
        Err(error) => return Err(error),
    };
    let mut claimed = None;
    let result = drive(
        &start,
        &database,
        &stop,
        &commands,
        provider.control.as_mut(),
        session,
        resumed,
        previous,
        context,
        context_hash,
        &mut claimed,
    );
    // A slot is released only after this succeeds, even when drive failed.
    if provider.control.shutdown().is_err() {
        return Err(Failure::RetirementUnconfirmed);
    }
    if let Some(turn) = claimed {
        start.port.retired(turn)?;
    }
    result
}

fn launch(
    start: &Start,
    token: Option<koru_session_protocol::ProviderResumeToken>,
) -> Result<(ManagedProvider, SessionId), Failure> {
    let mut request = start.request.clone();
    let grant = start.port.credential(start.thread, Uuid::new_v4())?;
    let session = grant.session;
    let mut delegation = grant.launch;
    if delegation.credential.is_none() {
        return Err(Error::Unauthenticated.into());
    }
    // Only the adapter's saved token may resume; the credential port has no
    // authority to substitute another thread's provider history.
    delegation.resume = token.clone();
    request.delegation = Some(delegation);
    let mut provider = start.factory.create(request)?;
    match provider.control.start(AgentStart {
        session_id: session,
        parent_session_id: None,
        resume: token,
    }) {
        Ok(_) => Ok((provider, session)),
        Err(error) => {
            if provider.control.shutdown().is_err() {
                return Err(Failure::RetirementUnconfirmed);
            }
            Err(error.into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn drive(
    start: &Start,
    database: &Mutex<BotDatabase>,
    stop: &AtomicBool,
    commands: &mpsc::Receiver<Control>,
    control: &mut dyn AgentControl,
    session: SessionId,
    resumed: bool,
    previous: Option<Saved>,
    context: Context,
    context_hash: [u8; 32],
    claimed: &mut Option<Turn>,
) -> Result<(), Failure> {
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let mut saved = if resumed {
        previous.expect("resume requires saved state")
    } else {
        Saved {
            version: 1,
            token: None,
            context_hash,
            context_delivery: None,
            inputs: vec![],
        }
    };
    let resume = if resumed {
        Resume::Resumed {
            found_deliveries: saved
                .inputs
                .iter()
                .filter(|proof| control.replayed_input_hashes().contains(&proof.hash))
                .map(|proof| proof.delivery)
                .collect(),
        }
    } else {
        Resume::Rebuilt
    };
    let context_proven = saved.context_delivery.is_some_and(|delivery| {
        saved.inputs.iter().any(|proof| {
            proof.delivery == delivery && control.replayed_input_hashes().contains(&proof.hash)
        })
    });
    let current =
        (!resumed || saved.context_hash != context_hash || !context_proven).then_some(context);
    // Serialize the wrapper before claiming, reserving its exact byte cost.
    // The engine bounds the Delivery JSON; no post-claim input truncation.
    let prefix = format!(
        "{{\"version\":1,\"context\":{},\"delivery\":",
        serde_json::to_string(&current).map_err(|_| Error::InvalidRequest)?
    );
    let budget = MAX_INPUT_BYTES
        .checked_sub(prefix.len() + 1)
        .ok_or(Error::InputTooSmall)?;
    if budget < 1024 {
        return Err(Error::InputTooSmall.into());
    }
    let Some(delivery) = start
        .port
        .claim(start.thread, session.as_uuid(), resume, budget)?
    else {
        return Ok(());
    };
    if delivery.turn.worker != start.worker || delivery.turn.session != session.as_uuid() {
        return Err(Error::StaleTurn.into());
    }
    let turn = delivery.turn;
    *claimed = Some(turn);
    let mut text = prefix;
    text.push_str(&serde_json::to_string(&delivery).map_err(|_| Error::InvalidRequest)?);
    text.push('}');
    let input = AgentInput {
        turn_id: turn
            .id
            .to_string()
            .parse()
            .map_err(|_| Error::InvalidRequest)?,
        text,
        attachments: vec![],
        references: vec![],
        mode: None,
        fast: None,
        reasoning: None,
        model: None,
        skills: vec![],
    };
    input.validate(MAX_INPUT_BYTES)?;
    journal(
        database,
        turn.id,
        JournalEntry::Input(input.text.as_bytes()),
    )?;
    saved.inputs.push(InputProof {
        delivery: delivery.id,
        hash: *blake3::hash(input.text.as_bytes()).as_bytes(),
    });
    if saved.inputs.len() > MAX_SAVED_INPUTS {
        saved.inputs.remove(0);
    }
    saved.token = control.resume_token();
    saved.context_hash = context_hash;
    if current.is_some() {
        saved.context_delivery = Some(delivery.id);
    }
    // Persist before submit. This records an offer, not acceptance: only a
    // matching user-input hash in qualified replay can prove it later.
    save(database, start, session, &saved)?;
    if let Err(error) = control.submit(input) {
        start.port.unknown(turn, delivery.id)?;
        return Err(error.into());
    }
    let result = observe(
        start, database, stop, commands, control, &delivery, &mut saved,
    );
    saved.token = control.resume_token();
    save(database, start, session, &saved)?;
    if result.is_err() {
        start.port.unknown(turn, delivery.id)?;
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn observe(
    start: &Start,
    database: &Mutex<BotDatabase>,
    stop: &AtomicBool,
    commands: &mpsc::Receiver<Control>,
    control: &mut dyn AgentControl,
    delivery: &Delivery,
    saved: &mut Saved,
) -> Result<(), Failure> {
    let turn = delivery.turn;
    let id: AgentTurnId = turn
        .id
        .to_string()
        .parse()
        .map_err(|_| Error::InvalidRequest)?;
    let mut accepted = false;
    let mut reply = String::new();
    let mut steer_at = std::time::Instant::now();
    // The isolated P1.7 probe qualifies this installed route. Until another
    // route/version has explicit acknowledgement evidence its input simply
    // waits for the next turn (D16), even if an adapter implements steer().
    let mut steering = control.capabilities().is_some_and(|caps| {
        caps.provider.as_str() == "claude" && caps.executable_version.as_str() == "0.76.0"
    });
    loop {
        if stop.load(Ordering::Acquire) {
            control.cancel(id)?;
            if !accepted {
                start.port.unknown(turn, delivery.id)?;
            }
            return Ok(());
        }
        for command in commands.try_iter() {
            if command.turn != turn {
                continue;
            }
            match command.action {
                Action::Approval {
                    id,
                    decision,
                    choice,
                } => control.answer_approval(id, decision, choice)?,
            }
        }
        if accepted && steering && std::time::Instant::now() >= steer_at {
            steer_at = std::time::Instant::now() + Duration::from_millis(200);
            steering = steer(start, database, control, turn, saved)?;
        }
        let Some(event) = control.next_event(Duration::from_millis(100))? else {
            continue;
        };
        let bytes = encode(&event)?;
        // Normalized turn events carry a typed v4 identity. Session metadata
        // has no turn; a stale turn's output must never reach this one.
        let value = serde_json::to_value(&event).map_err(|_| Error::InvalidRequest)?;
        if value
            .get("turn_id")
            .is_some_and(|value| value.as_str() != Some(&turn.id.to_string()))
        {
            continue;
        }
        journal(database, turn.id, JournalEntry::Event(&bytes))?;
        // submit/TurnStarted prove a write to the transport, not acceptance.
        // Live processing or a completed response proves the input was read.
        let evidence = matches!(
            event,
            AgentEvent::MessageDelta { .. }
                | AgentEvent::ToolStarted { .. }
                | AgentEvent::ApprovalRequired { .. }
                | AgentEvent::QuestionsRequired { .. }
                | AgentEvent::TurnCompleted { .. }
        );
        if evidence && !accepted {
            start.port.accepted(turn, delivery.id)?;
            journal(
                database,
                turn.id,
                JournalEntry::EngineDelivery(&encode(
                    &serde_json::json!({"accepted":delivery.id}),
                )?),
            )?;
            accepted = true;
        }
        start.port.event(turn, &event)?;
        // Event is already journaled above. The engine acknowledgement is
        // control evidence, not another full copy of every text/tool event.
        match event {
            AgentEvent::MessageDelta { text, .. } => {
                if reply.len().saturating_add(text.len()) > koru_bot_client_wire::MAX_FRAME_BYTES {
                    return Err(Error::InvalidRequest.into());
                }
                reply.push_str(&text);
            }
            AgentEvent::TurnCompleted { .. } => {
                start.port.publish(turn, Uuid::new_v4(), reply)?;
                return Ok(());
            }
            AgentEvent::TurnCancelled { .. } | AgentEvent::TurnFailed { .. } => {
                if !accepted {
                    start.port.unknown(turn, delivery.id)?;
                }
                return Err(Failure::ProviderEnded);
            }
            _ => {}
        }
    }
}

fn steer(
    start: &Start,
    database: &Mutex<BotDatabase>,
    control: &mut dyn AgentControl,
    turn: Turn,
    saved: &mut Saved,
) -> Result<bool, Failure> {
    const PREFIX: &str = "{\"version\":1,\"context\":null,\"delivery\":";
    let Some(delivery) = start
        .port
        .offer_steer(turn, MAX_INPUT_BYTES - PREFIX.len() - 1)?
    else {
        return Ok(true);
    };
    if delivery.turn != turn {
        return Err(Error::StaleTurn.into());
    }
    // Both native steering adapters currently carry text only.
    // Files remain structured and owed for the next ordinary submission.
    if delivery
        .work
        .iter()
        .any(|message| !message.content.attachments.is_empty())
    {
        start.port.reject_steer(turn, delivery.id)?;
        return Ok(true);
    }
    let text = format!(
        "{PREFIX}{}}}",
        serde_json::to_string(&delivery).map_err(|_| Error::InvalidRequest)?
    );
    let input = AgentInput {
        turn_id: turn
            .id
            .to_string()
            .parse()
            .map_err(|_| Error::InvalidRequest)?,
        text,
        attachments: vec![],
        references: vec![],
        mode: None,
        fast: None,
        reasoning: None,
        model: None,
        skills: vec![],
    };
    input.validate(MAX_INPUT_BYTES)?;
    journal(
        database,
        turn.id,
        JournalEntry::Input(input.text.as_bytes()),
    )?;
    saved.inputs.push(InputProof {
        delivery: delivery.id,
        hash: *blake3::hash(input.text.as_bytes()).as_bytes(),
    });
    if saved.inputs.len() > MAX_SAVED_INPUTS {
        saved.inputs.remove(0);
    }
    saved.token = control.resume_token();
    let session = turn
        .session
        .to_string()
        .parse()
        .map_err(|_| Error::InvalidRequest)?;
    save(database, start, session, saved)?;
    match control.steer(input) {
        Ok(()) => {
            // steer() returns success only for the adapter's explicit native
            // acknowledgement. Later text/completion cannot stand in for it.
            start.port.accepted(turn, delivery.id)?;
            journal(
                database,
                turn.id,
                JournalEntry::EngineDelivery(&encode(
                    &serde_json::json!({"accepted":delivery.id}),
                )?),
            )?;
            Ok(true)
        }
        Err(error)
            if matches!(
                error.code,
                koru_session_protocol::ErrorCode::InvalidTransition
                    | koru_session_protocol::ErrorCode::DeliveryUnsupported
            ) =>
        {
            start.port.reject_steer(turn, delivery.id)?;
            Ok(false)
        }
        Err(error) => {
            start.port.unknown(turn, delivery.id)?;
            Err(error.into())
        }
    }
}
