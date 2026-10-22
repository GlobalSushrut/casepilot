use serde_json::{json, Value};

use crate::app::App;
use crate::connector::{connector_error, session_id_of};
use crate::engines::setup;
use crate::error::{AppError, AppResult};

pub struct CaseType {
    pub id: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub prompt: &'static str,
}

pub const TYPES: &[CaseType] = &[
    CaseType { id: "theft", group: "public_safety", label: "Theft", prompt: "What was taken, and when?" },
    CaseType { id: "burglary", group: "public_safety", label: "Burglary", prompt: "Where did someone enter, and what is missing?" },
    CaseType { id: "robbery", group: "public_safety", label: "Robbery", prompt: "What was taken, and was anyone hurt?" },
    CaseType { id: "assault", group: "public_safety", label: "Assault", prompt: "When did it happen, and who was hurt?" },
    CaseType { id: "missing_person", group: "public_safety", label: "Missing person", prompt: "Who is missing, and when were they last seen?" },
    CaseType { id: "fraud", group: "public_safety", label: "Fraud", prompt: "What was misrepresented, and what was lost?" },
    CaseType { id: "harassment", group: "public_safety", label: "Harassment", prompt: "What happened, and how often?" },
    CaseType { id: "property_damage", group: "public_safety", label: "Property damage", prompt: "What was damaged, and where?" },
    CaseType { id: "traffic_incident", group: "public_safety", label: "Traffic incident", prompt: "Where did it happen, and who was involved?" },
    CaseType { id: "lost_property", group: "public_safety", label: "Lost property", prompt: "What was lost, and where was it last?" },
    CaseType { id: "threat", group: "public_safety", label: "Threat", prompt: "What was said, and when?" },
    CaseType { id: "domestic_incident", group: "public_safety", label: "Domestic incident", prompt: "What happened, and is anyone in danger now?" },
    CaseType { id: "cyber_offense", group: "public_safety", label: "Cyber offense", prompt: "Which account or device, and what changed?" },
    CaseType { id: "public_disturbance", group: "public_safety", label: "Public disturbance", prompt: "Where is it, and what is happening?" },
    CaseType { id: "witness_statement", group: "public_safety", label: "Witness statement", prompt: "What did you see, and when?" },
    CaseType { id: "refund", group: "business", label: "Refund", prompt: "Which order, and why should it be refunded?" },
    CaseType { id: "replacement", group: "business", label: "Replacement", prompt: "Which item should be replaced, and why?" },
    CaseType { id: "damaged_delivery", group: "business", label: "Damaged delivery", prompt: "What arrived damaged?" },
    CaseType { id: "late_shipment", group: "business", label: "Late shipment", prompt: "What is late, and when did you expect it?" },
    CaseType { id: "wrong_item", group: "business", label: "Wrong item", prompt: "What did you receive instead?" },
    CaseType { id: "missing_part", group: "business", label: "Missing part", prompt: "Which part is missing?" },
    CaseType { id: "billing_dispute", group: "business", label: "Billing dispute", prompt: "Which charge is wrong?" },
    CaseType { id: "chargeback", group: "business", label: "Chargeback", prompt: "Which payment, and why is it disputed?" },
    CaseType { id: "cancellation", group: "business", label: "Cancellation", prompt: "What should be cancelled?" },
    CaseType { id: "warranty", group: "business", label: "Warranty", prompt: "What failed, and when did you buy it?" },
    CaseType { id: "service_outage", group: "business", label: "Service outage", prompt: "What stopped working, and when?" },
    CaseType { id: "account_access", group: "business", label: "Account access", prompt: "Which account, and what fails?" },
    CaseType { id: "contract_dispute", group: "business", label: "Contract dispute", prompt: "Which agreement, and what is disputed?" },
    CaseType { id: "overcharge", group: "business", label: "Overcharge", prompt: "What were you charged, and what did you expect?" },
    CaseType { id: "order_not_received", group: "business", label: "Order not received", prompt: "Which order never arrived?" },
    CaseType { id: "care_incident", group: "care", label: "Care incident", prompt: "What happened during care?" },
    CaseType { id: "missed_appointment", group: "care", label: "Missed appointment", prompt: "Which appointment, and what went wrong?" },
    CaseType { id: "record_correction", group: "care", label: "Record correction", prompt: "What in the record is wrong?" },
    CaseType { id: "medication_concern", group: "care", label: "Medication concern", prompt: "Which medicine, and what happened?" },
    CaseType { id: "service_billing", group: "care", label: "Service billing", prompt: "Which bill, and what looks wrong?" },
    CaseType { id: "facility_condition", group: "care", label: "Facility condition", prompt: "What in the place is unsafe or broken?" },
    CaseType { id: "discharge_concern", group: "care", label: "Discharge concern", prompt: "What was unclear at discharge?" },
    CaseType { id: "privacy_concern", group: "care", label: "Privacy concern", prompt: "What information was exposed?" },
    CaseType { id: "staff_conduct", group: "care", label: "Staff conduct", prompt: "What happened, and when?" },
    CaseType { id: "safety_incident", group: "care", label: "Safety incident", prompt: "What was unsafe, and who was affected?" },
];

