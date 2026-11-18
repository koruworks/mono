// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::bot_worker::{Heartbeat, Registration, Reply, TIMEOUT, VERSION};

pub(super) fn serve(
    listener: koru_ipc::Listener,
    registry: Registry,
    events: mpsc::Sender<Event>,
) -> Result<(), Error> {
    let pending = Arc::new(AtomicU32::new(0));
    std::thread::Builder::new()
        .name("koru-bot registration".into())
        .spawn(move || {
            while let Ok(stream) = listener.accept() {
                if !koru_ipc::same_user(&stream) {
                    continue;
                }
                if pending.fetch_add(1, Ordering::AcqRel) >= 16 {
                    pending.fetch_sub(1, Ordering::AcqRel);
                    continue;
                }
                let permit = Permit(pending.clone());
                let registry = registry.clone();
                let events = events.clone();
                let _ = std::thread::Builder::new()
                    .name("koru-bot connection".into())
                    .spawn(move || connection(stream, registry, events, permit));
            }
        })
        .map(|_| ())
        .map_err(|_| Error::EngineUnavailable)
}
struct Permit(Arc<AtomicU32>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Connected(Arc<Gate>);
impl Drop for Connected {
    fn drop(&mut self) {
        self.0.registration.store(DISCONNECTED, Ordering::Release);
    }
}

fn connection(
    mut stream: koru_ipc::Stream,
    registry: Registry,
    events: mpsc::Sender<Event>,
    permit: Permit,
) {
    koru_ipc::set_timeouts(&stream, Some(TIMEOUT));
    let Ok((VERSION, request)) = koru_ipc::frame::read_frame::<_, Registration>(&mut stream) else {
        return;
    };
    let gate = registry
        .lock()
        .ok()
        .and_then(|entries| entries.get(&request.bot).cloned());
    let Some(gate) = gate.filter(|gate| claim(gate, &request, Instant::now())) else {
        let _ = koru_ipc::frame::write_frame(&mut stream, VERSION, &Reply::Refused);
        return;
    };
    let _connected = Connected(gate.clone());
    // A registered connection no longer occupies a pending-handshake permit.
    drop(permit);
    let at_ms = now_ms();
    gate.last_heard.store(at_ms as u64, Ordering::Release);
    if koru_ipc::frame::write_frame(&mut stream, VERSION, &Reply::Registered).is_err() {
        return;
    }
    let _ = events.send(Event::Registered {
        bot: request.bot,
        incarnation: request.incarnation,
        at_ms,
    });
    while let Ok((VERSION, heartbeat)) = koru_ipc::frame::read_frame::<_, Heartbeat>(&mut stream) {
        gate.sessions
            .store(heartbeat.live_sessions, Ordering::Release);
        gate.last_heard.store(now_ms() as u64, Ordering::Release);
        let stopping = gate.stopping.load(Ordering::Acquire);
        if koru_ipc::frame::write_frame(
            &mut stream,
            VERSION,
            &if stopping {
                Reply::Stop
            } else {
                Reply::Continue
            },
        )
        .is_err()
            || stopping
        {
            break;
        }
    }
}

fn claim(gate: &Gate, request: &Registration, now: Instant) -> bool {
    if now >= gate.deadline
        || gate.stopping.load(Ordering::Acquire)
        || request.bot != gate.bootstrap.bot
        || request.incarnation != gate.bootstrap.incarnation
        || request.capability.len() != gate.bootstrap.capability.len()
        || request
            .capability
            .bytes()
            .zip(gate.bootstrap.capability.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            != 0
    {
        return false;
    }
    // One monotonic state: a replay cannot transiently resurrect connectivity
    // after the first connection closes. There is no reset to PENDING.
    gate.registration
        .compare_exchange(PENDING, CONNECTED, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

#[cfg(test)]
mod tests;
