// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn admission_refusals_preserve_exact_error_report_and_header_grammar() {
    for code in &BlobCode::ALL[17..] {
        assert_eq!(code.status(), Some(422));
        assert_eq!(BlobCode::parse(code.as_str()), Some(*code));
        assert_eq!(
            serde_json::to_value(crate::wire::BlobErrorV1::from(*code)).unwrap(),
            serde_json::json!({"code":code.as_str(), "message":"Cloud admission refused"})
        );
        let report = crate::wire::ReportOutcomeV1::Fenced { reason: *code };
        let json = serde_json::json!({"state":"fenced", "reason":code.as_str()});
        assert_eq!(serde_json::to_value(report).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<crate::wire::ReportOutcomeV1>(json).unwrap(),
            report
        );
    }
    assert!(BlobCode::parse("feedback_due").is_none());
}

/// The registry is §7's, whole and in its order. A code that is added to the
/// contract and not here — or here and not in the contract — is a vocabulary a
/// person meets in one place and not the other.
#[test]
fn the_registry_is_exactly_the_contracts_codes() {
    let expected = [
        "malformed_range",
        "range_conflict",
        "length_mismatch",
        "hash_mismatch",
        "wrong_region",
        "wrong_workspace",
        "wrong_object",
        "stale_incarnation",
        "stale_epoch",
        "capability_expired",
        "capability_invalid",
        "upload_expired",
        "object_missing",
        "object_too_large",
        "storage_unavailable",
        "mode_refuses_transport",
        "relay_path_refused",
        "cloud_policy_unavailable",
        "cloud_plan_required",
        "cloud_document_count_limit",
        "cloud_stored_document_bytes_limit",
        "cloud_physical_bytes_limit",
        "cloud_staging_bytes_limit",
        "cloud_removal_capacity_limit",
        "file_exceeds_plan_allowance",
        "usage_unknown",
        "account_over_quota",
    ];
    let actual: Vec<_> = BlobCode::ALL.iter().map(|code| code.as_str()).collect();
    assert_eq!(actual, expected);
}

/// The serialized form, the `Display` and the parse are one token. Two
/// spellings of one code is a client that branches on neither.
#[test]
fn every_code_round_trips_through_one_spelling() {
    for code in BlobCode::ALL {
        let text = code.as_str();
        assert_eq!(code.to_string(), text);
        assert_eq!(BlobCode::parse(text), Some(*code));
        assert_eq!(serde_json::to_value(code).unwrap(), serde_json::json!(text));
        assert_eq!(
            serde_json::from_value::<BlobCode>(serde_json::json!(text)).unwrap(),
            *code
        );
        assert!(
            text.bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
            "{text} is lowercase snake_case"
        );
    }
    assert_eq!(BlobCode::parse("not_a_code"), None);
}

/// §7's classing, exactly: five codes are statements about the thing carried
/// and defer their own unit; the rest are statements about the session.
#[test]
fn the_deferral_class_of_each_code_is_the_contracts() {
    for code in [
        BlobCode::RangeConflict,
        BlobCode::LengthMismatch,
        BlobCode::HashMismatch,
        BlobCode::ObjectMissing,
        BlobCode::WrongObject,
    ] {
        assert_eq!(code.deferral(), Deferral::Unit, "{code}");
    }
    for code in [
        BlobCode::StorageUnavailable,
        BlobCode::CapabilityExpired,
        BlobCode::CapabilityInvalid,
        BlobCode::StaleEpoch,
        BlobCode::StaleIncarnation,
        BlobCode::WrongRegion,
        BlobCode::ModeRefusesTransport,
    ] {
        assert_eq!(code.deferral(), Deferral::Session, "{code}");
    }
}

/// The two client-side conditions never reach the wire, and every other code
/// has exactly one status so a route handler never has to choose one.
#[test]
fn only_the_two_client_side_conditions_have_no_status() {
    for code in BlobCode::ALL {
        let status = code.status();
        let client_side = matches!(
            code,
            BlobCode::ModeRefusesTransport | BlobCode::RelayPathRefused
        );
        assert_eq!(status.is_none(), client_side, "{code}");
        if let Some(status) = status {
            assert!((400..600).contains(&status), "{code} answers {status}");
        }
    }
}

/// A denial must not teach an unauthorised caller which locators are real, so
/// "not yours" and "not there" answer the same code with the same status.
#[test]
fn not_yours_and_not_there_are_indistinguishable() {
    assert_eq!(
        BlobCode::WrongObject.status(),
        BlobCode::WrongObject.status()
    );
    assert_ne!(BlobCode::WrongObject, BlobCode::ObjectMissing);
    assert_ne!(
        BlobCode::WrongObject.status(),
        BlobCode::ObjectMissing.status()
    );
}

/// A credential failure says that it failed and never which claim was wrong,
/// with the single exception the contract names: region has its own code.
#[test]
fn a_credential_failure_names_only_region_and_expiry() {
    use koru_account_session::TokenCapabilityVerificationError as Error;
    assert_eq!(
        from_verification_error(Error::WrongRegion),
        BlobCode::WrongRegion
    );
    for error in [Error::CredentialExpired, Error::CredentialNotYetValid] {
        assert_eq!(from_verification_error(error), BlobCode::CapabilityExpired);
    }
    for error in [
        Error::Malformed,
        Error::UnknownKey,
        Error::Signature,
        Error::InvalidClaims,
        Error::UnsupportedVersion,
        Error::WrongPurpose,
        Error::WrongAudience,
        Error::WrongIssuer,
    ] {
        assert_eq!(from_verification_error(error), BlobCode::CapabilityInvalid);
    }
}

/// Developer text is untranslated and structural: it never carries an id, a
/// name, a filename or a length a person could be shown by accident.
#[test]
fn developer_text_is_structural_and_never_a_value() {
    for code in BlobCode::ALL {
        let message = code.message();
        assert!(!message.is_empty(), "{code}");
        assert!(
            message.chars().all(|c| !c.is_ascii_digit()),
            "{code} carries a number"
        );
        assert!(message.is_ascii(), "{code}");
    }
}
