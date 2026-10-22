use serde_json::{json, Value};

use crate::app::App;
use crate::connector::{connector_error, workbench_reply};
use crate::engines::coordination;
use crate::error::{AppError, AppResult};

pub fn role_instructions(role: &str) -> &'static str {
    match role {
        "customer" | "investigator" => {
            "You speak as the person at this desk. When the message contains a confirmed position, communicate that position and do not replace it with a generic holding line. Otherwise apologize briefly and say the team is looking. Repeat only the facts they gave. If stored knowledge names a time, say that time. If it does not, do not invent one. Do not invent a refund, a replacement, or a finding."
        }
        "worker" | "policy" | "resolution" => {
            "You are the worker agent for this one case. You can use the customer's words and the published handling rules. Draft the official reply, including what is still missing. Do not promise money, a finding, or a clinical outcome. The draft is not sent until a person releases it."
        }
        _ => "Answer only inside this case. Do not grant permission.",
    }
}

/// Ask the bound Connector agent through its workbench session.
/// Parameters are Connector's: `message` and `model`. CasePilot does not call a vendor.
pub async fn consult(
    app: &App,
    agent_pid: &str,
    session_id: &str,
    role: &str,
    user_message: &str,
) -> AppResult<Value> {
    if agent_pid.trim().is_empty() || session_id.trim().is_empty() {
        return Err(AppError::unavailable(
            "connector_agent_required: talk needs the registered pid and workbench session",
        ));
    }
    coordination::confirm_agent(app, agent_pid, role).await?;
    let account = crate::accounts::current().ok_or_else(|| {
        AppError::forbidden("Enter your email and link your own model before a reply can be asked.")
    })?;
    let permit = app.bind_model(&account).await?;
    let model = permit.model.clone();
    let message = format!("{}\n\n{user_message}", role_instructions(role));
    let body = app
        .connector()
        .workbench_turn(agent_pid, session_id, &message, &model)
        .await?;
    drop(permit);
    if let Some(error) = connector_error(&body) {
        return Err(AppError::unavailable(format!(
            "connector_turn_denied: {error}"
        )));
    }
    let text = workbench_reply(&body).ok_or_else(|| {
        AppError::unavailable(format!(
            "connector_turn_empty: the workbench turn returned no assistant event. Body: {body}"
        ))
    })?;
    Ok(json!({
        "text": text,
        "model": model,
        "agent_pid": agent_pid,
        "session_id": session_id,
        "parameters": { "message": message, "model": model },
        "grants_permission": false,
        "honesty": "Connector workbench turn. It does not admit a refund, replacement, or approval.",
    }))
}

pub async fn linked_model(app: &App) -> AppResult<String> {
    let status = app.connector().llm_status().await?;
    if let Some(model) = status
        .get("model")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(model.to_string());
    }
    if !app.model().trim().is_empty() {
        return Ok(app.model().trim().to_string());
    }
    Ok(String::new())
}

pub async fn lane(app: &App, case_id: &str) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    let status = app.connector().llm_status().await.ok();
    let model = linked_model(app).await.unwrap_or_default();
    let assignments = app.db.assignments(case_id, case.generation)?;
    let agents = assignments
        .iter()
        .map(|row| {
            json!({
                "role": row.role,
                "connector_pid": row.connector_pid,
                "session_id": row.session_id,
                "instructions": role_instructions(&row.role),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "case_id": case_id,
        "model": model,
        "llm": status,
        "gateway": format!("{}/v1", app.connector().base()),
        "completions": format!("{}/v1/chat/completions", app.connector().base()),
        "parameters": {
            "workbench_turn": {
                "message": "the case question plus the role instruction",
                "model": model
            },
            "tool_invoke": ["bridge_id", "tool", "agent_pid", "input", "idempotency_key", "mission_id"],
            "external_completions": {
                "model": model,
                "agent_pid": "the connector_pid of the role that should speak",
                "messages": [
                    {"role": "system", "content": "role instructions"},
                    {"role": "user", "content": "the case question"}
                ]
            }
        },
        "agents": agents,
        "external_agent": {
            "base_url": format!("{}/v1", app.connector().base()),
            "api_key": "the Connector API key, not the model vendor key",
            "required_field": "agent_pid",
        },
        "rules": [
            "Point an external agent at Connector /v1. Do not point it at the model vendor.",
            "Send agent_pid so the turn is bound to that CasePilot intelligence.",
            "Do not put a vendor API key in CasePilot or in the browser.",
            "Assistant text does not create a refund or replacement. Those go through CasePilot propose, then Connector tool admission.",
        ],
    }))
}
