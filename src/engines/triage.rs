use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::app::App;
use crate::db;
use crate::engines::{action, catalog, coordination, talk};
use crate::error::{AppError, AppResult};

pub const COMPLAINTS: &[&str] = &[
    "damaged_delivery",
    "late_shipment",
    "wrong_item",
    "missing_part",
    "billing_dispute",
    "refund_request",
];

pub async fn after_intake(app: &App, case_id: &str) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    let customer = coordination::require_bound(app, case_id, case.generation, "customer")?;
    let recorded = format!(
        "Customer {} said: {}",
        case.customer_id, case.summary
    );
    app.connector()
        .write_memory(&customer.pid, case_id, &recorded)
        .await?;
    app.db.add_message(case_id, &case.customer_id, "customer", &case.summary)?;
    app.db.add_evidence(case_id, "complaint_record", "customer", &recorded)?;

    let rule = catalog::type_rule(app, &app.db.issue_type(case_id)?)?;
    let receipt = talk::consult(
        app,
        &customer.pid,
        &customer.session_id,
        "customer",
        &format!(
            "{rule}\n\nThe customer just said this and nothing else: {}. Write the reply they will read. Apologize briefly. Say the team is looking at it. Repeat those facts. Give the next step from that knowledge line. If the knowledge names a time, say that time. If it does not, say the stored knowledge does not set a time. A person will answer in this thread. Do not invent a deadline or a remedy.",
            case.summary
        ),
    )
    .await?;
    let receipt_text = receipt["text"].as_str().unwrap_or("").trim().to_string();
    if receipt_text.is_empty() {
        return Err(AppError::unavailable(
            "connector_talk_empty: the customer agent did not write a receipt, so none was stored",
        ));
    }
    app.connector()
        .write_memory(&customer.pid, case_id, &receipt_text)
        .await?;
    app.db.add_message(case_id, &case.customer_id, "customer_agent", &receipt_text)?;

    let draft = draft_for_person(app, case_id, &case.summary).await?;
    let class = draft.class;
    app.db.save_triage(case_id, Some(class), None, None)?;
    file_and_notify(app, case_id, class, &case.summary).await?;
    app.db.set_status(case_id, "awaiting_human")?;
    Ok(json!({
        "classification": class,
        "receipt": receipt_text,
        "draft": draft.text,
        "triage": app.db.triage(case_id)?,
    }))
}

struct Draft {
    class: &'static str,
    text: String,
}

async fn draft_for_person(app: &App, case_id: &str, facts: &str) -> AppResult<Draft> {
    let case = app.db.get_case(case_id)?;
    let worker = coordination::require_bound(app, case_id, case.generation, "worker")?;
    let rule = catalog::type_rule(app, &app.db.issue_type(case_id)?)?;
    let reply = talk::consult(
        app,
        &worker.pid,
        &worker.session_id,
        "worker",
        &format!(
            "{rule}\n\nFacts the customer already gave: {facts}. You may use only these facts and that knowledge line. Write the reply a person will release to the customer. First line is exactly one of: easy, hard, augmented. Then a blank line. Then the reply: a short apology, that the team is looking at it, the facts, and the next step from the knowledge. If the knowledge names a time, include that time. If it does not, say the stored knowledge does not set a time. easy means the facts are enough. hard means something is still missing. augmented means a person must judge. Do not promise money or a finding."
        ),
    )
    .await?;
    let text = reply["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(AppError::unavailable(
            "connector_talk_empty: the worker did not draft a reply, so none was stored",
        ));
    }
    app.connector()
        .write_memory(&worker.pid, case_id, &text)
        .await?;
    let class = text.lines().next().and_then(parse_class).unwrap_or("augmented");
    let body = text
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    let body = if body.is_empty() { text.clone() } else { body };
    app.db.add_message(case_id, &case.customer_id, "worker_draft", &body)?;
    app.db.add_evidence(case_id, "worker_draft", "worker", &body)?;
    app.db.notify(
        case_id,
        "operator",
        &format!("{class} draft for {case_id}: {body}"),
    )?;
    Ok(Draft { class, text: body })
}

