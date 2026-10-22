use serde_json::{json, Value};

use crate::app::App;
use crate::error::AppError;

const SCHEMA: &str = include_str!("../deploy/surreal/custody.surql");

pub async fn search(q: &str) -> Value {
    let Some(url) = surreal_url() else {
        return json!({
            "present": false,
            "reason": "SurrealDB is not configured.",
        });
    };
    let q = q.trim();
    if q.is_empty() {
        return json!({ "present": true, "hits": [] });
    }
    let query = format!(
        "SELECT case_id, direction, body, at FROM utterance WHERE body @@ {} LIMIT 20;",
        surreal_string(q)
    );
    match sql(&url, &query).await {
        Ok(value) => {
            let hits = value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("result"))
                .cloned()
                .unwrap_or(value);
            json!({ "present": true, "hits": hits })
        }
        Err(err) => json!({ "present": false, "reason": err }),
    }
}

pub async fn boot() {
    let Some(url) = surreal_url() else {
        tracing::info!("SURREAL_URL is unset, so the custody graph is not installed");
        return;
    };
    match ensure_schema(&url).await {
        Ok(()) => tracing::info!("custody graph is installed"),
        Err(err) => tracing::error!("custody graph was not installed: {err}"),
    }
}

/// Rebuilds the custody graph for one case and returns the walk.
/// The working rows stay where the engines committed them. This graph is the
/// file a reviewer reads, and the effect table rejects any row whose verdict
/// is not proceed.
pub async fn seal(app: &App, case_id: &str) -> Value {
    let Some(url) = surreal_url() else {
        return json!({
            "present": false,
            "reason": "SurrealDB is not configured. The docker stack sets SURREAL_URL; without it there is no custody graph.",
        });
    };
    let snapshot = match snapshot(app, case_id) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            return json!({
                "present": false,
                "reason": err.message,
            });
        }
    };
    if let Err(err) = ensure_schema(&url).await {
        return json!({ "present": false, "reason": err });
    }
    let script = seal_script(&snapshot);
    if let Err(err) = sql(&url, &script).await {
        return json!({ "present": false, "reason": err, "refused_effect": !snapshot.effects.is_empty() && err_is_gate(&err) });
    }
    match sql(&url, &walk_query(&snapshot.case_id)).await {
        Ok(walk) => json!({
            "present": true,
            "case_id": snapshot.case_id,
            "effects_sealed": snapshot.effects.len(),
            "effects_refused": snapshot.refused,
            "gate": "An effect row is stored only with verdict proceed. The database event rejects every other insert.",
            "walk": walk,
            "chain": snapshot.chain(),
        }),
        Err(err) => json!({
            "present": false,
            "reason": err,
            "chain": snapshot.chain(),
        }),
    }
}

fn surreal_url() -> Option<String> {
    std::env::var("SURREAL_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("SURREAL_HOSTPORT")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty())
                .map(|hostport| format!("http://{hostport}"))
        })
}

fn err_is_gate(err: &str) -> bool {
    err.to_ascii_lowercase().contains("proceed")
}

struct Snapshot {
    case_id: String,
    customer_id: String,
    matter_key: String,
    subject_ref: String,
    summary: String,
    status: String,
    at: String,
    utterances: Vec<Utterance>,
    charters: Vec<Charter>,
    decision: Option<Decision>,
    receipts: Vec<Receipt>,
    effects: Vec<Effect>,
    refused: usize,
}

struct Utterance {
    id: String,
    direction: String,
    body: String,
    at: String,
}

struct Charter {
    id: String,
    role: String,
    pid: String,
}

struct Decision {
    kind: String,
    note: String,
    by_whom: String,
}

struct Receipt {
    id: String,
    effect_type: String,
    verdict: String,
    task_id: String,
}

struct Effect {
    id: String,
    kind: String,
}

