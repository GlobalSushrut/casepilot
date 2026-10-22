use serde_json::{json, Value};

use crate::app::App;
use crate::connector::{connector_error, extract_string, session_id_of};
use crate::engines::talk;
use crate::error::{AppError, AppResult};

pub struct BoundAgent {
    pub pid: String,
    pub session_id: String,
}

pub fn require_bound(app: &App, case_id: &str, generation: i64, role: &str) -> AppResult<BoundAgent> {
    let row = app
        .db
        .assignment(case_id, generation, role)?
        .ok_or_else(|| AppError::unavailable(format!("{role} is not a Connector agent on this case")))?;
    let pid = row
        .connector_pid
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::unavailable(format!("{role} has no Connector pid")))?;
    let session_id = row
        .session_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::unavailable(format!("{role} has no Connector workbench session")))?;
    Ok(BoundAgent { pid, session_id })
}

const ROLES: &[(&str, &str, &str)] = &[
    (
        "customer",
        "writer",
        "Speak in this customer's thread for this case only. Name yourself, repeat the facts they gave, state the public next step, and say a person will answer here. Do not offer a remedy, a finding, or a refund.",
    ),
    (
        "worker",
        "writer",
        "Draft the official reply for this case from the facts the customer already gave and from how this desk writes a position. Do not post into the customer thread. Do not promise money, a finding, or a clinical outcome. A person releases the words.",
    ),
];

pub async fn bind(app: &App, case_id: &str, generation: i64) -> AppResult<Value> {
    let case = app.db.get_case(case_id)?;
    let model = talk::linked_model(app).await.unwrap_or_default();
    let model_ref = if model.is_empty() { None } else { Some(model.as_str()) };
    let mut bound = Vec::new();
    for (role, connector_role, purpose) in ROLES {
        if let Some(existing) = app.db.assignment(case_id, generation, role)? {
            let agent = require_bound(app, case_id, generation, role)?;
            confirm_live(app, &agent.pid, role).await?;
            bound.push(json!({
                "role": role,
                "connector_pid": agent.pid,
                "session_id": agent.session_id,
                "reused": true,
                "assignment_id": existing.assignment_id,
            }));
            continue;
        }
        let name = format!("cp-{role}-{}", &case_id[case_id.len().saturating_sub(12)..]);
        let namespace = format!("m/casepilot/{case_id}/{role}");
        let registered = app
            .connector()
            .register_agent(&name, &namespace, connector_role, purpose, model_ref)
            .await?;
        if let Some(error) = connector_error(&registered) {
            return Err(AppError::unavailable(format!(
                "connector refused to register {role}: {error}"
            )));
        }
        if registered.get("executed") == Some(&json!(false)) {
            return Err(AppError::unavailable(format!(
                "connector did not execute registration for {role}"
            )));
        }
        let pid = extract_string(&registered, &["pid", "agent_pid"]).ok_or_else(|| {
            AppError::unavailable(format!(
                "connector register {role} did not return an intelligence id: {registered}"
            ))
        })?;
        confirm_live(app, &pid, role).await?;
        let session = app
            .connector()
            .open_workbench(
                &pid,
                &format!("case {case_id} {role}"),
                &format!("Case {} generation {generation}. {purpose}", case.case_id),
            )
            .await?;
        if let Some(error) = connector_error(&session) {
            return Err(AppError::unavailable(format!(
                "connector refused the {role} workbench session: {error}"
            )));
        }
        let session_id = session_id_of(&session).ok_or_else(|| {
            AppError::unavailable(format!(
                "connector workbench for {role} did not return a session_id: {session}"
            ))
        })?;
        let task_id = extract_string(&session, &["task_id"]);
        let workspace = charter(app, &pid, role, purpose, case_id, task_id.as_deref()).await?;
        app.db.add_assignment(
            case_id,
            role,
            generation,
            &pid,
            Some(&session_id),
        )?;
        app.db.add_evidence(case_id, "workspace_charter", role, &workspace.to_string())?;
        bound.push(json!({
            "role": role,
            "connector_pid": pid,
            "session_id": session_id,
            "task_id": task_id,
            "connector_role": connector_role,
            "parameters": {
                "register": ["name", "namespace", "role", "purpose", "instructions", "model"],
                "turn": ["message", "model"],
                "invoke": ["bridge_id", "tool", "agent_pid", "input", "idempotency_key", "mission_id"],
            },
            "workspace": workspace,
        }));
    }
    let worker = app
        .db
        .assignment(case_id, generation, "worker")?
        .ok_or_else(|| AppError::unavailable("worker agent was not bound"))?;
    app.db.set_binding(
        case_id,
        generation,
        worker.session_id.as_deref(),
        worker.connector_pid.as_deref(),
        "open",
    )?;
    Ok(json!({ "generation": generation, "agents": bound }))
}

pub async fn confirm_agent(app: &App, pid: &str, role: &str) -> AppResult<()> {
    confirm_live(app, pid, role).await
}

async fn confirm_live(app: &App, pid: &str, role: &str) -> AppResult<()> {
    let live = app.connector().get_agent(pid).await?;
    if let Some(error) = connector_error(&live) {
        return Err(AppError::unavailable(format!(
            "{role} is not a live Connector agent: {error}"
        )));
    }
    let live_pid = extract_string(&live, &["pid", "agent_pid"]).unwrap_or_default();
    if live_pid != pid {
        return Err(AppError::unavailable(format!(
            "{role} lookup did not return the registered pid"
        )));
    }
    Ok(())
}

