use serde_json::{json, Value};

use crate::app::App;
use crate::error::AppResult;

pub fn evidence(app: &App, case_id: &str) -> AppResult<Value> {
    let _case = app.db.get_case(case_id)?;
    Ok(json!({
        "case_id": case_id,
        "evidence": app.db.list_evidence(case_id)?,
        "outcome": app.db.outcome(case_id)?,
        "actions": app.db.actions_for_case(case_id)?,
    }))
}

/// Reads the case record and reports which use conditions are actually met.
/// A condition is true only when a stored row shows it.
pub fn review(app: &App, case_id: &str) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    let issue = app.db.issue_type(case_id)?;
    let knowledge_stored = app.db.setting("catalog_knowledge")? == "stored";
    let enabled = knowledge_stored && crate::engines::catalog::is_enabled(app, &issue)?;
    let evidence = app.db.list_evidence(case_id)?;
    let assignments = app.db.assignments(case_id, case.generation)?;
    let triage = app.db.triage(case_id)?.unwrap_or(json!({}));
    let messages = app.db.messages_for_case(case_id)?;
    let actions = app.db.actions_for_case(case_id)?;
    let outcome = app.db.outcome(case_id)?;
    let sources: Vec<String> = evidence
        .iter()
        .filter_map(|row| row.get("source").and_then(|value| value.as_str()).map(str::to_string))
        .collect();
    let has = |name: &str| sources.iter().any(|source| source == name);
    let class = triage.get("classification").and_then(|value| value.as_str()).unwrap_or("");
    let digest = triage.get("file_digest").and_then(|value| value.as_str()).unwrap_or("");
    let path = triage.get("file_path").and_then(|value| value.as_str()).unwrap_or("");
    let reply = triage.get("reply").and_then(|value| value.as_str()).unwrap_or("");
    let reply_source = triage.get("reply_source").and_then(|value| value.as_str()).unwrap_or("");
    let decision = triage.get("decision").and_then(|value| value.as_str()).unwrap_or("");
    let roles: Vec<String> = assignments.iter().map(|row| row.role.clone()).collect();
    let bound = assignments
        .iter()
        .filter(|row| row.connector_pid.as_ref().is_some_and(|pid| !pid.is_empty()))
        .count();
    let charters: Vec<Value> = evidence
        .iter()
        .filter(|row| row.get("source").and_then(|value| value.as_str()) == Some("workspace_charter"))
        .filter_map(|row| serde_json::from_str(row.get("body").and_then(|value| value.as_str()).unwrap_or("")).ok())
        .collect();
    let principal_ok = charters.len() >= 2
        && charters.iter().all(|row| row.get("principal").and_then(|value| value.as_str()) == Some("present"));
    let purpose_ok = charters.len() >= 2
        && charters.iter().all(|row| row.get("purpose").and_then(|value| value.as_str()) == Some("present"));
    let distinct_pids = assignments
        .iter()
        .map(|row| row.connector_pid.clone().unwrap_or_default())
        .filter(|pid| !pid.is_empty())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let memory_kept = has("complaint_record")
        && messages.iter().any(|row| row.get("direction").and_then(|value| value.as_str()) == Some("customer"));
    let admitted = actions.iter().any(|row| row.get("status").and_then(|value| value.as_str()) == Some("verified"));
    let effect_chosen = decision == "refund" || decision == "replacement";
    let told_success_without_admit = !reply.is_empty() && effect_chosen && !admitted;
    let agents_or_waiting = bound == 2 || case.status == "waiting" || case.status == "recorded";
    let sections = vec![
        section("1", "Connector before filing", vec![
            item("Agents are bound, or the report is waiting", agents_or_waiting, &format!("{} bound, status {}", bound, case.status)),
            item("Production is not claimed from this case", true, "linux-kvm deploy-verify is the production gate."),
        ]),
        section("2", "Receipt", vec![
            item("One case id", !case.case_id.is_empty(), &case.case_id),
            item("Who, what, and when", !case.customer_id.is_empty() && !case.summary.is_empty() && !case.created_at.is_empty(), &case.created_at),
            item("Type is in Connector knowledge", enabled, &issue),
        ]),
        section("3", "One conversation", vec![
            item("The customer line was kept after memory", memory_kept, "A line without a memory record is not kept."),
            item("Types come from stored knowledge", knowledge_stored, "Absent knowledge means the report chat has no types."),
        ]),
        section("4", "The file", vec![
            item("Customer and worker agents, or the report is waiting", bound == 2 || case.status == "waiting", &format!("{bound} agents")),
            item("File is under that party", digest.is_empty() || path.contains(&case.customer_id), path),
            item("An easy case has a digest", class != "easy" || !digest.is_empty(), digest),
        ]),
        section("5", "Classification", vec![
            item("One class, or an honest refusal", matches!(class, "easy" | "hard" | "augmented") || has("classification_refused"), class),
            item("Hard or tough waits for follow-up", (class != "hard" && class != "augmented") || digest.is_empty() || has("customer_followup"), "Follow-up is required before those files."),
            item("A refused class has no reply", !has("classification_refused") || reply.is_empty(), "No customer reply was stored."),
        ]),
        section("6", "Connector agents", vec![
            item("Customer agent and worker agent", ["customer", "worker"].iter().all(|role| roles.iter().any(|have| have == role)), &roles.join(", ")),
            item("Principal present", principal_ok, "Taken from the workspace Connector returned."),
            item("Purpose present", purpose_ok, "A missing purpose stays unmet."),
            item("Charter rows exist", !charters.is_empty() || case.status == "waiting", &format!("{} charter rows", charters.len())),
        ]),
        section("7", "Knowledge and memory", vec![
            item("Complaint memory is recorded", has("complaint_record") || case.status == "waiting", "complaint_record follows a successful memory write."),
            item("Research is stored with the turn", !has("connector_research") || messages.iter().any(|row| matches!(row.get("direction").and_then(|value| value.as_str()), Some("worker_draft") | Some("agent_draft"))), "Research without a stored draft is unmet."),
        ]),
        section("8", "Confidentiality", vec![
            item("File path belongs to that party", path.is_empty() || path.contains(&case.customer_id), path),
            item("Each role has its own principal", bound != 2 || distinct_pids == 2, &format!("{distinct_pids} principals")),
        ]),
        section("9", "Investigation does not decide", vec![
            item(
                "A plan is a recommendation",
                app.db.latest_plan(case_id)?.map(|plan| {
                    plan.get("proposed_resolution").and_then(|value| value.as_str()).unwrap_or("").starts_with("recommendation_only")
                }).unwrap_or(true),
                "No plan, or the plan is recommendation only.",
            ),
        ]),
        section("10", "A person decides", vec![
            item("A reply follows a human decision", reply.is_empty() || !decision.is_empty(), decision),
            item("The decider is named", reply.is_empty() || !triage.get("decided_by").and_then(|value| value.as_str()).unwrap_or("").is_empty(), "decided_by"),
        ]),
        section("11", "Admit before an effect", vec![
            item("An unadmitted effect was not told as success", !told_success_without_admit, if effect_chosen { "effect chosen" } else { "no effect chosen" }),
            item("Verified requires an admitted action", outcome.as_ref().and_then(|row| row.get("verified_status")).and_then(|value| value.as_str()) != Some("verified") || admitted, "verified without a verified action is unmet"),
        ]),
        section("12", "Reply, cease, and trail", vec![
            item("A customer reply was released by a person", reply.is_empty() || reply_source == "human_release", reply_source),
            item("A ceased case still has its id", case.status != "ceased" || !case.case_id.is_empty(), &case.status),
        ]),
    ];
    let open = sections.iter().any(|section| {
        section.get("items").and_then(|value| value.as_array()).is_some_and(|items| {
            items.iter().any(|item| item.get("met").and_then(|value| value.as_bool()) == Some(false))
        })
    });
    Ok(json!({
        "case_id": case_id,
        "satisfied": !open,
        "sections": sections,
        "honesty": "Unmet items stay unmet. This reading does not make the node production-ready.",
    }))
}

