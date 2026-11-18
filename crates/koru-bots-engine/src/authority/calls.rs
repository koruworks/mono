// SPDX-License-Identifier: MPL-2.0
use super::*;
pub(crate) mod arguments;

/// A descriptor constructed by the trusted door from the same parsed value
/// it forwards. Provider JSON never supplies its own argument digest.
#[derive(Clone)]
pub struct Call {
    descriptor: Descriptor,
    arguments: serde_json::Value,
}

/// Only the registered door and gateway construct descriptors. They cross the
/// private authority channel without sending page content to the engine.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    door_call: Uuid,
    tool: String,
    argument_digest: [u8; 32],
}
impl Descriptor {
    pub fn validate(&self) -> Result<(), Error> {
        if self.door_call.is_nil()
            || self.tool.is_empty()
            || self.tool.len() > 256
            || self.tool.chars().any(char::is_control)
        {
            Err(Error::InvalidRequest)
        } else {
            Ok(())
        }
    }
    pub fn tool(&self) -> &str {
        &self.tool
    }
}
impl std::fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallDescriptor(<redacted>)")
    }
}
impl Call {
    /// Parse once, rejecting duplicate keys at every nesting level. Forward
    /// arguments() itself; reparsing another copy could change the descriptor.
    pub fn parse(door_call: Uuid, tool: String, raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > koru_bot_client_wire::MAX_FRAME_BYTES {
            return Err(Error::InvalidRequest);
        }
        let arguments: arguments::Unique =
            serde_json::from_slice(raw).map_err(|_| Error::InvalidRequest)?;
        Self::from_value(door_call, tool, arguments.0)
    }

    /// The door has already rejected duplicate keys in the complete envelope.
    /// Preserve that exact value for both hashing and forwarding.
    pub(crate) fn from_value(
        door_call: Uuid,
        tool: String,
        arguments: serde_json::Value,
    ) -> Result<Self, Error> {
        if door_call.is_nil()
            || tool.is_empty()
            || tool.len() > 256
            || tool.chars().any(char::is_control)
        {
            return Err(Error::InvalidRequest);
        }
        if !arguments.is_object() {
            return Err(Error::InvalidRequest);
        }
        let canonical = receipts::canonical_json(&arguments)?;
        let mut hash = Sha256::new();
        hash.update(b"koru-bots-call-v1");
        hash.update((tool.len() as u32).to_be_bytes());
        hash.update(tool.as_bytes());
        hash.update((canonical.len() as u32).to_be_bytes());
        hash.update(canonical);
        Ok(Self {
            descriptor: Descriptor {
                door_call,
                tool,
                argument_digest: hash.finalize().into(),
            },
            arguments,
        })
    }
    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }
    pub fn arguments(&self) -> &serde_json::Value {
        &self.arguments
    }
    pub fn door_call(&self) -> Uuid {
        self.descriptor.door_call
    }
    pub fn tool(&self) -> &str {
        &self.descriptor.tool
    }
    pub fn argument_digest(&self) -> [u8; 32] {
        self.descriptor.argument_digest
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub id: Uuid,
    pub turn: messages::Turn,
}

/// The boundary evaluates both grants as restrictions. Returning a snapshot
/// does not authorize a resource without that check. No content in Debug.
#[derive(Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub scope: String,
    pub bot: Uuid,
    pub thread: Uuid,
    pub binding: Binding,
    pub captured_grant: serde_json::Value,
    pub current_grant: serde_json::Value,
}
impl std::fmt::Debug for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallResolution(<redacted>)")
    }
}