impl Snapshot {
    fn chain(&self) -> Value {
        let mut steps = vec![json!({
            "step": "filed",
            "who": self.customer_id,
            "text": self.summary,
        })];
        for row in &self.utterances {
            steps.push(json!({ "step": "said", "who": row.direction, "text": row.body }));
        }
        for row in &self.charters {
            steps.push(json!({ "step": "bound", "who": row.role, "text": row.pid }));
        }
        if let Some(decision) = &self.decision {
            steps.push(json!({ "step": "decided", "who": decision.by_whom, "text": decision.kind }));
        }
        for row in &self.receipts {
            steps.push(json!({ "step": "receipt", "who": row.verdict, "text": row.effect_type }));
        }
        for row in &self.effects {
            steps.push(json!({ "step": "effect", "who": "proceed", "text": row.kind }));
        }
        json!(steps)
    }
}

fn snapshot(app: &App, case_id: &str) -> Result<Snapshot, AppError> {
    let case = app.db.get_case(case_id)?;
    let messages = app.db.messages_for_case(case_id)?;
    let actions = app.db.actions_for_case(case_id)?;
    let triage = app.db.triage(case_id)?.unwrap_or(json!({}));
    let assignments = app.db.assignments(case_id, case.generation)?;
    let mut charters = Vec::new();
    for row in assignments {
        charters.push(Charter {
            id: row.assignment_id,
            role: row.role,
            pid: row.connector_pid.unwrap_or_default(),
        });
    }
    let decision_kind = triage.get("decision").and_then(|value| value.as_str()).unwrap_or("");
    let decision = if decision_kind.is_empty() {
        None
    } else {
        Some(Decision {
            kind: decision_kind.to_string(),
            note: triage.get("decision_note").and_then(|value| value.as_str()).unwrap_or("").to_string(),
            by_whom: triage.get("decided_by").and_then(|value| value.as_str()).unwrap_or("").to_string(),
        })
    };
    let mut receipts = Vec::new();
    let mut effects = Vec::new();
    let mut refused = 0;
    for row in actions {
        let id = row.get("action_id").and_then(|value| value.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let status = row.get("status").and_then(|value| value.as_str()).unwrap_or("");
        let verdict = if status == "verified" { "proceed" } else { "denied" };
        if status != "verified" {
            refused += 1;
        }
        receipts.push(Receipt {
            id: id.clone(),
            effect_type: row.get("effect_type").and_then(|value| value.as_str()).unwrap_or("").to_string(),
            verdict: verdict.to_string(),
            task_id: row.get("connector_task_id").and_then(|value| value.as_str()).unwrap_or("").to_string(),
        });
        if verdict == "proceed" {
            effects.push(Effect {
                id,
                kind: row.get("effect_type").and_then(|value| value.as_str()).unwrap_or("").to_string(),
            });
        }
    }
    let matter_key = app.db.matter_key(&case.case_id).unwrap_or_default();
    let subject_ref = app.db.subject_ref(&case.case_id).unwrap_or_default();
    Ok(Snapshot {
        case_id: case.case_id,
        customer_id: case.customer_id,
        matter_key,
        subject_ref,
        summary: case.summary,
        status: case.status,
        at: case.created_at,
        utterances: messages
            .into_iter()
            .filter_map(|row| {
                let id = row.get("message_id")?.as_str()?.to_string();
                Some(Utterance {
                    id,
                    direction: row.get("direction").and_then(|value| value.as_str()).unwrap_or("").to_string(),
                    body: row.get("body").and_then(|value| value.as_str()).unwrap_or("").to_string(),
                    at: row.get("created_at").and_then(|value| value.as_str()).unwrap_or("").to_string(),
                })
            })
            .collect(),
        charters,
        decision,
        receipts,
        effects,
        refused,
    })
}

fn seal_script(snapshot: &Snapshot) -> String {
    let case_id = record("case_file", &snapshot.case_id);
    let customer = record("customer", &snapshot.customer_id);
    let mut lines = vec![
        format!("DELETE {case_id};"),
        format!("DELETE {customer};"),
        format!(
            "CREATE {customer} SET customer_id = {}, name = {};",
            surreal_string(&snapshot.customer_id),
            surreal_string(&snapshot.customer_id)
        ),
        format!(
            "CREATE {case_id} SET case_id = {}, summary = {}, status = {}, at = {};",
            surreal_string(&snapshot.case_id),
            surreal_string(&snapshot.summary),
            surreal_string(&snapshot.status),
            surreal_string(&snapshot.at)
        ),
        format!(
            "UPDATE {case_id} SET party_id = {}, subject_ref = {}, matter_key = {};",
            surreal_string(&snapshot.customer_id),
            surreal_string(&snapshot.subject_ref),
            surreal_string(&snapshot.matter_key)
        ),
        format!("RELATE {customer}->filed->{case_id} SET at = {};", surreal_string(&snapshot.at)),
    ];
    if !snapshot.matter_key.is_empty() {
        let matter = record("matter", &snapshot.matter_key);
        lines.push(format!("DELETE {matter};"));
        lines.push(format!(
            "CREATE {matter} SET matter_key = {}, party_id = {}, subject_ref = {};",
            surreal_string(&snapshot.matter_key),
            surreal_string(&snapshot.customer_id),
            surreal_string(&snapshot.subject_ref)
        ));
        lines.push(format!("RELATE {customer}->has->{matter};"));
        lines.push(format!("RELATE {matter}->opened->{case_id};"));
    }
    for row in &snapshot.utterances {
        let id = record("utterance", &row.id);
        lines.push(format!("DELETE {id};"));
        lines.push(format!(
            "CREATE {id} SET case_id = {}, direction = {}, body = {}, at = {};",
            surreal_string(&snapshot.case_id),
            surreal_string(&row.direction),
            surreal_string(&row.body),
            surreal_string(&row.at)
        ));
        lines.push(format!("RELATE {case_id}->said->{id};"));
    }
    for row in &snapshot.charters {
        let id = record("charter", &row.id);
        lines.push(format!("DELETE {id};"));
        lines.push(format!(
            "CREATE {id} SET case_id = {}, role = {}, pid = {};",
            surreal_string(&snapshot.case_id),
            surreal_string(&row.role),
            surreal_string(&row.pid)
        ));
        lines.push(format!("RELATE {case_id}->bound->{id};"));
    }
    if let Some(decision) = &snapshot.decision {
        let id = record("decision", &snapshot.case_id);
        lines.push(format!("DELETE {id};"));
        lines.push(format!(
            "CREATE {id} SET case_id = {}, kind = {}, note = {}, by_whom = {};",
            surreal_string(&snapshot.case_id),
            surreal_string(&decision.kind),
            surreal_string(&decision.note),
            surreal_string(&decision.by_whom)
        ));
        lines.push(format!("RELATE {case_id}->decided->{id};"));
        for receipt in &snapshot.receipts {
            lines.push(format!(
                "RELATE {id}->cites->{};",
                record("receipt", &receipt.id)
            ));
        }
    }
    for row in &snapshot.receipts {
        let id = record("receipt", &row.id);
        lines.push(format!("DELETE {id};"));
        lines.push(format!(
            "CREATE {id} SET case_id = {}, action_id = {}, effect_type = {}, verdict = {}, task_id = {};",
            surreal_string(&snapshot.case_id),
            surreal_string(&row.id),
            surreal_string(&row.effect_type),
            surreal_string(&row.verdict),
            surreal_string(&row.task_id)
        ));
        lines.push(format!("RELATE {case_id}->admitted->{id};"));
    }
    for row in &snapshot.effects {
        let id = record("effect", &row.id);
        let receipt = record("receipt", &row.id);
        lines.push(format!("DELETE {id};"));
        lines.push(format!(
            "CREATE {id} SET case_id = {}, action_id = {}, kind = {}, verdict = 'proceed', receipt_id = {};",
            surreal_string(&snapshot.case_id),
            surreal_string(&row.id),
            surreal_string(&row.kind),
            surreal_string(&row.id)
        ));
        lines.push(format!("RELATE {id}->follows->{receipt};"));
    }
    lines.join("\n")
}

fn walk_query(case_id: &str) -> String {
    format!(
        "SELECT case_id, status, summary, <-filed<-customer.customer_id AS customer, ->said->utterance.{{direction, body, at}} AS said, ->bound->charter.{{role, pid}} AS agents, ->decided->decision.{{kind, note, by_whom}} AS decision, ->admitted->receipt.{{verdict, effect_type, task_id}} AS receipts FROM {};",
        record("case_file", case_id)
    )
}

fn record(table: &str, id: &str) -> String {
    format!("{table}:{}", id.chars().filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect::<String>())
}

fn surreal_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\r', "")
        .replace('\n', "\\n");
    format!("'{escaped}'")
}

