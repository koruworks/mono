// SPDX-License-Identifier: MPL-2.0

//! Localized readiness is a projection of the host's closed values.
use koru_i18n::Key;
use koru_session_protocol::{ErrorCode, ExecutableAvailability, ProviderReadiness, capability};

pub(crate) fn status(row: &ProviderReadiness) -> Option<Key> {
    match row.availability {
        ExecutableAvailability::Checking => Some(Key::TerminalChecking),
        ExecutableAvailability::Missing => Some(Key::TerminalNotInstalled),
        ExecutableAvailability::SignedOut { .. } => Some(Key::TerminalManagedSignedOut),
        ExecutableAvailability::Incompatible { .. } => Some(Key::TerminalManagedUnsupported),
        ExecutableAvailability::Failed { .. } => Some(Key::TerminalManagedCheckFailed),
        ExecutableAvailability::Available { .. } => {
            (!row.capabilities.supports(capability::MANAGED_DELEGATION))
                .then_some(Key::TerminalManagedCannotDelegate)
        }
    }
}

pub(crate) fn guidance(row: &ProviderReadiness) -> Option<Key> {
    if row.kind.as_str() == "claude" {
        match row.availability {
            ExecutableAvailability::Missing => {
                return Some(Key::TerminalManagedClaudeWrapperMissing);
            }
            ExecutableAvailability::SignedOut { .. } => {
                return Some(Key::TerminalManagedClaudeSignInRequired);
            }
            ExecutableAvailability::Failed { .. }
                if row.failure.as_ref().is_some_and(|failure| {
                    failure.diagnostic
                        == koru_session_protocol::ProviderDiagnostic::RuntimeUnavailable
                }) =>
            {
                return Some(Key::TerminalManagedClaudeRuntimeUnavailable);
            }
            _ => {}
        }
    }

    match row.availability {
        ExecutableAvailability::Checking => Some(Key::TerminalChecking),
        ExecutableAvailability::Missing => Some(Key::TerminalManagedInstallRequired),
        ExecutableAvailability::SignedOut { .. } => Some(Key::TerminalManagedSignInRequired),
        ExecutableAvailability::Incompatible {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        } => Some(Key::TerminalManagedHostUpdateRequired),
        ExecutableAvailability::Incompatible { .. } => Some(Key::TerminalManagedUpdateRequired),
        ExecutableAvailability::Failed { .. } => Some(Key::TerminalManagedRetryRequired),
        ExecutableAvailability::Available { .. } => {
            status(row).map(|_| Key::TerminalManagedChooseAnother)
        }
    }
}

pub(crate) fn retryable(row: &ProviderReadiness) -> bool {
    !matches!(row.availability, ExecutableAvailability::Checking) && status(row).is_some()
}
