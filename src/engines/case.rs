use serde_json::{json, Value};

use crate::app::App;
use crate::db::{approval_json, case_json};
use crate::error::AppResult;
use crate::engines::catalog;
use crate::engines::coordination;
use crate::engines::triage;

pub async fn intake(
    app: &App,
    customer_id: &str,
    summary: &str,
    issue_type: &str,
    subject_ref: &str,
) -> AppResult<Value> {
    let customer_id = customer_id.trim();
    let summary = summary.trim();
    let issue_type = issue_type.trim();
    let subject_ref = subject_ref.trim();
    if customer_id.is_empty() || summary.is_empty() || issue_type.is_empty() {
        return Err(crate::error::AppError::bad(
            "customer_id, summary, and issue_type are required",
        ));
    }
    if !catalog::is_enabled(app, issue_type)? {
        return Err(crate::error::AppError::bad(
            "that case type is not in Connector knowledge. The supervisor must store it before anyone can file it.",
        ));
    }
    let owner = crate::accounts::current().unwrap_or_default();
    let matter_key = format!(
        "{}_{}_{}_{}",
        token(&owner),
        token(customer_id),
        token(issue_type),
        token(subject_ref)
    );
    if !subject_ref.is_empty() {
        if let Some(existing) = app.db.open_matter(&matter_key)? {
            let existing_case = app.db.get_case(&existing)?;
            if existing_case.status == "replied" {
                let next = existing_case.generation + 1;
                match coordination::bind(app, &existing, next).await {
                    Ok(_) => {
                        let continued = triage::add_info(app, &existing, summary).await;
                        let mut record = detail(app, &existing)?;
                        record["continued"] = json!(true);
                        if let Err(err) = continued {
                            record["continue_error"] = json!(err.message);
                        }
                        let _ = crate::custody::seal(app, &existing).await;
                        return Ok(record);
                    }
                    Err(err) => {
                        app.db.add_message(&existing, customer_id, "customer", summary)?;
                        let mut record = detail(app, &existing)?;
                        record["continued"] = json!(true);
                        record["continue_error"] = json!(err.message);
                        return Ok(record);
                    }
                }
            }
            if existing_case.status == "waiting" || existing_case.status == "recorded" {
                app.db.set_summary(&existing, summary)?;
                app.db.set_status(&existing, "intake_unbound")?;
                match coordination::bind(app, &existing, existing_case.generation).await {
                    Ok(binding) => {
                        let filed = triage::after_intake(app, &existing).await;
                        let mut record = detail(app, &existing)?;
                        record["coordination"] = json!({ "ok": true, "binding": binding });
                        record["continued"] = json!(true);
                        match filed {
                            Ok(triage) => {
                                record["triage"] = triage;
                                let _ = crate::custody::seal(app, &existing).await;
                            }
                            Err(err) => {
                                app.db.set_status(&existing, "recorded")?;
                                record["classification_error"] = json!(err.message);
                            }
                        }
                        return Ok(record);
                    }
                    Err(err) => {
                        app.db.set_status(&existing, "waiting")?;
                        app.db.add_message(&existing, customer_id, "customer", summary)?;
                        let mut record = detail(app, &existing)?;
                        record["continued"] = json!(true);
                        record["continue_error"] = json!(err.message);
                        record["honesty"] = json!("Connector did not register an agent for this report, so no model reply was stored.");
                        return Ok(record);
                    }
                }
            }
            let continued = triage::add_info(app, &existing, summary).await;
            let mut record = detail(app, &existing)?;
            record["continued"] = json!(true);
            if let Err(err) = continued {
                record["continue_error"] = json!(err.message);
            }
            let _ = crate::custody::seal(app, &existing).await;
            return Ok(record);
        }
    }
    let case_id = app.db.create_case(customer_id, summary, issue_type)?;
    app.db.stamp_matter(&case_id, &matter_key, subject_ref)?;
    match coordination::bind(app, &case_id, 1).await {
        Ok(binding) => {
            let filed = triage::after_intake(app, &case_id).await;
            let mut record = detail(app, &case_id)?;
            record["coordination"] = json!({ "ok": true, "binding": binding });
            match filed {
                Ok(triage) => {
                    record["triage"] = triage;
                    let _ = crate::custody::seal(app, &case_id).await;
                    Ok(record)
                }
                Err(err) => {
                    app.db.set_status(&case_id, "recorded")?;
                    let _ = app.db.add_evidence(&case_id, "classification_refused", "worker", &err.message);
                    record = detail(app, &case_id)?;
                    record["classification_error"] = json!(err.message);
                    record["honesty"] = json!("The report is kept. It was not classified and no customer reply was stored.");
                    Ok(record)
                }
            }
        }
        Err(err) => {
            let _ = app.db.add_message(&case_id, customer_id, "customer", summary);
            let _ = app.db.set_status(&case_id, "recorded");
            let mut record = detail(app, &case_id)?;
            record["honesty"] = json!(format!(
                "The report is kept. Connector has not registered agents for it yet. {}",
                err.message
            ));
            Ok(record)
        }
    }
}

