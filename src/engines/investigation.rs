use serde_json::{json, Value};

use crate::app::App;
use crate::connector::{classify_admission, Admission};
use crate::engines::coordination;
use crate::engines::talk;
use crate::error::AppResult;

pub async fn investigate(app: &App, case_id: &str) -> AppResult<Value> {
    coordination::ensure_open(app, case_id).await?;
    let case = app.db.get_case(case_id)?;
    app.db.set_status(case_id, "in_progress")?;
    let worker = coordination::require_bound(app, case_id, case.generation, "worker")?;
    let pid = worker.pid.clone();
    coordination::confirm_agent(app, &pid, "worker").await?;

    let memory = app
        .connector()
        .write_memory(
            &pid,
            case_id,
            &format!("case {} customer {} :: {}", case.case_id, case.customer_id, case.summary),
        )
        .await?;

    let order_id = app.db.subject_ref(case_id)?;
    let read = if order_id.is_empty() {
        json!({ "error": "this report has no order reference" })
    } else {
        app.connector()
            .invoke_tool(
                &pid,
                "read_order",
                json!({
                    "case_id": case_id,
                    "order_id": order_id,
                    "customer_id": case.customer_id,
                    "effect_type": "read_order",
                }),
                &format!("read:{case_id}:{order_id}"),
            )
            .await?
    };
    let admission = classify_admission(&read);
    let evidence_body = match &admission {
        Admission::Proceed => match app.db.read_order(&order_id) {
            Ok(order) => order.to_string(),
            Err(err) => format!("order {order_id} is not in the desk world: {}", err.message),
        },
        Admission::Ask => "connector asked before the order read; order body withheld".into(),
        Admission::Denied { reason } => format!("order read denied: {reason}"),
    };
    let evidence_id = app.db.add_evidence(
        case_id,
        "governed_order_read",
        "worker",
        &evidence_body,
    )?;

    let talk_message = format!(
        "Case {}. Customer said: {}. Admitted evidence: {evidence_body}",
        case.case_id, case.summary
    );
    let talk = match talk::consult(app, &pid, &worker.session_id, "worker", &talk_message).await {
        Ok(reply) => {
            let text = reply["text"].as_str().unwrap_or("");
            app.db.add_evidence(
                case_id,
                "connector_talk",
                "worker",
                &format!("model text, not a business effect: {text}"),
            )?;
            json!({ "ok": true, "reply": reply })
        }
        Err(err) => json!({ "ok": false, "error": err.message }),
    };

    // Research is finished. The report goes back to the person who releases the draft.
    app.db.set_status(case_id, "awaiting_human")?;
    Ok(json!({
        "case_id": case_id,
        "evidence_id": evidence_id,
        "read_admitted": matches!(admission, Admission::Proceed),
        "memory": memory,
        "talk": talk,
    }))
}