fn key(name: &str) -> String {
    crate::accounts::setting_key(name)
}

fn account_slug() -> String {
    crate::accounts::current()
        .unwrap_or_else(|| "shared".into())
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

pub fn find(id: &str) -> Option<&'static CaseType> {
    TYPES.iter().find(|item| item.id == id)
}

pub fn is_enabled(app: &App, id: &str) -> AppResult<bool> {
    if app.db.setting(&key("catalog_knowledge"))? != "stored" {
        return Ok(false);
    }
    Ok(app.db.enabled_types()?.iter().any(|row| row.get("id").and_then(|v| v.as_str()) == Some(id)))
}

pub fn customer_view(app: &App) -> AppResult<Value> {
    let stored = app.db.setting(&key("catalog_knowledge"))? == "stored";
    let types = if stored { app.db.enabled_types()? } else { Vec::new() };
    Ok(json!({
        "knowledge_stored": stored,
        "types": types,
        "honesty": "Chips appear only after Connector stores them as agent knowledge. Knowledge is not permission.",
    }))
}

pub fn supervisor_view(app: &App) -> AppResult<Value> {
    let enabled = app.db.enabled_types()?;
    let on: Vec<String> = enabled
        .iter()
        .filter_map(|row| row.get("id").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    let catalog: Vec<Value> = TYPES
        .iter()
        .map(|item| {
            json!({
                "id": item.id,
                "group": item.group,
                "label": item.label,
                "prompt": item.prompt,
                "enabled": on.iter().any(|id| id == item.id),
            })
        })
        .collect();
    Ok(json!({
        "knowledge_stored": app.db.setting(&key("catalog_knowledge"))? == "stored",
        "catalog": catalog,
    }))
}

pub async fn publish(app: &App, ids: &[String]) -> AppResult<Value> {
    setup::require_real(app).await?;
    let mut rows = Vec::new();
    for id in ids {
        let Some(item) = find(id) else {
            return Err(AppError::bad(format!("unknown case type {id}")));
        };
        rows.push((
            item.id.to_string(),
            item.label.to_string(),
            item.group.to_string(),
            item.prompt.to_string(),
        ));
    }
    if rows.is_empty() {
        return Err(AppError::bad("choose at least one case type"));
    }
    let knowledge = rows
        .iter()
        .map(|(id, label, group, prompt)| format!("{group} / {label} ({id}): {prompt}"))
        .collect::<Vec<_>>()
        .join("\n");
    let pid = ensure_desk_agent(app).await?;
    let stored = app
        .connector()
        .setup_workspace(
            &pid,
            "desk-catalog",
            "Hold the case types this desk may offer. Knowledge is not permission and is not a directive.",
            &knowledge,
        )
        .await?;
    if let Some(error) = connector_error(&stored) {
        app.db.set_setting(&key("catalog_knowledge"), "absent")?;
        return Err(AppError::unavailable(format!(
            "connector_knowledge_refused: {error}"
        )));
    }
    let activated = app.connector().activate(&pid).await?;
    if let Some(error) = connector_error(&activated) {
        return Err(AppError::unavailable(format!(
            "connector did not activate the desk catalog agent: {error}"
        )));
    }
    app.db.replace_enabled(&rows)?;
    app.db.set_setting(&key("catalog_knowledge"), "stored")?;
    app.db.set_setting(&key("desk_agent_pid"), &pid)?;
    customer_view(app)
}

async fn ensure_desk_agent(app: &App) -> AppResult<String> {
    let existing = app.db.setting(&key("desk_agent_pid"))?;
    if !existing.is_empty() {
        let live = app.connector().get_agent(&existing).await?;
        if connector_error(&live).is_none()
            && live.get("pid").and_then(|v| v.as_str()) == Some(existing.as_str())
        {
            return Ok(existing);
        }
    }
    let registered = app
        .connector()
        .register_agent(
            "casepilot-desk-catalog",
            &format!("m/casepilot/desk/catalog-{}", account_slug()),
            "writer",
            "Hold the enabled case types for this desk. Do not grant remedies.",
            None,
        )
        .await?;
    if let Some(error) = connector_error(&registered) {
        if error.contains("namespace_in_use") {
            if let Some(pid) = existing_catalog_agent(app).await? {
                return Ok(pid);
            }
        }
        return Err(AppError::unavailable(format!(
            "connector refused the desk catalog agent: {error}"
        )));
    }
    registered
        .get("pid")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::unavailable("connector did not return a desk catalog agent id"))
}