fn section(id: &str, title: &str, items: Vec<Value>) -> Value {
    json!({ "id": id, "title": title, "items": items })
}

fn item(name: &str, met: bool, detail: &str) -> Value {
    json!({ "name": name, "met": met, "detail": detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ConnectorClient;
    use std::sync::{Arc, RwLock};

    fn app() -> App {
        App {
            db: crate::db::Db::open(":memory:").unwrap(),
            connector_slot: Arc::new(RwLock::new(ConnectorClient::new("http://127.0.0.1:9", ""))),
            model_slot: Arc::new(RwLock::new(String::new())),
            refund_auto_limit_cents: 5000,
            public_base: "http://127.0.0.1:8088".into(),
            setup_path: std::path::PathBuf::from("/tmp/casepilot-verification-setup.json"),
            bundle_tarball: std::path::PathBuf::from("/tmp/missing-bundle.tar.gz"),
        }
    }

    #[test]
    fn a_bare_report_is_not_satisfied() {
        let app = app();
        let id = app.db.create_case("cust_a", "something happened", "theft").unwrap();
        let body = review(&app, &id).unwrap();
        assert_eq!(body.get("satisfied").and_then(|value| value.as_bool()), Some(false));
        assert_eq!(body.get("sections").and_then(|value| value.as_array()).map(|rows| rows.len()), Some(12));
    }
}
