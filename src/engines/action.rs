use serde_json::{json, Value};

use crate::app::App;
use crate::connector::{classify_admission, extract_hitl_id, extract_string, Admission};
use crate::db::{self, refund_scope, ActionRow};
use crate::engines::coordination;
use crate::error::{AppError, AppResult};

pub async fn propose(
    app: &App,
    case_id: &str,
    effect_type: &str,
    order_id: &str,
    amount_cents: Option<i64>,
) -> AppResult<Value> {
    coordination::ensure_open(app, case_id).await?;
    let case = app.db.get_case(case_id)?;
    if effect_type != "replacement" && effect_type != "refund" {
        return Err(AppError::bad("effect_type must be replacement or refund"));
    }
    if order_id.trim().is_empty() {
        return Err(AppError::bad("order_id is required"));
    }
    if effect_type == "refund" && amount_cents.unwrap_or(0) <= 0 {
        return Err(AppError::bad("refund amount_cents must be positive"));
    }
    let resolution = app
        .db
        .assignment(case_id, case.generation, "worker")?
        .ok_or_else(|| AppError::unavailable("worker agent is not bound"))?;
    let pid = resolution
        .connector_pid
        .clone()
        .ok_or_else(|| AppError::unavailable("worker agent has no Connector pid"))?;
    let session_id = resolution
        .session_id
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::unavailable("worker agent has no Connector workbench session"))?;
    coordination::confirm_agent(app, &pid, "worker").await?;

    // Registering the bridge as the case agent is a memory write on a
    // namespace that agent does not own, and Connector quarantines it.
    // The node bridge `casepilot` is registered once, outside the case agent.
    let action = app.db.create_action(
        case_id,
        case.coordination_task_id.as_deref(),
        effect_type,
        amount_cents,
        Some(order_id),
        case.generation,
    )?;
    if effect_type == "refund" && amount_cents.unwrap_or(0) > app.refund_auto_limit_cents {
        let scope = refund_scope(&action.action_id, amount_cents.unwrap_or(0));
        app.db.create_approval(case_id, &action.action_id, &scope, None)?;
    }
    let nonce = app.db.nonce_for(&action.action_id)?;
    let tool = match effect_type {
        "replacement" => "create_replacement",
        _ => "create_refund",
    };
    let invoked = app
        .connector()
        .invoke_tool(
            &pid,
            tool,
            json!({
                "case_id": case_id,
                "order_id": order_id,
                "amount_cents": amount_cents,
                "effect_type": effect_type,
                "dispatch_nonce": nonce,
                "action_id": action.action_id,
            }),
            &action.idempotency_key,
        )
        .await?;
    if let Some(hitl) = extract_hitl_id(&invoked) {
        if let Some(pending) = app.db.pending_approval_for_action(&action.action_id)? {
            app.db.set_hitl_id(&pending.approval_id, &hitl)?;
        }
    }
    let settled = settle(app, &action, &invoked)?;
    attach_receipt(app, &pid, &action.action_id).await;
    Ok(json!({
        "result": settled.result,
        "reason": settled.reason,
        "verified_status": settled.verified_status,
        "observed": settled.observed,
        "action_id": action.action_id,
        "bridge_registration": "node bridge casepilot; not registered as the case agent",
        "connector": invoked,
        "agent_pid": pid,
        "session_id": session_id,
    }))
}

pub async fn readmit(app: &App, action_id: &str) -> AppResult<Value> {
    let action = app.db.get_action(action_id)?;
    let case = app.db.get_case(&action.case_id)?;
    if case.status == "ceased" || case.status == "cease_incomplete" {
        return Err(AppError::forbidden("ceased case cannot be readmitted"));
    }
    if action.generation != case.generation {
        return Err(AppError::forbidden("stale generation cannot be readmitted"));
    }
    let resolution = app
        .db
        .assignment(&case.case_id, case.generation, "worker")?
        .ok_or_else(|| AppError::unavailable("worker agent is not bound"))?;
    let pid = resolution
        .connector_pid
        .clone()
        .ok_or_else(|| AppError::unavailable("worker agent has no Connector pid"))?;
    coordination::confirm_agent(app, &pid, "worker").await?;
    let nonce = app.db.nonce_for(action_id)?;
    let tool = match action.effect_type.as_str() {
        "replacement" => "create_replacement",
        "refund" => "create_refund",
        other => return Err(AppError::bad(format!("unsupported effect {other}"))),
    };
    let invoked = app
        .connector()
        .invoke_tool(
            &pid,
            tool,
            json!({
                "case_id": action.case_id,
                "order_id": action.order_id,
                "amount_cents": action.amount_cents,
                "effect_type": action.effect_type,
                "dispatch_nonce": nonce,
                "action_id": action.action_id,
                "readmit": true,
            }),
            &format!("{}:readmit", action.idempotency_key),
        )
        .await?;
    let settled = settle(app, &action, &invoked)?;
    attach_receipt(app, &pid, action_id).await;
    Ok(json!({
        "result": settled.result,
        "reason": settled.reason,
        "verified_status": settled.verified_status,
        "observed": settled.observed,
        "connector": invoked,
    }))
}