async fn existing_catalog_agent(app: &App) -> AppResult<Option<String>> {
    let listed = app.connector().list_agents().await?;
    let Some(rows) = listed.get("agents").and_then(|value| value.as_array()) else {
        return Ok(None);
    };
    Ok(rows.iter().find_map(|row| {
        let namespace = row.get("namespace").and_then(|value| value.as_str()).unwrap_or("");
        if namespace == format!("m/casepilot/desk/catalog-{}", account_slug()) {
            row.get("pid").and_then(|value| value.as_str()).map(str::to_string)
        } else {
            None
        }
    }))
}

/// A person wrote before choosing a type. The report is stored first.
/// A type is not invented. The desk catalog agent is not used: its authority was revoked.
pub async fn answer_before_type(
    app: &App,
    customer_id: &str,
    message: &str,
    subject_ref: &str,
) -> AppResult<Value> {
    let message = message.trim();
    let customer_id = customer_id.trim();
    let subject_ref = subject_ref.trim();
    if message.is_empty() || customer_id.is_empty() {
        return Err(AppError::bad("a name and a message are required"));
    }
    let case_id = app.db.create_case(customer_id, message, "unspecified")?;
    let owner = crate::accounts::current().unwrap_or_default();
    let matter_key = format!(
        "{}_{}_{}_{}",
        slug(&owner),
        slug(customer_id),
        "unspecified",
        slug(subject_ref)
    );
    app.db.stamp_matter(&case_id, &matter_key, subject_ref)?;
    app.db.add_message(&case_id, customer_id, "customer", message)?;
    let knowledge = knowledge_text(app)?;
    let prompt = format!(
        "{knowledge}\n\nThe person filed this report and has not chosen a case type: {message}. The report is already stored. Answer as the person at this desk. Apologize briefly. Say the team is looking at it and the report is kept. Repeat their words. Do not invent a case type, a deadline, or a remedy. Do not talk about missing knowledge."
    );
    let answered = async {
        let mut pid = ensure_greeter(app).await?;
        match consult_desk(app, &pid, &prompt).await {
            Ok(reply) => Ok((pid, reply)),
            Err(err) if err.message.contains("authority_revoked") => {
                app.db.set_setting(&key("greeting_agent_pid"), "")?;
                app.db.set_setting(&key("desk_agent_session"), "")?;
                pid = ensure_greeter(app).await?;
                let reply = consult_desk(app, &pid, &prompt).await?;
                Ok((pid, reply))
            }
            Err(err) => Err(err),
        }
    }
    .await;
    let (pid, reply) = match answered {
        Ok(pair) => pair,
        Err(err) => {
            let _ = app.db.set_status(&case_id, "recorded");
            return Ok(json!({
                "text": "The report is kept. The team is looking at it.",
                "case": { "case_id": case_id },
                "honesty": err.message,
            }));
        }
    };
    let text = reply["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        let _ = app.db.set_status(&case_id, "recorded");
        return Ok(json!({
            "text": "The report is kept. The team is looking at it. A reply was not written yet.",
            "agent_pid": pid,
            "case": { "case_id": case_id },
            "honesty": "The report is stored. The desk agent did not write a reply.",
        }));
    }
    app.db.add_message(&case_id, customer_id, "customer_agent", &text)?;
    let _ = app.db.set_status(&case_id, "recorded");
    let suggestion = crate::engines::triage::suggest(app, &case_id).await.ok();
    Ok(json!({
        "text": text,
        "agent_pid": pid,
        "case": { "case_id": case_id },
        "suggestion": suggestion,
        "honesty": "The report is stored. No case type was invented. The worker draft waits for a person to confirm before the customer sees it.",
    }))
}

