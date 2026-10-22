use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS cases (
    case_id TEXT PRIMARY KEY,
    customer_id TEXT NOT NULL,
    status TEXT NOT NULL,
    generation INTEGER NOT NULL,
    summary TEXT NOT NULL,
    created_at TEXT NOT NULL,
    workbench_session_id TEXT,
    coordination_task_id TEXT
);
CREATE TABLE IF NOT EXISTS case_issues (
    issue_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    type TEXT NOT NULL,
    status TEXT NOT NULL,
    detail TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS case_evidence (
    evidence_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    source TEXT NOT NULL,
    digest TEXT NOT NULL,
    access_scope TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS case_plans (
    plan_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    proposed_resolution TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_assignments (
    assignment_id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    case_id TEXT NOT NULL,
    role TEXT NOT NULL,
    generation INTEGER NOT NULL,
    connector_pid TEXT,
    session_id TEXT
);
CREATE TABLE IF NOT EXISTS case_actions (
    action_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    task_id TEXT,
    effect_type TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL,
    amount_cents INTEGER,
    order_id TEXT,
    payload TEXT NOT NULL,
    dispatch_nonce TEXT NOT NULL,
    generation INTEGER NOT NULL,
    connector_response TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS approvals (
    approval_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    action_id TEXT NOT NULL,
    principal TEXT,
    scope TEXT NOT NULL,
    expiry TEXT,
    status TEXT NOT NULL,
    connector_hitl_id TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS action_results (
    action_id TEXT PRIMARY KEY,
    observed_state TEXT NOT NULL,
    receipt_id TEXT,
    connector_task_id TEXT,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS case_outcomes (
    case_id TEXT PRIMARY KEY,
    verified_status TEXT NOT NULL,
    evidence_refs TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS world_customers (
    customer_id TEXT PRIMARY KEY,
    name TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS world_orders (
    order_id TEXT PRIMARY KEY,
    customer_id TEXT NOT NULL,
    sku TEXT NOT NULL,
    title TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    status TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS world_shipments (
    shipment_id TEXT PRIMARY KEY,
    order_id TEXT NOT NULL,
    status TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS world_replacements (
    replacement_id TEXT PRIMARY KEY,
    order_id TEXT NOT NULL,
    case_id TEXT NOT NULL,
    action_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS world_refunds (
    refund_id TEXT PRIMARY KEY,
    order_id TEXT NOT NULL,
    case_id TEXT NOT NULL,
    action_id TEXT NOT NULL UNIQUE,
    amount_cents INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS world_notifications (
    notification_id TEXT PRIMARY KEY,
    customer_id TEXT NOT NULL,
    case_id TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS case_triage (
    case_id TEXT PRIMARY KEY,
    classification TEXT,
    info_needed TEXT,
    collected TEXT,
    file_path TEXT,
    file_digest TEXT,
    decision TEXT,
    decision_note TEXT,
    decided_by TEXT,
    reply TEXT,
    reply_source TEXT
);
CREATE TABLE IF NOT EXISTS desk_notifications (
    notification_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    audience TEXT NOT NULL,
    message TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS enabled_case_types (
    type_id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    grp TEXT NOT NULL,
    prompt TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS desk_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS case_messages (
    message_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    customer_id TEXT NOT NULL,
    direction TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS case_confirmations (
    confirmation_id TEXT PRIMARY KEY,
    case_id TEXT NOT NULL,
    customer_id TEXT NOT NULL,
    confirmed_position TEXT NOT NULL,
    confirmed_by TEXT NOT NULL,
    confirmed_at TEXT NOT NULL,
    release_phase TEXT NOT NULL,
    worker_handoff TEXT,
    customer_reply_message_id TEXT,
    last_error TEXT
);
"#;

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Clone, Debug)]
pub struct CaseRow {
    pub case_id: String,
    pub customer_id: String,
    pub status: String,
    pub generation: i64,
    pub summary: String,
    pub created_at: String,
    pub workbench_session_id: Option<String>,
    pub coordination_task_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Assignment {
    pub assignment_id: String,
    pub agent_id: String,
    pub case_id: String,
    pub role: String,
    pub generation: i64,
    pub connector_pid: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ActionRow {
    pub action_id: String,
    pub case_id: String,
    pub task_id: Option<String>,
    pub effect_type: String,
    pub idempotency_key: String,
    pub status: String,
    pub amount_cents: Option<i64>,
    pub order_id: Option<String>,
    pub generation: i64,
    pub created_at: String,
}

#[derive(Clone, Debug)]
pub struct ApprovalRow {
    pub approval_id: String,
    pub case_id: String,
    pub action_id: String,
    pub principal: Option<String>,
    pub scope: String,
    pub expiry: Option<String>,
    pub status: String,
    pub connector_hitl_id: Option<String>,
    pub created_at: String,
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

pub fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn now() -> String {
    Utc::now().to_rfc3339()
}

pub fn plus_minutes(minutes: i64) -> String {
    (Utc::now() + Duration::minutes(minutes)).to_rfc3339()
}

impl Db {
    pub fn open(path: &str) -> AppResult<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    AppError::unavailable(format!("create db dir: {err}"))
                })?;
            }
        }
        let conn = Connection::open(path).map_err(|err| AppError::unavailable(err.to_string()))?;
        conn.execute_batch(SCHEMA)
            .map_err(|err| AppError::unavailable(err.to_string()))?;
        for sql in [
            "ALTER TABLE cases ADD COLUMN matter_key TEXT",
            "ALTER TABLE cases ADD COLUMN subject_ref TEXT",
            "CREATE INDEX IF NOT EXISTS idx_cases_customer_created ON cases(customer_id, created_at)",
            "CREATE INDEX IF NOT EXISTS idx_cases_status_customer ON cases(status, customer_id)",
            "CREATE INDEX IF NOT EXISTS idx_messages_case_created ON case_messages(case_id, created_at)",
            "ALTER TABLE cases ADD COLUMN account_email TEXT NOT NULL DEFAULT ''",
            "CREATE INDEX IF NOT EXISTS idx_cases_account_created ON cases(account_email, created_at)",
        ] {
            let _ = conn.execute(sql, []);
        }
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.seed_world()?;
        Ok(db)
    }

    fn lock(&self) -> AppResult<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| AppError::unavailable("sqlite lock poisoned"))
    }

    fn seed_world(&self) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO world_customers (customer_id, name) VALUES (?1, ?2)",
            params!["cust_amina", "Amina Shah"],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO world_orders (order_id, customer_id, sku, title, amount_cents, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                "ord_4401",
                "cust_amina",
                "hp-200",
                "Studio headphones",
                12_900_i64,
                "delivered_damaged"
            ],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO world_shipments (shipment_id, order_id, status) VALUES (?1, ?2, ?3)",
            params!["shp_4401", "ord_4401", "delayed"],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn create_case(&self, customer_id: &str, summary: &str, issue_type: &str) -> AppResult<String> {
        let case_id = new_id("case");
        let issue_id = new_id("issue");
        let created = now();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO cases (case_id, customer_id, status, generation, summary, created_at)
             VALUES (?1, ?2, 'intake_unbound', 1, ?3, ?4)",
            params![case_id, customer_id, summary, created],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        conn.execute(
            "INSERT INTO case_issues (issue_id, case_id, type, status, detail)
             VALUES (?1, ?2, ?3, 'open', ?4)",
            params![issue_id, case_id, issue_type, summary],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        if let Some(account) = crate::accounts::current() {
            conn.execute(
                "UPDATE cases SET account_email = ?1 WHERE case_id = ?2",
                params![account, case_id],
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        }
        Ok(case_id)
    }

    pub fn stamp_matter(&self, case_id: &str, matter_key: &str, subject_ref: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE cases SET matter_key = ?1, subject_ref = ?2 WHERE case_id = ?3",
            params![matter_key, subject_ref, case_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    /// An open report for this party and subject. A second try continues it.
    pub fn open_matter(&self, matter_key: &str) -> AppResult<Option<String>> {
        let conn = self.lock()?;
        let account = crate::accounts::current().unwrap_or_default();
        conn.query_row(
            "SELECT case_id FROM cases
             WHERE matter_key = ?1 AND account_email = ?2
               AND status NOT IN ('ceased', 'cease_incomplete')
             ORDER BY created_at DESC LIMIT 1",
            params![matter_key, account],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn matter_key(&self, case_id: &str) -> AppResult<String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT matter_key FROM cases WHERE case_id = ?1",
            params![case_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|value| value.flatten().unwrap_or_default())
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn subject_ref(&self, case_id: &str) -> AppResult<String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT subject_ref FROM cases WHERE case_id = ?1",
            params![case_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|value| value.flatten().unwrap_or_default())
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn issue_type(&self, case_id: &str) -> AppResult<String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT type FROM case_issues WHERE case_id = ?1 ORDER BY rowid ASC LIMIT 1",
            params![case_id],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or_default())
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn live_case_count(&self) -> AppResult<i64> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT COUNT(*) FROM cases WHERE status IN ('open','in_progress','needs_info')",
            [],
            |row| row.get(0),
        )
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn get_case(&self, case_id: &str) -> AppResult<CaseRow> {
        let conn = self.lock()?;
        let row = if let Some(account) = crate::accounts::current() {
            conn.query_row(
                "SELECT case_id, customer_id, status, generation, summary, created_at, workbench_session_id, coordination_task_id
                 FROM cases WHERE case_id = ?1 AND account_email = ?2",
                params![case_id, account],
                map_case,
            )
        } else {
            conn.query_row(
                "SELECT case_id, customer_id, status, generation, summary, created_at, workbench_session_id, coordination_task_id
                 FROM cases WHERE case_id = ?1",
                params![case_id],
                map_case,
            )
        };
        row.optional()
            .map_err(|err| AppError::bad(err.to_string()))?
            .ok_or_else(|| AppError::not_found("case not found"))
    }

    pub fn list_cases(&self, customer_id: Option<&str>) -> AppResult<Vec<CaseRow>> {
        let conn = self.lock()?;
        let account = crate::accounts::current().unwrap_or_default();
        let mut stmt = if customer_id.is_some() {
            conn.prepare(
                "SELECT case_id, customer_id, status, generation, summary, created_at, workbench_session_id, coordination_task_id
                 FROM cases WHERE customer_id = ?1 AND account_email = ?2 ORDER BY created_at DESC",
            )
        } else {
            conn.prepare(
                "SELECT case_id, customer_id, status, generation, summary, created_at, workbench_session_id, coordination_task_id
                 FROM cases WHERE account_email = ?1 ORDER BY created_at DESC",
            )
        }
        .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = if let Some(customer) = customer_id {
            stmt.query_map(params![customer, account], map_case)
        } else {
            stmt.query_map(params![account], map_case)
        }
        .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn queue(&self) -> AppResult<Vec<CaseRow>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT case_id, customer_id, status, generation, summary, created_at, workbench_session_id, coordination_task_id
                 FROM cases
                 WHERE account_email = ?1
                 ORDER BY created_at DESC",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let account = crate::accounts::current().unwrap_or_default();
        let rows = stmt
            .query_map(params![account], map_case)
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn delete_case(&self, case_id: &str) -> AppResult<()> {
        let conn = self.lock()?;
        for sql in [
            "DELETE FROM case_issues WHERE case_id = ?1",
            "DELETE FROM case_evidence WHERE case_id = ?1",
            "DELETE FROM case_plans WHERE case_id = ?1",
            "DELETE FROM agent_assignments WHERE case_id = ?1",
            "DELETE FROM approvals WHERE case_id = ?1",
            "DELETE FROM case_outcomes WHERE case_id = ?1",
            "DELETE FROM action_results WHERE action_id IN (SELECT action_id FROM case_actions WHERE case_id = ?1)",
            "DELETE FROM case_actions WHERE case_id = ?1",
            "DELETE FROM case_triage WHERE case_id = ?1",
            "DELETE FROM desk_notifications WHERE case_id = ?1",
            "DELETE FROM case_messages WHERE case_id = ?1",
            "DELETE FROM cases WHERE case_id = ?1",
        ] {
            conn.execute(sql, params![case_id])
                .map_err(|err| AppError::bad(err.to_string()))?;
        }
        Ok(())
    }

    pub fn recent_actions(&self) -> AppResult<Vec<Value>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT case_id, action_id, effect_type, status, created_at
                 FROM case_actions ORDER BY created_at DESC LIMIT 12",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(json!({
                    "case_id": row.get::<_, String>(0)?,
                    "action_id": row.get::<_, String>(1)?,
                    "effect_type": row.get::<_, String>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "created_at": row.get::<_, String>(4)?,
                }))
            })
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn set_summary(&self, case_id: &str, summary: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE cases SET summary = ?1 WHERE case_id = ?2",
            params![summary, case_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn set_status(&self, case_id: &str, status: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE cases SET status = ?1 WHERE case_id = ?2",
            params![status, case_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn set_binding(
        &self,
        case_id: &str,
        generation: i64,
        session_id: Option<&str>,
        task_id: Option<&str>,
        status: &str,
    ) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE cases
             SET generation = ?1, workbench_session_id = ?2, coordination_task_id = ?3, status = ?4
             WHERE case_id = ?5",
            params![generation, session_id, task_id, status, case_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn add_assignment(
        &self,
        case_id: &str,
        role: &str,
        generation: i64,
        connector_pid: &str,
        session_id: Option<&str>,
    ) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO agent_assignments
             (assignment_id, agent_id, case_id, role, generation, connector_pid, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                new_id("asg"),
                connector_pid,
                case_id,
                role,
                generation,
                connector_pid,
                session_id
            ],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn assignment(&self, case_id: &str, generation: i64, role: &str) -> AppResult<Option<Assignment>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT assignment_id, agent_id, case_id, role, generation, connector_pid, session_id
             FROM agent_assignments
             WHERE case_id = ?1 AND generation = ?2 AND role = ?3
             ORDER BY rowid DESC LIMIT 1",
            params![case_id, generation, role],
            map_assignment,
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn assignments(&self, case_id: &str, generation: i64) -> AppResult<Vec<Assignment>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT assignment_id, agent_id, case_id, role, generation, connector_pid, session_id
                 FROM agent_assignments WHERE case_id = ?1 AND generation = ?2",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map(params![case_id, generation], map_assignment)
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn add_evidence(&self, case_id: &str, source: &str, access_scope: &str, body: &str) -> AppResult<String> {
        let evidence_id = new_id("ev");
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO case_evidence (evidence_id, case_id, source, digest, access_scope, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![evidence_id, case_id, source, sha256_hex(body), access_scope, body, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(evidence_id)
    }

    pub fn list_evidence(&self, case_id: &str) -> AppResult<Vec<Value>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT evidence_id, source, digest, access_scope, body, created_at
                 FROM case_evidence WHERE case_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map(params![case_id], |row| {
                Ok(json!({
                    "evidence_id": row.get::<_, String>(0)?,
                    "source": row.get::<_, String>(1)?,
                    "digest": row.get::<_, String>(2)?,
                    "access_scope": row.get::<_, String>(3)?,
                    "body": row.get::<_, String>(4)?,
                    "created_at": row.get::<_, String>(5)?,
                }))
            })
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn add_plan(&self, case_id: &str, proposed_resolution: &str) -> AppResult<i64> {
        let conn = self.lock()?;
        let version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM case_plans WHERE case_id = ?1",
                params![case_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        conn.execute(
            "INSERT INTO case_plans (plan_id, case_id, version, proposed_resolution, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![new_id("plan"), case_id, version, proposed_resolution, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(version)
    }

    pub fn latest_plan(&self, case_id: &str) -> AppResult<Option<Value>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT plan_id, version, proposed_resolution, created_at
             FROM case_plans WHERE case_id = ?1 ORDER BY version DESC LIMIT 1",
            params![case_id],
            |row| {
                Ok(json!({
                    "plan_id": row.get::<_, String>(0)?,
                    "version": row.get::<_, i64>(1)?,
                    "proposed_resolution": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, String>(3)?,
                }))
            },
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn create_action(
        &self,
        case_id: &str,
        task_id: Option<&str>,
        effect_type: &str,
        amount_cents: Option<i64>,
        order_id: Option<&str>,
        generation: i64,
    ) -> AppResult<ActionRow> {
        let action_id = new_id("act");
        let idempotency_key = format!("casepilot:{case_id}:{effect_type}:{action_id}");
        let nonce = new_id("nonce");
        let created = now();
        let payload = json!({
            "effect_type": effect_type,
            "order_id": order_id,
            "amount_cents": amount_cents,
            "case_id": case_id,
        })
        .to_string();
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO case_actions
             (action_id, case_id, task_id, effect_type, idempotency_key, status, amount_cents, order_id, payload, dispatch_nonce, generation, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'proposed', ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                action_id,
                case_id,
                task_id,
                effect_type,
                idempotency_key,
                amount_cents,
                order_id,
                payload,
                nonce,
                generation,
                created
            ],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(ActionRow {
            action_id,
            case_id: case_id.to_string(),
            task_id: task_id.map(str::to_string),
            effect_type: effect_type.to_string(),
            idempotency_key,
            status: "proposed".into(),
            amount_cents,
            order_id: order_id.map(str::to_string),
            generation,
            created_at: created,
        })
    }

    pub fn get_action(&self, action_id: &str) -> AppResult<ActionRow> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT action_id, case_id, task_id, effect_type, idempotency_key, status, amount_cents, order_id, generation, created_at
             FROM case_actions WHERE action_id = ?1",
            params![action_id],
            map_action,
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))?
        .ok_or_else(|| AppError::not_found("action not found"))
    }

    pub fn actions_for_case(&self, case_id: &str) -> AppResult<Vec<Value>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT a.action_id, a.effect_type, a.status, a.amount_cents, a.order_id, a.generation, a.created_at,
                        r.observed_state, r.receipt_id, r.connector_task_id
                 FROM case_actions a
                 LEFT JOIN action_results r ON r.action_id = a.action_id
                 WHERE a.case_id = ?1
                 ORDER BY a.created_at ASC",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map(params![case_id], |row| {
                Ok(json!({
                    "action_id": row.get::<_, String>(0)?,
                    "effect_type": row.get::<_, String>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "amount_cents": row.get::<_, Option<i64>>(3)?,
                    "order_id": row.get::<_, Option<String>>(4)?,
                    "generation": row.get::<_, i64>(5)?,
                    "created_at": row.get::<_, String>(6)?,
                    "observed_state": row.get::<_, Option<String>>(7)?,
                    "receipt_id": row.get::<_, Option<String>>(8)?,
                    "connector_task_id": row.get::<_, Option<String>>(9)?,
                }))
            })
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn set_action_status(&self, action_id: &str, status: &str, connector_response: &Value) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE case_actions SET status = ?1, connector_response = ?2 WHERE action_id = ?3",
            params![status, connector_response.to_string(), action_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn nonce_for(&self, action_id: &str) -> AppResult<String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT dispatch_nonce FROM case_actions WHERE action_id = ?1",
            params![action_id],
            |row| row.get(0),
        )
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn action_id_for_nonce(&self, nonce: &str) -> AppResult<Option<String>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT action_id FROM case_actions WHERE dispatch_nonce = ?1",
            params![nonce],
            |row| row.get(0),
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn create_approval(&self, case_id: &str, action_id: &str, scope: &str, hitl_id: Option<&str>) -> AppResult<String> {
        let approval_id = new_id("apr");
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO approvals
             (approval_id, case_id, action_id, principal, scope, expiry, status, connector_hitl_id, created_at)
             VALUES (?1, ?2, ?3, NULL, ?4, NULL, 'pending', ?5, ?6)",
            params![approval_id, case_id, action_id, scope, hitl_id, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(approval_id)
    }

    pub fn pending_approval_for_action(&self, action_id: &str) -> AppResult<Option<ApprovalRow>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT approval_id, case_id, action_id, principal, scope, expiry, status, connector_hitl_id, created_at
             FROM approvals WHERE action_id = ?1 AND status = 'pending' ORDER BY created_at DESC LIMIT 1",
            params![action_id],
            map_approval,
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn get_approval(&self, approval_id: &str) -> AppResult<ApprovalRow> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT approval_id, case_id, action_id, principal, scope, expiry, status, connector_hitl_id, created_at
             FROM approvals WHERE approval_id = ?1",
            params![approval_id],
            map_approval,
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))?
        .ok_or_else(|| AppError::not_found("approval not found"))
    }

    pub fn list_approvals(&self, status: Option<&str>) -> AppResult<Vec<ApprovalRow>> {
        let conn = self.lock()?;
        let account = crate::accounts::current().unwrap_or_default();
        let mut stmt = if status.is_some() {
            conn.prepare(
                "SELECT a.approval_id, a.case_id, a.action_id, a.principal, a.scope, a.expiry, a.status, a.connector_hitl_id, a.created_at
                 FROM approvals a
                 JOIN cases c ON c.case_id = a.case_id
                 WHERE a.status = ?1 AND c.account_email = ?2
                 ORDER BY a.created_at DESC",
            )
        } else {
            conn.prepare(
                "SELECT a.approval_id, a.case_id, a.action_id, a.principal, a.scope, a.expiry, a.status, a.connector_hitl_id, a.created_at
                 FROM approvals a
                 JOIN cases c ON c.case_id = a.case_id
                 WHERE c.account_email = ?1
                 ORDER BY a.created_at DESC",
            )
        }
        .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = if let Some(status) = status {
            stmt.query_map(params![status, account], map_approval)
        } else {
            stmt.query_map(params![account], map_approval)
        }
        .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn mark_approval(&self, approval_id: &str, status: &str, principal: &str, expiry: Option<&str>) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE approvals SET status = ?1, principal = ?2, expiry = ?3 WHERE approval_id = ?4",
            params![status, principal, expiry, approval_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn set_hitl_id(&self, approval_id: &str, hitl_id: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE approvals SET connector_hitl_id = ?1 WHERE approval_id = ?2",
            params![hitl_id, approval_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn valid_approval(&self, action_id: &str, scope: &str) -> AppResult<Option<ApprovalRow>> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT approval_id, case_id, action_id, principal, scope, expiry, status, connector_hitl_id, created_at
                 FROM approvals
                 WHERE action_id = ?1 AND scope = ?2 AND status = 'approved'
                 ORDER BY created_at DESC LIMIT 1",
                params![action_id, scope],
                map_approval,
            )
            .optional()
            .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(row.filter(|approval| approval_is_current(approval)))
    }

    pub fn attach_receipt(&self, action_id: &str, receipt_id: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE action_results
             SET receipt_id = ?1
             WHERE action_id = ?2 AND (receipt_id IS NULL OR receipt_id = '')",
            params![receipt_id, action_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn save_result(
        &self,
        action_id: &str,
        observed_state: &str,
        receipt_id: Option<&str>,
        connector_task_id: Option<&str>,
    ) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO action_results (action_id, observed_state, receipt_id, connector_task_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(action_id) DO UPDATE SET
                observed_state = excluded.observed_state,
                receipt_id = excluded.receipt_id,
                connector_task_id = excluded.connector_task_id",
            params![action_id, observed_state, receipt_id, connector_task_id, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn upsert_outcome(&self, case_id: &str, verified_status: &str, evidence_refs: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO case_outcomes (case_id, verified_status, evidence_refs, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(case_id) DO UPDATE SET
                verified_status = excluded.verified_status,
                evidence_refs = excluded.evidence_refs,
                updated_at = excluded.updated_at",
            params![case_id, verified_status, evidence_refs, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn outcome(&self, case_id: &str) -> AppResult<Option<Value>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT verified_status, evidence_refs, updated_at FROM case_outcomes WHERE case_id = ?1",
            params![case_id],
            |row| {
                Ok(json!({
                    "verified_status": row.get::<_, String>(0)?,
                    "evidence_refs": row.get::<_, String>(1)?,
                    "updated_at": row.get::<_, String>(2)?,
                }))
            },
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn read_order(&self, order_id: &str) -> AppResult<Value> {
        let conn = self.lock()?;
        let order = conn
            .query_row(
                "SELECT order_id, customer_id, sku, title, amount_cents, status FROM world_orders WHERE order_id = ?1",
                params![order_id],
                |row| {
                    Ok(json!({
                        "order_id": row.get::<_, String>(0)?,
                        "customer_id": row.get::<_, String>(1)?,
                        "sku": row.get::<_, String>(2)?,
                        "title": row.get::<_, String>(3)?,
                        "amount_cents": row.get::<_, i64>(4)?,
                        "status": row.get::<_, String>(5)?,
                    }))
                },
            )
            .optional()
            .map_err(|err| AppError::bad(err.to_string()))?
            .ok_or_else(|| AppError::not_found("order not found"))?;
        let shipment: Option<String> = conn
            .query_row(
                "SELECT status FROM world_shipments WHERE order_id = ?1",
                params![order_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| AppError::bad(err.to_string()))?;
        let replacements: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM world_replacements WHERE order_id = ?1",
                params![order_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let refunds: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM world_refunds WHERE order_id = ?1",
                params![order_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(json!({
            "order": order,
            "shipment_status": shipment,
            "replacement_count": replacements,
            "refund_count": refunds,
        }))
    }

    pub fn apply_replacement(&self, case_id: &str, action_id: &str, order_id: &str) -> AppResult<Value> {
        let conn = self.lock()?;
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM world_orders WHERE order_id = ?1",
                params![order_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        if exists == 0 {
            return Err(AppError::not_found("order not found"));
        }
        conn.execute(
            "INSERT OR IGNORE INTO world_replacements (replacement_id, order_id, case_id, action_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![new_id("rpl"), order_id, case_id, action_id, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        conn.execute(
            "UPDATE world_orders SET status = 'replacement_created' WHERE order_id = ?1",
            params![order_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        let replacement_id: String = conn
            .query_row(
                "SELECT replacement_id FROM world_replacements WHERE action_id = ?1",
                params![action_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(json!({
            "effect_type": "replacement",
            "order_id": order_id,
            "replacement_id": replacement_id,
        }))
    }

    pub fn apply_refund(&self, case_id: &str, action_id: &str, order_id: &str, amount_cents: i64) -> AppResult<Value> {
        let conn = self.lock()?;
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM world_orders WHERE order_id = ?1",
                params![order_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        if exists == 0 {
            return Err(AppError::not_found("order not found"));
        }
        conn.execute(
            "INSERT OR IGNORE INTO world_refunds (refund_id, order_id, case_id, action_id, amount_cents, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![new_id("rfd"), order_id, case_id, action_id, amount_cents, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        conn.execute(
            "UPDATE world_orders SET status = 'refund_created' WHERE order_id = ?1",
            params![order_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        let refund_id: String = conn
            .query_row(
                "SELECT refund_id FROM world_refunds WHERE action_id = ?1",
                params![action_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(json!({
            "effect_type": "refund",
            "order_id": order_id,
            "refund_id": refund_id,
            "amount_cents": amount_cents,
        }))
    }

    pub fn observed_effect(&self, action: &ActionRow) -> AppResult<Option<Value>> {
        let conn = self.lock()?;
        match action.effect_type.as_str() {
            "replacement" => conn
                .query_row(
                    "SELECT replacement_id, order_id FROM world_replacements WHERE action_id = ?1",
                    params![action.action_id],
                    |row| {
                        Ok(json!({
                            "effect_type": "replacement",
                            "replacement_id": row.get::<_, String>(0)?,
                            "order_id": row.get::<_, String>(1)?,
                        }))
                    },
                )
                .optional()
                .map_err(|err| AppError::bad(err.to_string())),
            "refund" => conn
                .query_row(
                    "SELECT refund_id, order_id, amount_cents FROM world_refunds WHERE action_id = ?1",
                    params![action.action_id],
                    |row| {
                        Ok(json!({
                            "effect_type": "refund",
                            "refund_id": row.get::<_, String>(0)?,
                            "order_id": row.get::<_, String>(1)?,
                            "amount_cents": row.get::<_, i64>(2)?,
                        }))
                    },
                )
                .optional()
                .map_err(|err| AppError::bad(err.to_string())),
            _ => Ok(None),
        }
    }

    pub fn save_triage(&self, case_id: &str, classification: Option<&str>, info_needed: Option<&str>, collected: Option<&str>) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO case_triage (case_id, classification, info_needed, collected)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(case_id) DO UPDATE SET
                classification = COALESCE(excluded.classification, case_triage.classification),
                info_needed = COALESCE(excluded.info_needed, case_triage.info_needed),
                collected = COALESCE(excluded.collected, case_triage.collected)",
            params![case_id, classification, info_needed, collected],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn triage(&self, case_id: &str) -> AppResult<Option<Value>> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT classification, info_needed, collected, file_path, file_digest, decision, decision_note, decided_by, reply, reply_source
             FROM case_triage WHERE case_id = ?1",
            params![case_id],
            |row| {
                Ok(json!({
                    "classification": row.get::<_, Option<String>>(0)?,
                    "info_needed": row.get::<_, Option<String>>(1)?,
                    "collected": row.get::<_, Option<String>>(2)?,
                    "file_path": row.get::<_, Option<String>>(3)?,
                    "file_digest": row.get::<_, Option<String>>(4)?,
                    "decision": row.get::<_, Option<String>>(5)?,
                    "decision_note": row.get::<_, Option<String>>(6)?,
                    "decided_by": row.get::<_, Option<String>>(7)?,
                    "reply": row.get::<_, Option<String>>(8)?,
                    "reply_source": row.get::<_, Option<String>>(9)?,
                }))
            },
        )
        .optional()
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn file_dossier(&self, case_id: &str, path: &str, digest: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE case_triage SET file_path = ?1, file_digest = ?2 WHERE case_id = ?3",
            params![path, digest, case_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn save_decision(&self, case_id: &str, decision: &str, note: &str, principal: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE case_triage SET decision = ?1, decision_note = ?2, decided_by = ?3 WHERE case_id = ?4",
            params![decision, note, principal, case_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn save_reply(&self, case_id: &str, reply: &str, source: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE case_triage SET reply = ?1, reply_source = ?2 WHERE case_id = ?3",
            params![reply, source, case_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn notify(&self, case_id: &str, audience: &str, message: &str) -> AppResult<String> {
        let id = new_id("ntf");
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO desk_notifications (notification_id, case_id, audience, message, status, created_at)
             VALUES (?1, ?2, ?3, ?4, 'open', ?5)",
            params![id, case_id, audience, message, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(id)
    }

    pub fn notifications_for(&self, audience: &str) -> AppResult<Vec<Value>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT notification_id, case_id, message, status, created_at
                 FROM desk_notifications WHERE audience = ?1 ORDER BY created_at DESC LIMIT 20",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map(params![audience], |row| {
                Ok(json!({
                    "notification_id": row.get::<_, String>(0)?,
                    "case_id": row.get::<_, String>(1)?,
                    "message": row.get::<_, String>(2)?,
                    "status": row.get::<_, String>(3)?,
                    "created_at": row.get::<_, String>(4)?,
                }))
            })
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn replace_enabled(&self, rows: &[(String, String, String, String)]) -> AppResult<()> {
        if let Some(account) = crate::accounts::current() {
            let value = rows
                .iter()
                .map(|(id, label, group, prompt)| {
                    json!({"id": id, "label": label, "group": group, "prompt": prompt})
                })
                .collect::<Vec<_>>();
            let text = serde_json::to_string(&value).map_err(|err| AppError::bad(err.to_string()))?;
            return self.set_setting(&format!("{account}::types"), &text);
        }
        let conn = self.lock()?;
        conn.execute("DELETE FROM enabled_case_types", [])
            .map_err(|err| AppError::bad(err.to_string()))?;
        for (id, label, group, prompt) in rows {
            conn.execute(
                "INSERT INTO enabled_case_types (type_id, label, grp, prompt) VALUES (?1, ?2, ?3, ?4)",
                params![id, label, group, prompt],
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        }
        Ok(())
    }

    pub fn enabled_types(&self) -> AppResult<Vec<Value>> {
        if let Some(account) = crate::accounts::current() {
            let raw = self.setting(&format!("{account}::types"))?;
            if raw.is_empty() {
                return Ok(Vec::new());
            }
            return serde_json::from_str(&raw).map_err(|err| AppError::bad(err.to_string()));
        }
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT type_id, label, grp, prompt FROM enabled_case_types ORDER BY grp, label")
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "label": row.get::<_, String>(1)?,
                    "group": row.get::<_, String>(2)?,
                    "prompt": row.get::<_, String>(3)?,
                }))
            })
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO desk_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn customer_page(&self, sort: &str, q: &str, limit: i64, offset: i64) -> AppResult<Value> {
        let sort = if sort == "hot" || sort == "all" { sort } else { "latest" };
        let q = q.trim();
        let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let hot = if sort == "hot" { 1 } else { 0 };
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        let conn = self.lock()?;
        let account = crate::accounts::current().unwrap_or_default();
        let filter = "c.account_email = ?7 AND (?1 = '' OR c.customer_id LIKE ?2 ESCAPE '\\' OR c.summary LIKE ?2 ESCAPE '\\')";
        let total: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM (
                        SELECT c.customer_id
                        FROM cases c
                        WHERE {filter}
                        GROUP BY c.customer_id
                        HAVING (?3 = 0 OR SUM(CASE WHEN c.status = 'awaiting_human' THEN 1 ELSE 0 END) > 0)
                    )"
                ),
                params![q, like, hot, "", "", "", account],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let mut counts = conn
            .prepare(&format!(
                "SELECT
                    COUNT(DISTINCT c.customer_id),
                    COUNT(DISTINCT CASE WHEN c.status = 'awaiting_human' THEN c.customer_id END),
                    SUM(CASE WHEN c.status = 'awaiting_human' THEN 1 ELSE 0 END)
                 FROM cases c
                 WHERE {filter}"
            ))
            .map_err(|err| AppError::bad(err.to_string()))?;
        let (customers, hot_customers, waiting_cases): (i64, i64, i64) = counts
            .query_row(params![q, like, 0, "", "", "", account], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, Option<i64>>(2)?.unwrap_or(0))))
            .map_err(|err| AppError::bad(err.to_string()))?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT
                    c.customer_id,
                    COUNT(*),
                    SUM(CASE WHEN c.status = 'awaiting_human' THEN 1 ELSE 0 END),
                    MAX(c.created_at),
                    MIN(CASE WHEN c.status = 'awaiting_human' THEN c.created_at END),
                    (
                        SELECT c2.summary FROM cases c2
                        WHERE c2.customer_id = c.customer_id
                          AND c2.account_email = c.account_email
                        ORDER BY c2.created_at DESC, c2.case_id ASC
                        LIMIT 1
                    ),
                    (
                        SELECT MAX(m.created_at) FROM case_messages m
                        JOIN cases cx ON cx.case_id = m.case_id
                        WHERE cx.customer_id = c.customer_id
                          AND cx.account_email = c.account_email
                    ),
                    (
                        SELECT cf.release_phase FROM case_confirmations cf
                        JOIN cases cx ON cx.case_id = cf.case_id
                        WHERE cx.customer_id = c.customer_id
                          AND cx.account_email = c.account_email
                          AND cx.status = 'awaiting_human'
                          AND cf.release_phase IN ('reply_failed', 'handoff_failed')
                        ORDER BY cf.confirmed_at DESC
                        LIMIT 1
                    )
                 FROM cases c
                 WHERE {filter}
                 GROUP BY c.customer_id
                 HAVING (?3 = 0 OR SUM(CASE WHEN c.status = 'awaiting_human' THEN 1 ELSE 0 END) > 0)
                 ORDER BY
                    CASE WHEN ?4 = 'hot' THEN MIN(CASE WHEN c.status = 'awaiting_human' THEN c.created_at END) END ASC,
                    CASE WHEN ?4 != 'hot' THEN MAX(c.created_at) END DESC,
                    c.customer_id ASC
                 LIMIT ?5 OFFSET ?6"
            ))
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map(params![q, like, hot, sort, limit, offset, account], |row| {
                Ok(json!({
                    "customer_id": row.get::<_, String>(0)?,
                    "case_count": row.get::<_, i64>(1)?,
                    "awaiting_human_count": row.get::<_, i64>(2)?,
                    "latest_case_at": row.get::<_, String>(3)?,
                    "oldest_waiting_at": row.get::<_, Option<String>>(4)?,
                    "summary": row.get::<_, String>(5)?,
                    "last_activity_at": row.get::<_, Option<String>>(6)?,
                    "attention": row.get::<_, Option<String>>(7)?,
                }))
            })
            .map_err(|err| AppError::bad(err.to_string()))?;
        let customers_page = rows.collect::<Result<Vec<_>, _>>().map_err(|err| AppError::bad(err.to_string()))?;
        Ok(json!({
            "customers": customers_page,
            "total": total,
            "offset": offset,
            "limit": limit,
            "sort": sort,
            "counts": {
                "customers": customers,
                "hot_customers": hot_customers,
                "cases_awaiting_human": waiting_cases,
            }
        }))
    }

    pub fn customer_record(&self, customer_id: &str) -> AppResult<Value> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT case_id, summary, status, created_at, subject_ref, generation
                 FROM cases WHERE customer_id = ?1 AND account_email = ?2
                 ORDER BY created_at DESC, case_id ASC",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let cases = stmt
            .query_map(params![customer_id, crate::accounts::current().unwrap_or_default()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|err| AppError::bad(err.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))?;
        drop(stmt);
        let mut references = Vec::new();
        let mut current = Vec::new();
        let mut history = Vec::new();
        for (case_id, summary, status, created_at, subject_ref, generation) in cases {
            if let Some(reference) = subject_ref.filter(|value| !value.is_empty()) {
                if !references.contains(&reference) {
                    references.push(reference);
                }
            }
            let worker_id: Option<String> = conn
                .query_row(
                    "SELECT connector_pid FROM agent_assignments
                     WHERE case_id = ?1 AND generation = ?2 AND role = 'worker'
                     LIMIT 1",
                    params![case_id, generation],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| AppError::bad(err.to_string()))?;
            let reply: Option<String> = conn
                .query_row(
                    "SELECT body FROM case_messages
                     WHERE case_id = ?1 AND direction IN ('customer_agent', 'released')
                     ORDER BY created_at DESC LIMIT 1",
                    params![case_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| AppError::bad(err.to_string()))?;
            let position: Option<String> = conn
                .query_row(
                    "SELECT confirmed_position FROM case_confirmations
                     WHERE case_id = ?1 ORDER BY confirmed_at DESC LIMIT 1",
                    params![case_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| AppError::bad(err.to_string()))?;
            let phase: Option<String> = conn
                .query_row(
                    "SELECT release_phase FROM case_confirmations
                     WHERE case_id = ?1 ORDER BY confirmed_at DESC LIMIT 1",
                    params![case_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|err| AppError::bad(err.to_string()))?;
            let item = json!({
                "case_id": case_id,
                "summary": summary,
                "status": status,
                "created_at": created_at,
                "worker_id": worker_id,
                "release_phase": phase,
                "confirmed_position": position,
                "actual_customer_reply": reply,
            });
            if status == "replied" || status == "recorded" {
                history.push(item);
            } else {
                current.push(item);
            }
        }
        let waiting = current.iter().filter(|row| row.get("status").and_then(|v| v.as_str()) == Some("awaiting_human")).count();
        let snapshot = format!(
            "{} complaints recorded. {} require a human decision. {} earlier complaints are already closed.",
            current.len() + history.len(),
            waiting,
            history.len()
        );
        Ok(json!({
            "customer_id": customer_id,
            "references": references,
            "current": current,
            "history": history,
            "snapshot": snapshot,
        }))
    }

    pub fn latest_confirmation(&self, case_id: &str) -> AppResult<Value> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT confirmation_id, confirmed_position, confirmed_by, confirmed_at, release_phase,
                        worker_handoff, customer_reply_message_id, last_error
                 FROM case_confirmations WHERE case_id = ?1
                 ORDER BY confirmed_at DESC LIMIT 1",
                params![case_id],
                |row| {
                    Ok(json!({
                        "confirmation_id": row.get::<_, String>(0)?,
                        "confirmed_position": row.get::<_, String>(1)?,
                        "confirmed_by": row.get::<_, String>(2)?,
                        "confirmed_at": row.get::<_, String>(3)?,
                        "release_phase": row.get::<_, String>(4)?,
                        "worker_handoff": row.get::<_, Option<String>>(5)?,
                        "customer_reply_message_id": row.get::<_, Option<String>>(6)?,
                        "last_error": row.get::<_, Option<String>>(7)?,
                    }))
                },
            )
            .optional()
            .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(row.unwrap_or(Value::Null))
    }

    pub fn insert_confirmation(
        &self,
        case_id: &str,
        customer_id: &str,
        position: &str,
        principal: &str,
    ) -> AppResult<String> {
        let id = new_id("conf");
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO case_confirmations
             (confirmation_id, case_id, customer_id, confirmed_position, confirmed_by, confirmed_at, release_phase)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'position_confirmed')",
            params![id, case_id, customer_id, position, principal, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(id)
    }

    pub fn set_confirmation_position(&self, confirmation_id: &str, position: &str) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE case_confirmations SET confirmed_position = ?1 WHERE confirmation_id = ?2",
            params![position, confirmation_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn update_confirmation(
        &self,
        confirmation_id: &str,
        phase: &str,
        handoff: Option<&str>,
        reply_message_id: Option<&str>,
        error: Option<&str>,
    ) -> AppResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE case_confirmations
             SET release_phase = ?1,
                 worker_handoff = COALESCE(?2, worker_handoff),
                 customer_reply_message_id = COALESCE(?3, customer_reply_message_id),
                 last_error = ?4
             WHERE confirmation_id = ?5",
            params![phase, handoff, reply_message_id, error, confirmation_id],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(())
    }

    pub fn message_body(&self, message_id: &str) -> AppResult<String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT body FROM case_messages WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn refund_recorded(&self, case_id: &str) -> AppResult<bool> {
        let conn = self.lock()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM world_refunds WHERE case_id = ?1",
                params![case_id],
                |row| row.get(0),
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(count > 0)
    }

    pub fn setting(&self, key: &str) -> AppResult<String> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT value FROM desk_settings WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or_default())
        .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn add_message(&self, case_id: &str, customer_id: &str, direction: &str, body: &str) -> AppResult<String> {
        let id = new_id("msg");
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO case_messages (message_id, case_id, customer_id, direction, body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, case_id, customer_id, direction, body, now()],
        )
        .map_err(|err| AppError::bad(err.to_string()))?;
        Ok(id)
    }

    pub fn messages_for_case(&self, case_id: &str) -> AppResult<Vec<Value>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT message_id, direction, body, created_at FROM case_messages
                 WHERE case_id = ?1 ORDER BY created_at ASC",
            )
            .map_err(|err| AppError::bad(err.to_string()))?;
        let rows = stmt
            .query_map(params![case_id], |row| {
                Ok(json!({
                    "message_id": row.get::<_, String>(0)?,
                    "direction": row.get::<_, String>(1)?,
                    "body": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, String>(3)?,
                }))
            })
            .map_err(|err| AppError::bad(err.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn refund_count(&self) -> AppResult<i64> {
        let conn = self.lock()?;
        conn.query_row("SELECT COUNT(*) FROM world_refunds", [], |row| row.get(0))
            .map_err(|err| AppError::bad(err.to_string()))
    }

    pub fn replacement_count(&self) -> AppResult<i64> {
        let conn = self.lock()?;
        conn.query_row("SELECT COUNT(*) FROM world_replacements", [], |row| row.get(0))
            .map_err(|err| AppError::bad(err.to_string()))
    }
}

fn approval_is_current(approval: &ApprovalRow) -> bool {
    let Some(expiry) = approval.expiry.as_deref() else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(expiry)
        .map(|expiry| expiry > Utc::now())
        .unwrap_or(false)
}

fn map_case(row: &rusqlite::Row<'_>) -> rusqlite::Result<CaseRow> {
    Ok(CaseRow {
        case_id: row.get(0)?,
        customer_id: row.get(1)?,
        status: row.get(2)?,
        generation: row.get(3)?,
        summary: row.get(4)?,
        created_at: row.get(5)?,
        workbench_session_id: row.get(6)?,
        coordination_task_id: row.get(7)?,
    })
}

fn map_assignment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Assignment> {
    Ok(Assignment {
        assignment_id: row.get(0)?,
        agent_id: row.get(1)?,
        case_id: row.get(2)?,
        role: row.get(3)?,
        generation: row.get(4)?,
        connector_pid: row.get(5)?,
        session_id: row.get(6)?,
    })
}

fn map_action(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActionRow> {
    Ok(ActionRow {
        action_id: row.get(0)?,
        case_id: row.get(1)?,
        task_id: row.get(2)?,
        effect_type: row.get(3)?,
        idempotency_key: row.get(4)?,
        status: row.get(5)?,
        amount_cents: row.get(6)?,
        order_id: row.get(7)?,
        generation: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn map_approval(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalRow> {
    Ok(ApprovalRow {
        approval_id: row.get(0)?,
        case_id: row.get(1)?,
        action_id: row.get(2)?,
        principal: row.get(3)?,
        scope: row.get(4)?,
        expiry: row.get(5)?,
        status: row.get(6)?,
        connector_hitl_id: row.get(7)?,
        created_at: row.get(8)?,
    })
}

pub fn case_json(case: &CaseRow) -> Value {
    json!({
        "case_id": case.case_id,
        "customer_id": case.customer_id,
        "status": case.status,
        "generation": case.generation,
        "summary": case.summary,
        "created_at": case.created_at,
        "workbench_session_id": case.workbench_session_id,
        "coordination_task_id": case.coordination_task_id,
    })
}

pub fn approval_json(row: &ApprovalRow) -> Value {
    json!({
        "approval_id": row.approval_id,
        "case_id": row.case_id,
        "action_id": row.action_id,
        "principal": row.principal,
        "scope": row.scope,
        "expiry": row.expiry,
        "status": row.status,
        "connector_hitl_id": row.connector_hitl_id,
        "created_at": row.created_at,
    })
}

pub fn refund_scope(action_id: &str, amount_cents: i64) -> String {
    format!("refund:{action_id}:{amount_cents}")
}
