// SPDX-License-Identifier: MPL-2.0

use super::*;
const FIELDS: &[(&str, &str, usize, bool)] = &[
    ("decision_id", "text", 36, false),
    ("session_id", "text", 36, false),
    ("turn_id", "text", 36, false),
    ("prompt_id", "text", 4096, false),
    ("response_key", "text", 36, false),
    ("choice_id", "text", 4096, false),
    ("choice_intent", "text", 32, false),
    ("native_decision", "text", 16, false),
    ("state", "text", 32, false),
    ("acknowledgment_json", "text", DECISION_RECEIPT_BYTES, true),
    ("created_at_ms", "integer", 0, false),
    ("updated_at_ms", "integer", 0, false),
];
pub(super) fn read(
    conn: &Connection,
    receipt: &DecisionReceipt,
) -> Result<Option<DecisionNativeResponse>, BotLedgerError> {
    let found = conn
        .query_row(
            &query("bot_decision_native_answers", FIELDS, "decision_id=?1"),
            [receipt.decision.to_storage()],
            |r| Ok(decode(r, receipt)),
        )
        .optional()
        .map_err(sql)?
        .transpose()?;
    if receipt.state.kind == DecisionKind::NativeApproval
        && receipt.answer.is_some()
        && found.is_none()
    {
        return Err(corrupt("bot_decision_native_answers", "missing"));
    }
    Ok(found)
}
fn decode(
    row: &Row<'_>,
    receipt: &DecisionReceipt,
) -> Result<DecisionNativeResponse, BotLedgerError> {
    let c = Cells {
        row,
        table: "bot_decision_native_answers",
    };
    c.check()?;
    let choice = DecisionNativeChoice {
        id: c.id("choice_id")?,
        intent: c.id("choice_intent")?,
    };
    let answer = receipt
        .answer
        .as_ref()
        .ok_or_else(|| corrupt(c.table, "answer"))?;
    c.same(
        answer.choice
            == DecisionAnswerChoice::Native {
                offered: choice.clone(),
            }
            && c.get::<String>("native_decision")?
                == if choice.intent.grants() {
                    "approve"
                } else {
                    "reject"
                },
    )?;
    let key = DecisionNativeResponseKey {
        decision: c.id("decision_id")?,
        session: c.id("session_id")?,
        turn: c.id("turn_id")?,
        prompt: c.id("prompt_id")?,
        response_key: c.id("response_key")?,
        choice,
    };
    c.same(
        key.matches_binding(receipt.decision, &receipt.binding)
            && c.get::<i64>("created_at_ms")? == answer.answered_at_ms
            && c.get::<i64>("updated_at_ms")? >= answer.answered_at_ms,
    )?;
    let acknowledgment: Option<DecisionNativeResponse> = c.optional_json("acknowledgment_json")?;
    let response = match (c.get::<String>("state")?.as_str(), acknowledgment) {
        ("prepared", None) => DecisionNativeResponse::Prepared { key },
        ("unconfirmed", None) => DecisionNativeResponse::Unconfirmed { key },
        ("acknowledged", Some(v @ DecisionNativeResponse::Acknowledged { .. })) => {
            if let DecisionNativeResponse::Acknowledged {
                key: k,
                acknowledgment,
                observed_at_ms,
            } = &v
            {
                c.same(
                    k == &key
                        && acknowledgment == &key
                        && *observed_at_ms >= answer.answered_at_ms
                        && *observed_at_ms <= c.get::<i64>("updated_at_ms")?,
                )?;
            }
            v
        }
        _ => return Err(corrupt(c.table, "acknowledgment")),
    };
    valid(&response).map_err(|_| corrupt(c.table, "response"))?;
    Ok(response)
}