pub async fn add_info(app: &App, case_id: &str, detail: &str) -> AppResult<Value> {
    let detail = detail.trim();
    if detail.is_empty() {
        return Err(AppError::bad("a message is required"));
    }
    let case = app.db.get_case(case_id)?;
    let customer = coordination::require_bound(app, case_id, case.generation, "customer")?;
    let _ = app.connector().write_memory(&customer.pid, case_id, detail).await;
    app.db.add_message(case_id, &case.customer_id, "customer", detail)?;
    let rule = catalog::type_rule(app, &app.db.issue_type(case_id)?)?;
    let receipt = talk::consult(
        app,
        &customer.pid,
        &customer.session_id,
        "customer",
        &format!(
            "{rule}\n\nEarlier the customer said: {}.\nThey just added: {detail}. Write the reply they will read. Apologize briefly. Say the team is looking at it. Repeat the new facts. Give the next step from that knowledge line. If the knowledge names a time, say that time. If it does not, say the stored knowledge does not set a time. A person will answer in this thread. Do not invent a deadline or a remedy.",
            case.summary
        ),
    )
    .await?;
    let receipt_text = receipt["text"].as_str().unwrap_or("").trim().to_string();
    if receipt_text.is_empty() {
        return Err(AppError::unavailable(
            "connector_talk_empty: the customer agent did not answer this message, so none was stored",
        ));
    }
    let _ = app.connector().write_memory(&customer.pid, case_id, &receipt_text).await;
    app.db.add_message(case_id, &case.customer_id, "customer_agent", &receipt_text)?;
    let class = app
        .db
        .triage(case_id)?
        .and_then(|row| row.get("classification").and_then(|v| v.as_str()).map(str::to_string))
        .unwrap_or_else(|| "augmented".to_string());
    app.db.save_triage(case_id, None, None, Some(detail))?;
    app.db.add_evidence(case_id, "customer_followup", "customer", detail)?;
    draft_for_person(app, case_id, &format!("{}\n\nFollow-up: {detail}", case.summary)).await?;
    file_and_notify(app, case_id, &class, &format!("{}\n\nFollow-up: {detail}", case.summary)).await?;
    Ok(json!({
        "case_id": case_id,
        "receipt": receipt_text,
        "triage": app.db.triage(case_id)?,
    }))
}

/// The worker drafts a reply the operator can confirm. If the worker cannot,
/// the desk gets a notification asking what the customer should be told.
/// The customer does not see this draft.
pub async fn suggest(app: &App, case_id: &str) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    if let Err(err) = coordination::bind(app, case_id, case.generation).await {
        app.db.notify(
            case_id,
            "operator",
            &format!(
                "Report {case_id} is stored. A worker agent is not registered yet ({}). What should the customer be told?",
                err.message
            ),
        )?;
        app.db.set_status(case_id, "awaiting_human")?;
        return Ok(json!({ "suggested": false, "reason": err.message }));
    }
    let case = app.db.get_case(case_id)?;
    let worker = coordination::require_bound(app, case_id, case.generation, "worker")?;
    let reply = talk::consult(
        app,
        &worker.pid,
        &worker.session_id,
        "worker",
        &format!(
            "Facts the customer already gave: {}. Write the reply the company could send. Apologize briefly, say the team is looking, and repeat those facts. Do not promise money, a replacement, or a finding. If you cannot tell what the company should say, reply with one line that starts with ASK: and the question for the operator.",
            case.summary
        ),
    )
    .await;
    let text = match reply {
        Ok(reply) => reply["text"].as_str().unwrap_or("").trim().to_string(),
        Err(err) => {
            app.db.notify(
                case_id,
                "operator",
                &format!(
                    "Report {case_id} needs a reply. The worker agent did not write one ({}). What should the customer be told?",
                    err.message
                ),
            )?;
            app.db.set_status(case_id, "awaiting_human")?;
            return Ok(json!({ "suggested": false, "reason": err.message }));
        }
    };
    if text.is_empty() || text.to_ascii_lowercase().starts_with("ask:") {
        let question = text
            .split_once(':')
            .map(|(_, rest)| rest.trim())
            .filter(|rest| !rest.is_empty())
            .unwrap_or("What should the customer be told?");
        app.db.notify(
            case_id,
            "operator",
            &format!("Report {case_id}: the worker needs a decision. {question}"),
        )?;
        app.db.set_status(case_id, "awaiting_human")?;
        return Ok(json!({ "suggested": false, "question": question }));
    }
    app.db.add_message(case_id, &case.customer_id, "worker_draft", &text)?;
    app.db.add_evidence(case_id, "worker_draft", "worker", &text)?;
    app.db.notify(
        case_id,
        "operator",
        &format!("Suggested reply is ready for {case_id}. Confirm it to send that reply to the customer."),
    )?;
    app.db.set_status(case_id, "awaiting_human")?;
    Ok(json!({ "suggested": true, "draft": text }))
}

