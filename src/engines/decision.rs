use serde_json::{json, Value};

use crate::app::App;
use crate::db::{self, refund_scope};
use crate::engines::action;
use crate::engines::coordination;
use crate::engines::talk;
use crate::error::{AppError, AppResult};

pub async fn policy(app: &App, case_id: &str) -> AppResult<Value> {
    coordination::ensure_open(app, case_id).await?;
    let case = app.db.get_case(case_id)?;
    let analyst = coordination::require_bound(app, case_id, case.generation, "worker")?;
    let reply = talk::consult(
        app,
        &analyst.pid,
        &analyst.session_id,
        "worker",
        "Recommend a resolution for a damaged delivered order. State whether a human approval is likely. You cannot grant it.",
    )
    .await?;
    let text = reply["text"].as_str().unwrap_or("").to_string();
    let version = app
        .db
        .add_plan(case_id, &format!("recommendation_only: {text}"))?;
    Ok(json!({
        "case_id": case_id,
        "plan_version": version,
        "proposed_resolution": format!("recommendation_only: {text}"),
        "talk": reply,
        "grants_permission": false,
    }))
}

pub async fn approve(app: &App, approval_id: &str, principal: &str) -> AppResult<Value> {
    let principal = principal.trim();
    if principal.is_empty() {
        return Err(AppError::bad("principal is required"));
    }
    let approval = app.db.get_approval(approval_id)?;
    if approval.status != "pending" {
        return Err(AppError::bad("approval is not pending"));
    }
    let case = app.db.get_case(&approval.case_id)?;
    if case.status == "ceased" || case.status == "cease_incomplete" {
        return Err(AppError::forbidden("ceased case cannot be approved"));
    }
    if let Some(hitl) = approval.connector_hitl_id.as_deref() {
        if let Some(pid) = app
            .db
            .assignment(&case.case_id, case.generation, "worker")?
            .and_then(|row| row.connector_pid)
        {
            let _ = app
                .connector()
                .hitl_approve(&pid, hitl, &format!("casepilot supervisor {principal}"))
                .await?;
        }
    }
    let expiry = db::plus_minutes(15);
    app.db
        .mark_approval(approval_id, "approved", principal, Some(&expiry))?;
    let settled = action::readmit(app, &approval.action_id).await?;
    Ok(json!({
        "approval_id": approval_id,
        "principal": principal,
        "scope": approval.scope,
        "expiry": expiry,
        "settled": settled,
    }))
}

pub async fn deny(app: &App, approval_id: &str, principal: &str) -> AppResult<Value> {
    let principal = principal.trim();
    if principal.is_empty() {
        return Err(AppError::bad("principal is required"));
    }
    let approval = app.db.get_approval(approval_id)?;
    if approval.status != "pending" {
        return Err(AppError::bad("approval is not pending"));
    }
    if let Some(hitl) = approval.connector_hitl_id.as_deref() {
        let case = app.db.get_case(&approval.case_id)?;
        if let Some(pid) = app
            .db
            .assignment(&case.case_id, case.generation, "worker")?
            .and_then(|row| row.connector_pid)
        {
            let _ = app
                .connector()
                .hitl_deny(&pid, hitl, &format!("casepilot supervisor {principal}"))
                .await?;
        }
    }
    app.db.mark_approval(approval_id, "denied", principal, None)?;
    app.db.set_action_status(
        &approval.action_id,
        "denied",
        &json!({"reason": "supervisor_denied", "approval_id": approval_id}),
    )?;
    Ok(json!({
        "approval_id": approval_id,
        "result": "denied",
        "scope": refund_scope(&approval.action_id, 0),
    }))
}