fn open(
    connection: &Connection,
    identity: &Identity,
    turn: Option<Uuid>,
) -> Result<Resolution, Error> {
    let row:Option<(String,u64,Vec<u8>,Vec<u8>,String)>=connection.query_row(
        "SELECT t.id,t.generation,t.grant,b.access,h.kind FROM turns t JOIN bots b ON b.id=t.bot JOIN threads h ON h.id=t.thread WHERE t.bot=?1 AND t.thread=?2 AND t.session=?3 AND t.worker_incarnation=?4 AND t.state='running' AND t.stopped=0 AND (?5 IS NULL OR t.id=?5)",
        params![identity.bot.to_string(),identity.thread.to_string(),identity.session.to_string(),identity.worker.to_string(),turn.map(|id|id.to_string())],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (turn, generation, captured, current, kind) = row.ok_or(Error::NoTurn)?;
    // Routine policy's fresh restriction arrives with M3. Never widen it to
    // the Bot's main-thread grant in the meantime.
    if kind == "routine" {
        return Err(Error::NotAllowed);
    }
    Ok(Resolution {
        scope: identity.scope.clone(),
        bot: identity.bot,
        thread: identity.thread,
        binding: Binding {
            id: Uuid::nil(),
            turn: messages::Turn {
                id: id(turn)?,
                generation,
                worker: identity.worker,
                session: identity.session,
            },
        },
        captured_grant: decode(&captured)?,
        current_grant: decode(&current)?,
    })
}

fn existing(
    connection: &Connection,
    session: Uuid,
    call: &Descriptor,
) -> Result<Option<Binding>, Error> {
    let row:Option<(String,String,u64,String,Vec<u8>,Vec<u8>)>=connection.query_row("SELECT binding,turn,generation,tool,argument_digest,details FROM tool_calls WHERE session=?1 AND door_call=?2",params![session.to_string(),call.door_call.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    row.map(|(binding, turn, generation, tool, digest, details)| {
        if tool != call.tool || digest.as_slice() != call.argument_digest {
            return Err(Error::BindingConflict);
        }
        let worker: Uuid = decode(&details)?;
        Ok(Binding {
            id: id(binding)?,
            turn: messages::Turn {
                id: id(turn)?,
                generation,
                worker,
                session,
            },
        })
    })
    .transpose()
}

impl Database {
    /// Arrival binds once. P0.2 qualified no provider-call correlation, so
    /// this API deliberately cannot accept a claimed provider call id.
    pub fn bind_call(
        &mut self,
        who: Worker,
        secret: &Secret,
        call: &Call,
        command: Uuid,
        now: i64,
    ) -> Result<Binding, Error> {
        self.bind_descriptor(who, secret, call.descriptor(), command, now)
    }

    pub fn bind_descriptor(
        &mut self,
        who: Worker,
        secret: &Secret,
        call: &Descriptor,
        command: Uuid,
        now: i64,
    ) -> Result<Binding, Error> {
        call.validate()?;
        if command.is_nil()
            || call.door_call.is_nil()
            || call.tool.is_empty()
            || call.tool.len() > 256
            || call.tool.chars().any(char::is_control)
        {
            return Err(Error::InvalidRequest);
        }
        let tx = self.connection.transaction()?;
        let identity = identity(&tx, secret)?;
        if identity.bot != who.bot || identity.worker != who.incarnation {
            return Err(Error::Unauthenticated);
        }
        let prior = existing(&tx, identity.session, call)?;
        let resolved = open(&tx, &identity, prior.map(|binding| binding.turn.id))?;
        let subject = format!("bind-call:{}:{}", who.bot, who.incarnation);
        let digest = receipts::digest(
            "bind-call",
            &(
                identity.session,
                call.door_call,
                &call.tool,
                call.argument_digest,
            ),
        )?;
        if let Some(answer) = receipts::find::<Binding>(&tx, &subject, command, &digest)? {
            if prior != Some(answer) {
                return Err(Error::BindingConflict);
            }
            return Ok(answer);
        }
        if let Some(binding) = prior {
            if binding.turn != resolved.binding.turn {
                return Err(Error::BindingConflict);
            }
            receipts::save(&tx, &subject, command, &digest, &binding, now)?;
            tx.commit()?;
            return Ok(binding);
        }
        let binding = Binding {
            id: Uuid::new_v4(),
            turn: resolved.binding.turn,
        };
        tx.execute("INSERT INTO tool_calls(binding,door_call,session,turn,generation,tool,argument_digest,details) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![binding.id.to_string(),call.door_call.to_string(),identity.session.to_string(),binding.turn.id.to_string(),binding.turn.generation,call.tool,call.argument_digest.as_slice(),encode(&identity.worker)?])?;
        receipts::save(&tx, &subject, command, &digest, &binding, now)?;
        tx.commit()?;
        Ok(binding)
    }

    /// Every use re-reads live identity, the originally bound turn, and both
    /// grants in one transaction. It never resolves the session's newest turn.
    pub fn resolve_call(
        &mut self,
        secret: &Secret,
        binding: Uuid,
        call: &Call,
    ) -> Result<Resolution, Error> {
        self.resolve_descriptor(secret, binding, call.descriptor())
    }

    pub fn resolve_descriptor(
        &mut self,
        secret: &Secret,
        binding: Uuid,
        call: &Descriptor,
    ) -> Result<Resolution, Error> {
        call.validate()?;
        let tx = self.connection.transaction()?;
        let identity = identity(&tx, secret)?;
        let prior = existing(&tx, identity.session, call)?
            .filter(|prior| prior.id == binding)
            .ok_or(Error::BindingConflict)?;
        let mut resolution = open(&tx, &identity, Some(prior.turn.id))?;
        if resolution.binding.turn != prior.turn {
            return Err(Error::BindingConflict);
        }
        resolution.binding = prior;
        tx.commit()?;
        Ok(resolution)
    }
}