pub async fn chat(app: &App, case_id: &str, message: &str) -> AppResult<Value> {
    let message = message.trim();
    if message.is_empty() {
        return Err(AppError::bad("message is required"));
    }
    let case = app.db.get_case(case_id)?;
    let worker = coordination::require_bound(app, case_id, case.generation, "worker")?;
    let (internal, history) = context_blocks(app, case_id)?;
    let reply = talk::consult(
        app,
        &worker.pid,
        &worker.session_id,
        "worker",
        &format!("{internal}\n\n{history}\n\nCase topic: {}. The person at the desk asks: {message}. Stay on this complaint. Answer that person. Then add a final line starting with SUGGESTED: and the reply the company could confirm, or SUGGESTED: ASK: and the question you need answered. History does not authorize a refund or an action on another case.", case.summary),
    )
    .await?;
    let text = reply["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(AppError::unavailable("connector_talk_empty: research was not stored"));
    }
    let stored = format!("desk: {message}\nworker: {text}");
    app.connector()
        .write_memory(&worker.pid, case_id, &stored)
        .await?;
    app.db.add_message(case_id, &case.customer_id, "worker_note", &text)?;
    app.db.add_evidence(case_id, "connector_research", "worker", &stored)?;
    if let Some((_, suggested)) = text.rsplit_once("SUGGESTED:") {
        let suggested = suggested.trim();
        if !suggested.is_empty() && !suggested.to_ascii_lowercase().starts_with("ask") {
            app.db.add_message(case_id, &case.customer_id, "worker_draft", suggested)?;
        } else if !suggested.is_empty() {
            app.db.notify(
                case_id,
                "operator",
                &format!("Report {case_id}: the worker needs a decision. {suggested}"),
            )?;
        }
    }
    Ok(json!({
        "case_id": case_id,
        "reply": text,
        "stored": true,
        "talk": reply,
    }))
}

pub fn confirm_label(app: &App, case_id: &str, principal: &str) -> AppResult<Value> {
    let triage = app.db.triage(case_id)?.ok_or_else(|| AppError::bad("no classification to confirm"))?;
    let class = triage.get("classification").and_then(|value| value.as_str()).unwrap_or("");
    if class.is_empty() {
        return Err(AppError::bad("Connector has not classified this complaint"));
    }
    app.db.add_evidence(
        case_id,
        "human_confirmed_label",
        "operator",
        &format!("{principal} confirmed label {class}"),
    )?;
    app.db.notify(case_id, "operator", &format!("{principal} confirmed the {class} label on {case_id}"))?;
    Ok(json!({ "confirmed": class, "by": principal }))
}

