use serde_json::{json, Value};

use crate::app::App;
use crate::engines::coordination;
use crate::error::{AppError, AppResult};

pub async fn cease(app: &App, case_id: &str) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    if case.status == "ceased" {
        return Ok(json!({ "case_id": case_id, "status": "ceased", "already": true }));
    }
    let assignments = app.db.assignments(case_id, case.generation)?;
    if assignments.is_empty() {
        return Err(AppError::bad("case has no Connector agents to cease"));
    }
    let mut results = Vec::new();
    let mut failed = false;
    for assignment in assignments {
        let Some(pid) = assignment.connector_pid else {
            failed = true;
            results.push(json!({"role": assignment.role, "error": "missing_pid"}));
            continue;
        };
        match app.connector().cease(&pid).await {
            Ok(body) if body.get("error").is_none() => {
                results.push(json!({"role": assignment.role, "connector_pid": pid, "body": body}));
            }
            Ok(body) => {
                failed = true;
                results.push(json!({"role": assignment.role, "connector_pid": pid, "body": body}));
            }
            Err(err) => {
                failed = true;
                results.push(json!({"role": assignment.role, "connector_pid": pid, "error": err.message}));
            }
        }
    }
    let status = if failed { "cease_incomplete" } else { "ceased" };
    app.db.set_status(case_id, status)?;
    Ok(json!({ "case_id": case_id, "status": status, "agents": results }))
}

pub async fn resume(app: &App, case_id: &str) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    if case.status != "ceased" {
        return Err(AppError::bad(
            "resume requires a ceased case; cease_incomplete must be ceased again first",
        ));
    }
    let next = case.generation + 1;
    let binding = coordination::bind(app, case_id, next).await?;
    Ok(json!({
        "case_id": case_id,
        "generation": next,
        "binding": binding,
    }))
}

pub async fn deployment(app: &App) -> AppResult<Value> {
    let health = match app.connector().health().await {
        Ok(body) => json!({ "reachable": true, "body": body }),
        Err(err) => json!({ "reachable": false, "error": err.message }),
    };
    let product_status = command_output("connectorctl", &["product", "status"]).await;
    let deploy_verify = command_output("connectorctl", &["govern", "deploy-verify", "linux-kvm"]).await;
    Ok(json!({
        "connector_base": app.connector().base(),
        "health": health,
        "product_status": product_status,
        "deploy_verify": deploy_verify,
        "disclosure": "CasePilot writes a refund or replacement only after Connector tool invoke returns Success or proceed, and only when a scoped unexpired approval exists for a refund above the limit. Connector MCP dispatch in this build admits in-process and does not POST the bridge URL. A model sentence is not a completed refund. Backends that are not READY in deploy-verify are unproven.",
    }))
}

async fn command_output(program: &str, args: &[&str]) -> Value {
    let output = tokio::process::Command::new(program).args(args).output().await;
    match output {
        Ok(output) => json!({
            "program": program,
            "args": args,
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }),
        Err(err) => json!({
            "program": program,
            "args": args,
            "error": err.to_string(),
            "disclosure": "connectorctl is not installed on this host",
        }),
    }
}