pub fn detail(app: &App, case_id: &str) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    let assignments = app
        .db
        .assignments(&case.case_id, case.generation)?
        .into_iter()
        .map(|row| {
            json!({
                "role": row.role,
                "generation": row.generation,
                "connector_pid": row.connector_pid,
                "session_id": row.session_id,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "case": case_json(&case),
        "assignments": assignments,
        "actions": app.db.actions_for_case(case_id)?,
        "plan": app.db.latest_plan(case_id)?,
        "outcome": app.db.outcome(case_id)?,
        "triage": app.db.triage(case_id)?,
        "messages": app.db.messages_for_case(case_id)?,
        "timeline": timeline(app, case_id)?,
    }))
}

pub fn desk(app: &App, case_id: &str) -> AppResult<Value> {
    let detail = detail(app, case_id)?;
    let evidence = app.db.list_evidence(case_id)?;
    let research: Vec<Value> = evidence
        .iter()
        .filter(|row| {
            matches!(
                row.get("source").and_then(|value| value.as_str()),
                Some("connector_talk") | Some("connector_research") | Some("customer_reply")
            )
        })
        .cloned()
        .collect();
    let file = read_dossier(detail.get("triage"));
    Ok(json!({
        "detail": detail,
        "research": research,
        "filesystem": file,
        "evidence": evidence,
        "verification": crate::engines::verification::review(app, case_id)?,
    }))
}

fn token(value: &str) -> String {
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
    if cleaned.is_empty() {
        "none".into()
    } else {
        cleaned
    }
}

fn timeline(app: &App, case_id: &str) -> AppResult<Vec<Value>> {
    let mut events = Vec::new();
    let case = app.db.get_case(case_id)?;
    events.push(json!({
        "at": case.created_at,
        "title": "Complaint received",
        "detail": case.summary,
    }));
    for row in app.db.list_evidence(case_id)? {
        events.push(json!({
            "at": row.get("created_at").cloned().unwrap_or(Value::Null),
            "title": row.get("source").cloned().unwrap_or(json!("event")),
            "detail": row.get("body").cloned().unwrap_or(Value::Null),
        }));
    }
    Ok(events)
}

fn read_dossier(triage: Option<&Value>) -> Value {
    let Some(triage) = triage.filter(|value| !value.is_null()) else {
        return json!({ "present": false });
    };
    let path = triage.get("file_path").and_then(|value| value.as_str()).unwrap_or("");
    if path.is_empty() {
        return json!({ "present": false, "digest": triage.get("file_digest") });
    }
    match std::fs::read_to_string(path) {
        Ok(body) => json!({
            "present": true,
            "path": path,
            "digest": triage.get("file_digest"),
            "body": body,
        }),
        Err(err) => json!({
            "present": false,
            "path": path,
            "error": err.to_string(),
        }),
    }
}

pub fn list_for_customer(app: &App, customer_id: &str) -> AppResult<Value> {
    let cases = app
        .db
        .list_cases(Some(customer_id))?
        .into_iter()
        .map(|row| case_json(&row))
        .collect::<Vec<_>>();
    Ok(json!({ "cases": cases }))
}

pub fn queue(app: &App) -> AppResult<Value> {
    let cases = app
        .db
        .queue()?
        .into_iter()
        .map(|row| case_json(&row))
        .collect::<Vec<_>>();
    Ok(json!({ "cases": cases }))
}

pub fn approvals(app: &App) -> AppResult<Value> {
    let rows = app
        .db
        .list_approvals(Some("pending"))?
        .into_iter()
        .map(|row| approval_json(&row))
        .collect::<Vec<_>>();
    Ok(json!({ "approvals": rows }))
}