pub async fn decide(app: &App, case_id: &str, principal: &str, choice: &str, note: &str) -> AppResult<Value> {
    let note = note.trim();
    if note.is_empty() {
        return Err(AppError::bad("a human decision note is required"));
    }
    if !matches!(choice, "replacement" | "refund" | "decline" | "need_more") {
        return Err(AppError::bad("choice must be replacement, refund, decline, or need_more"));
    }
    let triage = app.db.triage(case_id)?.ok_or_else(|| AppError::bad("complaint is not filed"))?;
    if triage.get("file_digest").and_then(|v| v.as_str()).unwrap_or("").is_empty() && choice != "need_more" {
        return Err(AppError::bad("the case file is not stored yet"));
    }
    if choice == "need_more" {
        app.db.save_decision(case_id, choice, note, principal)?;
        app.db.set_status(case_id, "needs_info")?;
        app.db.notify(case_id, "customer", note)?;
        return Ok(json!({ "result": "need_more", "reply": Value::Null }));
    }
    let effect = if choice == "replacement" || choice == "refund" {
        let amount = if choice == "refund" { Some(12_900) } else { None };
        Some(action::propose(app, case_id, choice, "ord_4401", amount).await?)
    } else {
        None
    };
    if let Some(effect) = &effect {
        let result = effect.get("result").and_then(|v| v.as_str()).unwrap_or("");
        if result != "admitted" {
            app.db.save_decision(case_id, choice, note, principal)?;
            app.db.notify(
                case_id,
                "operator",
                &format!("Human chose {choice}, but Connector did not admit it ({result}). No customer reply was sent."),
            )?;
            return Ok(json!({
                "result": result,
                "effect": effect,
                "reply": Value::Null,
                "honesty": "The customer was not told this succeeded.",
            }));
        }
    }
    app.db.save_decision(case_id, choice, note, principal)?;
    let text = release(app, case_id, principal, note).await?;
    Ok(json!({
        "result": "replied",
        "reply": text,
        "effect": effect,
        "source": "human_release",
    }))
}