async fn ensure_schema(url: &str) -> Result<(), String> {
    sql_ns(url, None, None, "DEFINE NAMESPACE IF NOT EXISTS casepilot;").await?;
    sql_ns(url, Some("casepilot"), None, "DEFINE DATABASE IF NOT EXISTS custody;").await?;
    sql(url, SCHEMA).await?;
    Ok(())
}

async fn sql(url: &str, query: &str) -> Result<Value, String> {
    sql_ns(url, Some("casepilot"), Some("custody"), query).await
}

async fn sql_ns(url: &str, ns: Option<&str>, db: Option<&str>, query: &str) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|err| err.to_string())?;
    let mut request = client
        .post(format!("{url}/sql"))
        .basic_auth(
            std::env::var("SURREAL_USER").unwrap_or_else(|_| "root".into()),
            Some(std::env::var("SURREAL_PASS").unwrap_or_else(|_| "root".into())),
        )
        .header("Accept", "application/json")
        .body(query.to_string());
    if let Some(ns) = ns {
        request = request.header("surreal-ns", ns);
    }
    if let Some(db) = db {
        request = request.header("surreal-db", db);
    }
    let response = request.send().await.map_err(|err| format!("surreal_unreachable: {err}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("surreal_http_{}: {text}", status.as_u16()));
    }
    let value: Value = serde_json::from_str(&text).unwrap_or(json!({ "raw": text }));
    if let Some(rows) = value.as_array() {
        for row in rows {
            if row.get("status").and_then(|item| item.as_str()) == Some("ERR") {
                let detail = row.get("result").map(|item| item.to_string()).unwrap_or_else(|| row.to_string());
                return Err(detail);
            }
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{seal_script, surreal_string, Snapshot, Charter, Decision, Effect, Receipt, Utterance};

    fn bare() -> Snapshot {
        Snapshot {
            case_id: "case_1".into(),
            customer_id: "cust_a".into(),
            matter_key: "cust_a_refund_ord".into(),
            subject_ref: "ord".into(),
            summary: "headphones".into(),
            status: "open".into(),
            at: "t".into(),
            utterances: vec![Utterance {
                id: "msg_1".into(),
                direction: "customer".into(),
                body: "it's broken".into(),
                at: "t".into(),
            }],
            charters: vec![Charter { id: "asg_1".into(), role: "resolution".into(), pid: "pid_1".into() }],
            decision: Some(Decision { kind: "refund".into(), note: "no".into(), by_whom: "lee".into() }),
            receipts: vec![Receipt { id: "act_1".into(), effect_type: "refund".into(), verdict: "denied".into(), task_id: String::new() }],
            effects: vec![],
            refused: 1,
        }
    }

    #[test]
    fn a_denied_receipt_does_not_write_an_effect() {
        let script = seal_script(&bare());
        assert!(!script.contains("CREATE effect:"));
        assert!(script.contains("verdict = 'denied'"));
        assert!(script.contains("->said->"));
    }

    #[test]
    fn a_proceed_receipt_writes_the_effect_on_that_receipt() {
        let mut snapshot = bare();
        snapshot.receipts[0].verdict = "proceed".into();
        snapshot.effects.push(Effect { id: "act_1".into(), kind: "refund".into() });
        snapshot.refused = 0;
        let script = seal_script(&snapshot);
        assert!(script.contains("CREATE effect:act_1"));
        assert!(script.contains("verdict = 'proceed'"));
        assert!(script.contains("->follows->"));
    }

    #[test]
    fn quotes_in_a_customer_line_stay_inside_the_string() {
        assert_eq!(surreal_string("it's a \"file\""), "'it\\'s a \"file\"'");
    }
}