async fn charter(
    app: &App,
    pid: &str,
    role: &str,
    purpose: &str,
    case_id: &str,
    task_id: Option<&str>,
) -> AppResult<Value> {
    let catalog = crate::engines::catalog::knowledge_text(app).unwrap_or_default();
    let knowledge = if role == "worker" {
        format!(
            "Published types, which are what the customer agent already knows. Knowledge is not permission.\n{catalog}\n\nDeeper access for this case only: the customer's words in memory, the published next step, and how to draft a position. The draft names what is still missing. It does not promise money, a finding, or a clinical outcome. The draft is not sent until a person releases it."
        )
    } else {
        format!(
            "Public knowledge for the customer thread. Repeat only a published next step. Knowledge is not a remedy and not permission.\n{catalog}"
        )
    };
    let setup = app.connector().setup_workspace(pid, role, purpose, &knowledge).await?;
    if let Some(error) = connector_error(&setup) {
        return Err(AppError::unavailable(format!(
            "connector setup refused for {role}: {error}"
        )));
    }
    let stored = app.connector().get_contract(pid).await?;
    if let Some(error) = connector_error(&stored) {
        return Err(AppError::unavailable(format!(
            "connector contract was not readable for {role}: {error}"
        )));
    }
    let activated = app.connector().activate(pid).await?;
    if let Some(error) = connector_error(&activated) {
        return Err(AppError::unavailable(format!(
            "connector did not activate {role}: {error}"
        )));
    }
    let directive = if role == "worker" {
        "Draft the reply a person will release. You may use this case's memory and the published handling rules. You may not post to the customer or move money."
    } else {
        "Tell the customer who you are, what you heard, the public next step, and that a person will answer in this thread. Do not decide a remedy."
    };
    let directive_result = app
        .connector()
        .post_directive(pid, &format!("{role}-duty"), directive)
        .await?;
    if let Some(error) = connector_error(&directive_result) {
        return Err(AppError::unavailable(format!(
            "connector directive refused for {role}: {error}"
        )));
    }
    let situation = app.connector().post_situation(pid, task_id).await?;
    if let Some(error) = connector_error(&situation) {
        return Err(AppError::unavailable(format!(
            "connector situation refused for {role}: {error}"
        )));
    }
    let presence = app.connector().presence(pid).await?;
    if let Some(error) = connector_error(&presence) {
        return Err(AppError::unavailable(format!(
            "connector presence refused for {role}: {error}"
        )));
    }
    let projection = app.connector().workspace(pid).await?;
    if let Some(error) = connector_error(&projection) {
        return Err(AppError::unavailable(format!(
            "connector workspace refused for {role}: {error}"
        )));
    }
    let principal = dimension_status(&projection, "principal");
    let purpose_status = dimension_status(&projection, "purpose");
    if principal != "present" || purpose_status != "present" {
        return Err(AppError::unavailable(format!(
            "{role} is not a real Connector identity: principal={principal} purpose={purpose_status}"
        )));
    }
    Ok(json!({
        "principal": principal,
        "purpose": purpose_status,
        "presence": dimension_status(&projection, "presence"),
        "knowledge": dimension_status(&projection, "knowledge"),
        "directives": dimension_status(&projection, "directives"),
        "authority": dimension_status(&projection, "authority"),
        "situation": dimension_status(&projection, "situation"),
        "situation_record": situation.get("status").cloned().unwrap_or(json!("absent")),
        "presence_record": presence,
        "contract": stored,
        "honesty": "Statuses come from Connector's workspace projection. Absent means Connector did not store that record.",
    }))
}

fn dimension_status(body: &Value, name: &str) -> String {
    body.pointer(&format!("/dimensions/{name}/status"))
        .and_then(|value| value.as_str())
        .unwrap_or("absent")
        .to_string()
}

pub async fn free_agents(app: &App, case_id: &str) -> Value {
    let Ok(case) = app.db.get_case(case_id) else {
        return json!({ "freed": false });
    };
    let Ok(assignments) = app.db.assignments(case_id, case.generation) else {
        return json!({ "freed": false });
    };
    let mut freed = Vec::new();
    for assignment in assignments {
        let Some(pid) = assignment.connector_pid else {
            continue;
        };
        match app.connector().cease(&pid).await {
            Ok(body) => freed.push(json!({ "role": assignment.role, "pid": pid, "body": body })),
            Err(err) => freed.push(json!({ "role": assignment.role, "pid": pid, "error": err.message })),
        }
    }
    json!({ "freed": freed })
}

pub async fn ensure_open(app: &App, case_id: &str) -> AppResult<()> {
    let case = app.db.get_case(case_id)?;
    if case.status == "ceased" || case.status == "cease_incomplete" {
        return Err(AppError::forbidden(
            "case is ceased; resume to open a new Connector generation",
        ));
    }
    let bound = app
        .db
        .assignment(case_id, case.generation, "worker")?
        .and_then(|row| row.connector_pid)
        .is_some();
    if !bound || case.status == "intake_unbound" {
        bind(app, case_id, case.generation).await?;
    }
    Ok(())
}