/// The person confirms the organization's position. The worker hands it to the customer agent.
/// The customer sees only the customer agent's reply. One confirmation stores at most one reply.
pub async fn release(app: &App, case_id: &str, principal: &str, text: &str) -> AppResult<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::bad("the position is required"));
    }
    let case = app.db.get_case(case_id)?;
    if claims_issued_refund(text) && !app.db.refund_recorded(case_id)? {
        return Err(AppError::bad(
            "this position says a refund was issued, and no refund is stored for this complaint",
        ));
    }
    let existing = app.db.latest_confirmation(case_id)?;
    if existing.get("customer_reply_message_id").and_then(|value| value.as_str()).is_some() {
        let message_id = existing["customer_reply_message_id"].as_str().unwrap_or("");
        return app.db.message_body(message_id);
    }
    let (confirmation_id, handoff_ready) = if existing.is_null() {
        (app.db.insert_confirmation(case_id, &case.customer_id, text, principal)?, None)
    } else {
        let id = existing["confirmation_id"].as_str().unwrap_or("").to_string();
        app.db.set_confirmation_position(&id, text)?;
        let handoff = existing.get("worker_handoff").and_then(|value| value.as_str()).map(str::to_string);
        (id, handoff)
    };
    if coordination::require_bound(app, case_id, case.generation, "customer").is_err()
        || coordination::require_bound(app, case_id, case.generation, "worker").is_err()
    {
        coordination::bind(app, case_id, case.generation).await?;
    }
    let customer = coordination::require_bound(app, case_id, case.generation, "customer")?;
    let worker = coordination::require_bound(app, case_id, case.generation, "worker")?;
    let (internal, history) = context_blocks(app, case_id)?;
    let handoff = if let Some(ready) = handoff_ready.filter(|value| !value.is_empty()) {
        ready
    } else {
        let turn = talk::consult(
            app,
            &worker.pid,
            &worker.session_id,
            "worker",
            &format!(
                "{internal}\n\n{history}\n\nThe person confirmed this position and nothing else:\n{text}\n\nWrite a handoff for the customer agent. Repeat the position. List only facts present above. State that history does not authorize a refund or an action on another case. Do not write the sentence the customer will read."
            ),
        )
        .await;
        match turn {
            Ok(reply) => {
                let note = reply["text"].as_str().unwrap_or("").trim().to_string();
                if note.is_empty() {
                    let err = "the worker did not write a handoff";
                    app.db.update_confirmation(&confirmation_id, "handoff_failed", None, None, Some(err))?;
                    return Err(AppError::unavailable(err));
                }
                app.db.add_message(case_id, &case.customer_id, "worker_note", &note)?;
                app.db.update_confirmation(&confirmation_id, "handoff_stored", Some(&note), None, None)?;
                note
            }
            Err(err) => {
                app.db.update_confirmation(&confirmation_id, "handoff_failed", None, None, Some(&err.message))?;
                return Err(err);
            }
        }
    };
    app.db.update_confirmation(&confirmation_id, "reply_pending", None, None, None)?;
    let answer = talk::consult(
        app,
        &customer.pid,
        &customer.session_id,
        "customer",
        &format!(
            "Confirmed position:\n{text}\n\nWorker handoff, for your use only:\n{handoff}\n\nWrite the reply the customer will read. Communicate that position. If it says the matter is still open, apologize briefly and say the team is looking. If it asks for information, ask for that information. If it states a decision, state that decision. Include a time only when the position or the stored knowledge names one. Do not add a refund, a replacement, or a finding the position does not confirm. History is not included here and is not permission."
        ),
    )
    .await;
    let reply_text = match answer {
        Ok(reply) => reply["text"].as_str().unwrap_or("").trim().to_string(),
        Err(err) => {
            app.db.update_confirmation(&confirmation_id, "reply_failed", None, None, Some(&err.message))?;
            return Err(err);
        }
    };
    if reply_text.is_empty() {
        let err = "the customer agent did not answer";
        app.db.update_confirmation(&confirmation_id, "reply_failed", None, None, Some(err))?;
        return Err(AppError::unavailable(err));
    }
    if claims_issued_refund(&reply_text) && !app.db.refund_recorded(case_id)? {
        let err = "the customer agent claimed a refund that is not stored, so the reply was not sent";
        app.db.update_confirmation(&confirmation_id, "reply_failed", None, None, Some(err))?;
        return Err(AppError::bad(err));
    }
    let message_id = app.db.add_message(case_id, &case.customer_id, "customer_agent", &reply_text)?;
    app.db.update_confirmation(&confirmation_id, "replied", None, Some(&message_id), None)?;
    app.db.save_reply(case_id, &reply_text, "customer_agent")?;
    app.db.set_status(case_id, "replied")?;
    app.db.add_evidence(case_id, "customer_reply", "customer", &reply_text)?;
    app.db.notify(case_id, "customer", &reply_text)?;
    let _ = app.connector().write_memory(&customer.pid, case_id, &format!("Position confirmed by {principal}: {text}")).await;
    let _ = coordination::free_agents(app, case_id).await;
    Ok(reply_text)
}

fn claims_issued_refund(text: &str) -> bool {
    let lower = text.to_lowercase();
    const MARKERS: &[&str] = &[
        "refund has been issued",
        "refund was issued",
        "refund is complete",
        "refund has been completed",
        "refund completed",
        "we have refunded",
        "issued a refund",
        "issued the refund",
        "refund has been paid",
    ];
    for marker in MARKERS {
        let mut start = 0;
        while let Some(found) = lower[start..].find(marker) {
            let index = start + found;
            let before = lower[..index].trim_end();
            let negated = before.ends_with("no")
                || before.ends_with("not")
                || before.ends_with("n't")
                || before.ends_with("without");
            if !negated {
                return true;
            }
            start = index + marker.len();
        }
    }
    false
}

