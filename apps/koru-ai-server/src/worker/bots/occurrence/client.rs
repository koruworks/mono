// SPDX-License-Identifier: MPL-2.0
use super::*;

macro_rules! starter {
    ($name:ident, $request:ty, $reply:ty, $variant:ident, $read:expr) => {
        pub fn $name(
            &self,
            context: RoutineCallContext,
            request: $request,
        ) -> Result<RoutineCall<$reply>, RoutineBridgeRefusal> {
            context.validate()?;
            if !request.valid() {
                return Err(RoutineBridgeRefusal::Invalid);
            }
            let permit = self.routine_calls.acquire($read)?;
            let (reply, call) = channel(context.cancellation.clone());
            let incoming = Incoming {
                request: Request::$variant(request),
                reply: Reply::$variant(reply),
                context,
                permit,
            };
            self.sender
                .try_send(Envelope::RoutineOccurrence(Box::new(incoming)))
                .map_err(|error| match error {
                    TrySendError::Full(_) => RoutineBridgeRefusal::Capacity,
                    TrySendError::Disconnected(_) => RoutineBridgeRefusal::Unavailable,
                })?;
            Ok(call)
        }
    };
}
impl AiWorkerClient {
    starter!(
        synchronize_routine_preclaim,
        SynchronizeRoutinePreclaim,
        SynchronizeRoutinePreclaimReply,
        Preclaim,
        false
    );
    starter!(
        prepare_routine_occurrence,
        PrepareOccurrence,
        PrepareOccurrenceReply,
        Prepare,
        false
    );
    starter!(
        read_routine_preparation,
        ReadPreparation,
        ReadPreparationReply,
        Read,
        true
    );
    starter!(
        finalize_routine_occurrence,
        FinalizeOccurrence,
        FinalizeOccurrenceReply,
        Finalize,
        false
    );
    starter!(
        read_routine_occurrence_link,
        ReadOccurrenceLink,
        ReadOccurrenceLinkReply,
        Link,
        true
    );
}