async fn attach_receipt(app: &App, agent_pid: &str, action_id: &str) {
    let Ok(body) = app.connector().receipts(agent_pid).await else {
        return;
    };
    let Some(receipt_id) = find_receipt_id(&body) else {
        return;
    };
    let _ = app.db.attach_receipt(action_id, &receipt_id);
}

fn find_receipt_id(value: &Value) -> Option<String> {
    if let Some(id) = value.get("receipt_id").and_then(|item| item.as_str()) {
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    match value {
        Value::Array(items) => items.iter().find_map(find_receipt_id),
        Value::Object(map) => map.values().find_map(find_receipt_id),
        _ => None,
    }
}

struct Settled {
    result: String,
    reason: String,
    verified_status: Option<String>,
    observed: Option<Value>,
}

fn settle(app: &App, action: &ActionRow, connector_body: &Value) -> AppResult<Settled> {
    let case = app.db.get_case(&action.case_id)?;
    if case.status == "ceased" || case.status == "cease_incomplete" || action.generation != case.generation {
        app.db.set_action_status(
            &action.action_id,
            "denied",
            &json!({"reason": "stale_or_ceased", "connector": connector_body}),
        )?;
        return Ok(Settled {
            result: "denied".into(),
            reason: "stale_or_ceased".into(),
            verified_status: None,
            observed: None,
        });
    }

    if let Some(existing) = app.db.observed_effect(action)? {
        return Ok(Settled {
            result: "admitted".into(),
            reason: "already_observed".into(),
            verified_status: Some("verified".into()),
            observed: Some(existing),
        });
    }

    match classify_admission(connector_body) {
        Admission::Ask => {
            ensure_pending_refund_approval(app, action)?;
            app.db.set_action_status(&action.action_id, "approval_required", connector_body)?;
            Ok(Settled {
                result: "approval_required".into(),
                reason: "connector_asked".into(),
                verified_status: None,
                observed: None,
            })
        }
        Admission::Denied { reason } => {
            app.db.set_action_status(&action.action_id, "denied", connector_body)?;
            Ok(Settled {
                result: "denied".into(),
                reason,
                verified_status: None,
                observed: None,
            })
        }
        Admission::Proceed => {
            if refund_needs_human(app, action)? {
                ensure_pending_refund_approval(app, action)?;
                app.db.set_action_status(
                    &action.action_id,
                    "approval_required",
                    &json!({
                        "reason": "human_approval_required",
                        "connector": connector_body,
                    }),
                )?;
                return Ok(Settled {
                    result: "approval_required".into(),
                    reason: "human_approval_required".into(),
                    verified_status: None,
                    observed: None,
                });
            }
            apply_and_verify(app, action, connector_body)
        }
    }
}

fn refund_needs_human(app: &App, action: &ActionRow) -> AppResult<bool> {
    if action.effect_type != "refund" {
        return Ok(false);
    }
    let amount = action.amount_cents.unwrap_or(0);
    if amount <= app.refund_auto_limit_cents {
        return Ok(false);
    }
    let scope = refund_scope(&action.action_id, amount);
    Ok(app.db.valid_approval(&action.action_id, &scope)?.is_none())
}

fn ensure_pending_refund_approval(app: &App, action: &ActionRow) -> AppResult<()> {
    if action.effect_type != "refund" {
        return Ok(());
    }
    if app.db.pending_approval_for_action(&action.action_id)?.is_some() {
        return Ok(());
    }
    let amount = action.amount_cents.unwrap_or(0);
    app.db.create_approval(
        &action.case_id,
        &action.action_id,
        &refund_scope(&action.action_id, amount),
        None,
    )?;
    Ok(())
}

fn apply_and_verify(app: &App, action: &ActionRow, connector_body: &Value) -> AppResult<Settled> {
    let order_id = action
        .order_id
        .clone()
        .ok_or_else(|| AppError::bad("action has no order_id"))?;
    let observed = match action.effect_type.as_str() {
        "replacement" => app.db.apply_replacement(&action.case_id, &action.action_id, &order_id)?,
        "refund" => {
            let amount = action.amount_cents.unwrap_or(0);
            app.db
                .apply_refund(&action.case_id, &action.action_id, &order_id, amount)?
        }
        other => return Err(AppError::bad(format!("unsupported effect {other}"))),
    };
    let confirmed = app.db.observed_effect(action)?;
    let task_id = extract_string(connector_body, &["pate_task_id", "task_id"]);
    let receipt_id = extract_string(connector_body, &["receipt_id"]);
    let verified_status = if confirmed.is_some() && (task_id.is_some() || receipt_id.is_some()) {
        "verified"
    } else if confirmed.is_some() {
        "observed_without_connector_evidence"
    } else {
        "mismatch"
    };
    app.db.save_result(
        &action.action_id,
        &observed.to_string(),
        receipt_id.as_deref(),
        task_id.as_deref(),
    )?;
    app.db.set_action_status(&action.action_id, verified_status, connector_body)?;
    let refs = json!({
        "action_id": action.action_id,
        "receipt_id": receipt_id,
        "connector_task_id": task_id,
    })
    .to_string();
    app.db.upsert_outcome(&action.case_id, verified_status, &refs)?;
    if verified_status == "verified" {
        app.db.set_status(&action.case_id, "in_progress")?;
    }
    Ok(Settled {
        result: "admitted".into(),
        reason: verified_status.into(),
        verified_status: Some(verified_status.into()),
        observed: Some(observed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ConnectorClient;
    use serde_json::json;
    use std::sync::{Arc, RwLock};

    fn test_app() -> App {
        App {
            db: db::Db::open(":memory:").expect("db"),
            connector_slot: Arc::new(RwLock::new(ConnectorClient::new("http://127.0.0.1:9", ""))),
            model_slot: Arc::new(RwLock::new(String::new())),
            refund_auto_limit_cents: 5_000,
            public_base: "http://127.0.0.1:8088".into(),
            setup_path: std::path::PathBuf::from("/tmp/casepilot-test-setup.json"),
            bundle_tarball: std::path::PathBuf::from("/tmp/casepilot-missing-bundle.tar.gz"),
        }
    }

    fn open_action(app: &App, effect: &str, amount: Option<i64>) -> (String, ActionRow) {
        let case_id = app.db.create_case("cust_amina", "damaged headphones", "damage").unwrap();
        app.db.set_status(&case_id, "open").unwrap();
        let action = app
            .db
            .create_action(&case_id, None, effect, amount, Some("ord_4401"), 1)
            .unwrap();
        (case_id, action)
    }

    #[test]
    fn replacement_proceeds_only_with_connector_evidence() {
        let app = test_app();
        let (_case_id, action) = open_action(&app, "replacement", None);
        let settled = settle(
            &app,
            &action,
            &json!({"outcome": "Success", "pate_task_id": "task_1"}),
        )
        .unwrap();
        assert_eq!(settled.result, "admitted");
        assert_eq!(settled.verified_status.as_deref(), Some("verified"));
        assert_eq!(app.db.replacement_count().unwrap(), 1);
    }

    #[test]
    fn over_limit_refund_does_not_write_on_connector_success() {
        let app = test_app();
        let (_case_id, action) = open_action(&app, "refund", Some(12_900));
        let settled = settle(
            &app,
            &action,
            &json!({"outcome": "Success", "pate_task_id": "task_2"}),
        )
        .unwrap();
        assert_eq!(settled.result, "approval_required");
        assert_eq!(app.db.refund_count().unwrap(), 0);
    }

    #[test]
    fn scoped_approval_then_proceed_writes_refund() {
        let app = test_app();
        let (case_id, action) = open_action(&app, "refund", Some(12_900));
        let scope = refund_scope(&action.action_id, 12_900);
        let approval_id = app.db.create_approval(&case_id, &action.action_id, &scope, None).unwrap();
        app.db
            .mark_approval(&approval_id, "approved", "supervisor.lee", Some(&db::plus_minutes(15)))
            .unwrap();
        let settled = settle(
            &app,
            &action,
            &json!({"outcome": "Success", "pate_task_id": "task_3"}),
        )
        .unwrap();
        assert_eq!(settled.verified_status.as_deref(), Some("verified"));
        assert_eq!(app.db.refund_count().unwrap(), 1);
    }

    #[test]
    fn ceased_case_does_not_write() {
        let app = test_app();
        let (case_id, action) = open_action(&app, "replacement", None);
        app.db.set_status(&case_id, "ceased").unwrap();
        let settled = settle(
            &app,
            &action,
            &json!({"outcome": "Success", "pate_task_id": "task_4"}),
        )
        .unwrap();
        assert_eq!(settled.result, "denied");
        assert_eq!(app.db.replacement_count().unwrap(), 0);
    }

    #[test]
    fn unrecognized_connector_body_does_not_write() {
        let app = test_app();
        let (_case_id, action) = open_action(&app, "replacement", None);
        let settled = settle(&app, &action, &json!({"ok": true})).unwrap();
        assert_eq!(settled.result, "denied");
        assert_eq!(app.db.replacement_count().unwrap(), 0);
    }
}