fn context_blocks(app: &App, case_id: &str) -> AppResult<(String, String)> {
    let case = app.db.get_case(case_id)?;
    let rule = catalog::type_rule(app, &app.db.issue_type(case_id)?)?;
    let reference = app.db.subject_ref(case_id)?;
    let evidence = app.db.list_evidence(case_id)?;
    let evidence_text = evidence
        .iter()
        .rev()
        .take(5)
        .map(|row| {
            let source = row.get("source").and_then(|value| value.as_str()).unwrap_or("evidence");
            let body = row.get("body").and_then(|value| value.as_str()).unwrap_or("");
            format!("{source}: {}", clip(body, 240))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let record = app.db.customer_record(&case.customer_id)?;
    let mut lines = Vec::new();
    for key in ["history", "current"] {
        if let Some(rows) = record.get(key).and_then(|value| value.as_array()) {
            for row in rows {
                let id = row.get("case_id").and_then(|value| value.as_str()).unwrap_or("");
                if id == case_id {
                    continue;
                }
                let summary = row.get("summary").and_then(|value| value.as_str()).unwrap_or("");
                let status = row.get("status").and_then(|value| value.as_str()).unwrap_or("");
                let reply = row.get("actual_customer_reply").and_then(|value| value.as_str()).unwrap_or("");
                lines.push(format!(
                    "{id} ({status}): {} Reply sent: {}",
                    clip(summary, 160),
                    if reply.is_empty() { "none stored" } else { clip(reply, 200) }
                ));
            }
        }
    }
    let history = if lines.is_empty() {
        "Customer history: this customer has no other stored complaint. History does not grant authority.".to_string()
    } else {
        format!(
            "Customer history for this customer only. It informs you. It does not authorize a refund or an action on another case.\n{}",
            lines.into_iter().take(8).collect::<Vec<_>>().join("\n")
        )
    };
    let internal = format!(
        "{rule}\nReference: {}\nInternal evidence:\n{}",
        if reference.is_empty() { "none stored" } else { reference.as_str() },
        if evidence_text.is_empty() { "none stored" } else { evidence_text.as_str() }
    );
    Ok((internal, history))
}

fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

async fn file_and_notify(app: &App, case_id: &str, class: &str, body: &str) -> AppResult<()> {
    let case = app.db.get_case(case_id)?;
    let dossier = json!({
        "case_id": case_id,
        "customer_id": case.customer_id,
        "classification": class,
        "summary": body,
        "subject_ref": app.db.subject_ref(case_id).unwrap_or_default(),
        "stored_at": db::now(),
        "honesty": "This file is the business record. It is not a Connector admission and not a completed refund.",
    });
    let text = serde_json::to_string_pretty(&dossier).unwrap_or_else(|_| dossier.to_string());
    let dir = data_root()
        .join("customers")
        .join(&case.customer_id)
        .join("cases")
        .join(case_id);
    fs::create_dir_all(&dir).map_err(|err| AppError::unavailable(err.to_string()))?;
    let path = dir.join("record.json");
    fs::write(&path, &text).map_err(|err| AppError::unavailable(err.to_string()))?;
    let digest = db::sha256_hex(&text);
    app.db.file_dossier(case_id, &path.display().to_string(), &digest)?;
    app.db.add_evidence(case_id, "case_file", "casepilot", &format!("{digest} {}", path.display()))?;
    app.db.set_status(case_id, "awaiting_human")?;
    app.db.notify(
        case_id,
        "operator",
        &format!("{class} report {case_id}: {}", body.chars().take(180).collect::<String>()),
    )?;
    Ok(())
}

fn data_root() -> PathBuf {
    if let Ok(db) = std::env::var("CASEPILOT_DB") {
        if let Some(parent) = PathBuf::from(db).parent() {
            if !parent.as_os_str().is_empty() {
                return parent.to_path_buf();
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}

fn parse_class(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    let hits = [
        lower.contains("easy"),
        lower.contains("hard"),
        lower.contains("augmented"),
    ]
    .into_iter()
    .filter(|hit| *hit)
    .count();
    if hits != 1 {
        return None;
    }
    if lower.contains("augmented") {
        Some("augmented")
    } else if lower.contains("hard") {
        Some("hard")
    } else {
        Some("easy")
    }
}

#[cfg(test)]
mod tests {
    use super::parse_class;

    #[test]
    fn one_class_only() {
        assert_eq!(parse_class("easy"), Some("easy"));
        assert_eq!(parse_class("This is hard."), Some("hard"));
        assert_eq!(parse_class("augmented case"), Some("augmented"));
        assert_eq!(parse_class("easy or hard"), None);
        assert_eq!(parse_class("I need more time"), None);
    }
}