fn slug(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "none".into()
    } else {
        trimmed.to_string()
    }
}

async fn ensure_greeter(app: &App) -> AppResult<String> {
    let existing = app.db.setting(&key("greeting_agent_pid"))?;
    if !existing.is_empty() {
        let live = app.connector().get_agent(&existing).await?;
        let same = live.get("pid").and_then(|value| value.as_str()) == Some(existing.as_str());
        if connector_error(&live).is_none() && same {
            return Ok(existing);
        }
    }
    let stamp: String = crate::db::now()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(18)
        .collect();
    let namespace = format!("m/casepilot/desk/greeting-{stamp}");
    let purpose = "Answer in the report thread before a case type is chosen. Do not invent a type. Do not grant a remedy.";
    let registered = app
        .connector()
        .register_agent("cp-report-greeter", &namespace, "writer", purpose, None)
        .await?;
    if let Some(error) = connector_error(&registered) {
        return Err(AppError::unavailable(format!(
            "connector refused the report agent: {error}"
        )));
    }
    let pid = registered
        .get("pid")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::unavailable("connector did not return a report agent id"))?;
    let knowledge = knowledge_text(app)?;
    let setup = app
        .connector()
        .setup_workspace(&pid, "cp-report-greeter", purpose, &knowledge)
        .await?;
    if let Some(error) = connector_error(&setup) {
        return Err(AppError::unavailable(format!(
            "connector setup refused for the report agent: {error}"
        )));
    }
    let activated = app.connector().activate(&pid).await?;
    if let Some(error) = connector_error(&activated) {
        return Err(AppError::unavailable(format!(
            "connector did not activate the report agent: {error}"
        )));
    }
    app.db.set_setting(&key("greeting_agent_pid"), &pid)?;
    app.db.set_setting(&key("desk_agent_session"), "")?;
    Ok(pid)
}

async fn consult_desk(app: &App, pid: &str, prompt: &str) -> AppResult<Value> {
    let mut session = app.db.setting(&key("desk_agent_session"))?;
    if session.is_empty() {
        session = open_desk_session(app, pid).await?;
    }
    match crate::engines::talk::consult(app, pid, &session, "customer", prompt).await {
        Ok(reply) => Ok(reply),
        Err(_) => {
            app.db.set_setting(&key("desk_agent_session"), "")?;
            let session = open_desk_session(app, pid).await?;
            crate::engines::talk::consult(app, pid, &session, "customer", prompt).await
        }
    }
}

async fn open_desk_session(app: &App, pid: &str) -> AppResult<String> {
    let opened = app
        .connector()
        .open_workbench(pid, "report", "Answer before a case type is chosen")
        .await?;
    if let Some(error) = connector_error(&opened) {
        return Err(AppError::unavailable(format!(
            "connector refused the desk session: {error}"
        )));
    }
    let session = session_id_of(&opened).ok_or_else(|| {
        AppError::unavailable(format!(
            "connector desk session did not return a session_id: {opened}"
        ))
    })?;
    app.db.set_setting(&key("desk_agent_session"), &session)?;
    Ok(session)
}

pub fn type_rule(app: &App, type_id: &str) -> AppResult<String> {
    let types = app.db.enabled_types()?;
    for row in &types {
        let id = row.get("id").and_then(|value| value.as_str()).unwrap_or("");
        if id != type_id {
            continue;
        }
        let label = row.get("label").and_then(|value| value.as_str()).unwrap_or(type_id);
        let prompt = row.get("prompt").and_then(|value| value.as_str()).unwrap_or("");
        return Ok(format!(
            "Connector knowledge for this report ({label}): {prompt} Knowledge is not permission. Do not promise a refund, a replacement, or a finding."
        ));
    }
    Ok("This type is not in the stored Connector knowledge. Do not invent a rule. Do not promise a remedy.".into())
}

pub fn knowledge_text(app: &App) -> AppResult<String> {
    let types = app.db.enabled_types()?;
    if types.is_empty() {
        return Ok("No case types are stored in Connector knowledge.".into());
    }
    let lines = types
        .iter()
        .filter_map(|row| {
            let label = row.get("label")?.as_str()?;
            let prompt = row.get("prompt")?.as_str()?;
            Some(format!("{label}: {prompt}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "Enabled case types from Connector knowledge. These are not permission.\n{lines}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_forty_types() {
        assert_eq!(TYPES.len(), 40);
    }
}
