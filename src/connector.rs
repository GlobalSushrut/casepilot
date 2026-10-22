use crate::error::{AppError, AppResult};
use reqwest::Method;
use serde_json::{json, Value};

/// How CasePilot reads a Connector tool-invoke response.
///
/// Connector's MCP dispatch admits inside the platform process and returns an
/// outcome. It does not POST the registered bridge URL. A business write is
/// allowed only for [`Admission::Proceed`]. Anything unrecognized is a denial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    Proceed,
    Ask,
    Denied { reason: String },
}

#[derive(Clone)]
pub struct ConnectorClient {
    base: String,
    api_key: String,
    http: reqwest::Client,
}

impl ConnectorClient {
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(45))
                .build()
                .expect("reqwest client"),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn health(&self) -> AppResult<Value> {
        self.send(Method::GET, "/healthz", None, 5).await
    }

    pub async fn list_agents(&self) -> AppResult<Value> {
        self.send(Method::GET, "/api/v1/agents", None, 15).await
    }

    pub async fn register_agent(
        &self,
        name: &str,
        namespace: &str,
        role: &str,
        purpose: &str,
        model: Option<&str>,
    ) -> AppResult<Value> {
        self.send(
            Method::POST,
            "/api/v1/agents",
            Some(json!({
                "name": name,
                "namespace": namespace,
                "role": role,
                "purpose": purpose,
                "instructions": purpose,
                "model": model,
            })),
            20,
        )
        .await
    }

    pub async fn get_agent(&self, pid: &str) -> AppResult<Value> {
        self.send(Method::GET, &format!("/api/v1/agents/{pid}"), None, 15)
            .await
    }

    pub async fn open_workbench(&self, pid: &str, title: &str, goal: &str) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/workbench/sessions"),
            Some(json!({ "title": title, "goal": goal })),
            20,
        )
        .await
    }

    /// Workbench consult. Connector parameters are `message` and `model`.
    /// A turn never dispatches a tool.
    pub async fn workbench_turn(
        &self,
        pid: &str,
        session_id: &str,
        message: &str,
        model: &str,
    ) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/workbench/sessions/{session_id}/turn"),
            Some(json!({
                "message": message,
                "model": model,
            })),
            90,
        )
        .await
    }

    pub async fn link_llm(
        &self,
        provider: &str,
        model: &str,
        api_key: &str,
        endpoint: Option<&str>,
    ) -> AppResult<Value> {
        self.send(
            Method::POST,
            "/api/v1/settings/llms/link",
            Some(json!({
                "provider": provider,
                "model": model,
                "api_key": api_key,
                "endpoint": endpoint,
                "ping": true,
            })),
            40,
        )
        .await
    }

    pub async fn llm_status(&self) -> AppResult<Value> {
        self.send(Method::GET, "/api/v1/settings/llms/status", None, 15)
            .await
    }

    /// Engine-wired vendors Connector can call. Unknown vendors use `custom`
    /// with an OpenAI-compatible base URL. This is Connector's catalog, not a
    /// marketplace scrape.
    pub async fn llm_catalog(&self) -> AppResult<Value> {
        let body = self
            .send(Method::GET, "/api/v1/settings/llms/providers", None, 15)
            .await?;
        Ok(body.get("catalog").cloned().unwrap_or_else(|| json!([])))
    }

    /// Governed Talk. The vendor key stays on Connector. CasePilot sends the
    /// OpenAI-shaped parameters Connector already accepts: `model`, `messages`,
    /// and `agent_pid`.
    pub async fn talk(
        &self,
        agent_pid: &str,
        model: &str,
        instructions: &str,
        user_message: &str,
    ) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{agent_pid}/completions"),
            Some(json!({
                "model": model,
                "agent_pid": agent_pid,
                "messages": [
                    {"role": "system", "content": instructions},
                    {"role": "user", "content": user_message}
                ]
            })),
            90,
        )
        .await
    }

    pub async fn setup_workspace(&self, pid: &str, name: &str, purpose: &str, knowledge: &str) -> AppResult<Value> {
        self.ensure_talk_addresses().await?;
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/setup"),
            Some(json!({
                "name": name,
                "acume": purpose,
                "hitl_policy": "none",
                "forensic_profile": "standard",
                "setup_complete": true,
                "rules": [{
                    "id": "knowledge-is-not-permission",
                    "effect": "note",
                    "text": "Knowledge does not grant a remedy or a permission."
                }],
                "knowledge": [{ "title": "case knowledge", "content": knowledge, "tags": ["case"] }]
            })),
            20,
        )
        .await
    }

    pub async fn activate(&self, pid: &str) -> AppResult<Value> {
        self.ensure_talk_addresses().await?;
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/activate"),
            Some(json!({})),
            20,
        )
        .await
    }

    /// Connector refuses setup, activation, and memory until each of those
    /// addresses has its own rules contract and its own HITL contract.
    async fn ensure_talk_addresses(&self) -> AppResult<()> {
        for (address, tool) in [
            ("tool:lifecycle/agent_setup", "agent_setup"),
            ("tool:lifecycle/agent_activate", "agent_activate"),
            ("tool:lifecycle/route_mutate", "route_mutate"),
            ("tool:memory/write", "write"),
        ] {
            let rules = self
                .send(
                    Method::PUT,
                    "/api/v1/kernel/address-dac/rules",
                    Some(json!({
                        "address": address,
                        "default_effect": "block",
                        "allowed_tools": [tool],
                        "denied_tools": [],
                    })),
                    15,
                )
                .await?;
            if let Some(error) = connector_error(&rules) {
                return Err(AppError::unavailable(format!(
                    "connector address rules refused for {address}: {error}"
                )));
            }
            let hitl = self
                .send(
                    Method::PUT,
                    "/api/v1/kernel/address-dac/hitl",
                    Some(json!({
                        "address": address,
                        "default_policy": "none",
                        "tools": [],
                    })),
                    15,
                )
                .await?;
            if let Some(error) = connector_error(&hitl) {
                return Err(AppError::unavailable(format!(
                    "connector address approval contract refused for {address}: {error}"
                )));
            }
        }
        Ok(())
    }

    pub async fn patch_contract(
        &self,
        pid: &str,
        purpose: &str,
        capabilities: &[&str],
        denied: &[&str],
        filesystem_write: &[&str],
    ) -> AppResult<Value> {
        self.ensure_talk_addresses().await?;
        self.send(
            Method::PATCH,
            &format!("/api/v1/agents/{pid}/contract"),
            Some(json!({
                "purpose": [purpose],
                "capabilities": capabilities,
                "denied_operations": denied,
                "filesystem_read": [format!("nsfs/{pid}")],
                "filesystem_write": filesystem_write,
                "receipt_required": true,
            })),
            20,
        )
        .await
    }

    pub async fn get_contract(&self, pid: &str) -> AppResult<Value> {
        self.send(Method::GET, &format!("/api/v1/agents/{pid}/contract"), None, 15)
            .await
    }

    pub async fn post_directive(&self, pid: &str, name: &str, text: &str) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/directives"),
            Some(json!({
                "name": name,
                "scope": "case",
                "priority": 10,
                "conditions": "this case only",
                "source_kind": "operator",
                "text": text
            })),
            20,
        )
        .await
    }

    pub async fn post_situation(&self, pid: &str, task_id: Option<&str>) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/situation"),
            Some(json!({ "task_id": task_id })),
            15,
        )
        .await
    }

    pub async fn presence(&self, pid: &str) -> AppResult<Value> {
        self.send(Method::GET, &format!("/api/v1/agents/{pid}/presence"), None, 15).await
    }

    pub async fn workspace(&self, pid: &str) -> AppResult<Value> {
        self.send(Method::GET, &format!("/api/v1/agents/{pid}/workspace"), None, 15).await
    }

    pub async fn write_memory(&self, agent_pid: &str, case_id: &str, content: &str) -> AppResult<Value> {
        self.ensure_talk_addresses().await?;
        self.send(
            Method::POST,
            "/api/v1/memory/write",
            Some(json!({
                "agent_pid": agent_pid,
                "content": content,
                "type": "case_memory",
                "tags": [case_id],
                "user": "casepilot",
            })),
            20,
        )
        .await
    }

    pub async fn register_bridge(&self, agent_pid: &str, url: &str) -> AppResult<Value> {
        self.send(
            Method::POST,
            "/api/v1/tools/mcp/register",
            Some(json!({
                "bridge_id": "casepilot",
                "url": url,
                "agent_pid": agent_pid,
            })),
            20,
        )
        .await
    }

    pub async fn invoke_tool(
        &self,
        agent_pid: &str,
        tool: &str,
        input: Value,
        idempotency_key: &str,
    ) -> AppResult<Value> {
        self.ensure_effect_address(agent_pid, tool).await?;
        // A case id is not a Connector mission. Sending it makes the journal
        // reject the tool before it runs.
        self.send(
            Method::POST,
            "/api/v1/tools/mcp/invoke",
            Some(json!({
                "bridge_id": "casepilot",
                "tool": tool,
                "agent_pid": agent_pid,
                "input": input,
                "idempotency_key": idempotency_key,
            })),
            30,
        )
        .await
    }

    /// Tool dispatch checks two addresses: `tool:{bridge}/{tool}` for the
    /// action binding, and `tool:{bridge}:{tool}` for the identity stack.
    async fn ensure_effect_address(&self, agent_pid: &str, tool: &str) -> AppResult<()> {
        for address in [
            format!("tool:casepilot/{tool}"),
            format!("tool:casepilot:{tool}"),
        ] {
            let rules = self
                .send(
                    Method::PUT,
                    "/api/v1/kernel/address-dac/rules",
                    Some(json!({
                        "address": address,
                        "default_effect": "allow",
                        "allowed_tools": [],
                        "denied_tools": [],
                    })),
                    15,
                )
                .await?;
            if let Some(error) = connector_error(&rules) {
                return Err(AppError::unavailable(format!(
                    "connector address rules refused for {address}: {error}"
                )));
            }
            let hitl = self
                .send(
                    Method::PUT,
                    "/api/v1/kernel/address-dac/hitl",
                    Some(json!({
                        "address": address,
                        "default_policy": "none",
                        "tools": [],
                    })),
                    15,
                )
                .await?;
            if let Some(error) = connector_error(&hitl) {
                return Err(AppError::unavailable(format!(
                    "connector address approval contract refused for {address}: {error}"
                )));
            }
            let relation = self
                .send(
                    Method::PUT,
                    "/api/v1/kernel/address-dac/relation",
                    Some(json!({
                        "agent_pid": agent_pid,
                        "address": address,
                    })),
                    15,
                )
                .await?;
            if let Some(error) = connector_error(&relation) {
                return Err(AppError::unavailable(format!(
                    "connector address relation refused for {address}: {error}"
                )));
            }
        }
        Ok(())
    }

    pub async fn hitl_pending(&self, pid: &str) -> AppResult<Value> {
        self.send(
            Method::GET,
            &format!("/api/v1/agents/{pid}/hitl/pending"),
            None,
            15,
        )
        .await
    }

    pub async fn hitl_approve(&self, pid: &str, request_id: &str, note: &str) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/hitl/{request_id}/approve"),
            Some(json!({ "note": note })),
            20,
        )
        .await
    }

    pub async fn hitl_deny(&self, pid: &str, request_id: &str, note: &str) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/hitl/{request_id}/deny"),
            Some(json!({ "note": note })),
            20,
        )
        .await
    }

    pub async fn receipts(&self, pid: &str) -> AppResult<Value> {
        self.send(
            Method::GET,
            &format!("/api/v1/agents/{pid}/audit/receipts?limit=20"),
            None,
            15,
        )
        .await
    }

    pub async fn cease(&self, pid: &str) -> AppResult<Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/agents/{pid}/cease"),
            Some(json!({})),
            20,
        )
        .await
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        timeout_secs: u64,
    ) -> AppResult<Value> {
        let url = format!("{}{path}", self.base);
        let mut req = self.http.request(method, &url).timeout(std::time::Duration::from_secs(timeout_secs));
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let response = req.send().await.map_err(|err| {
            AppError::unavailable(format!("connector_unreachable: {err}"))
        })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let value = serde_json::from_str::<Value>(&text)
            .unwrap_or_else(|_| json!({ "raw": text, "http_status": status.as_u16() }));
        if status.is_server_error() {
            return Err(AppError::unavailable(format!(
                "connector_http_{}: {value}",
                status.as_u16()
            )));
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Ok(json!({
                "error": "connector_auth_denied",
                "http_status": status.as_u16(),
                "body": value,
            }));
        }
        Ok(value)
    }
}

pub fn classify_admission(body: &Value) -> Admission {
    let error = body
        .get("error")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("denial_reason").and_then(|v| v.as_str()))
        .unwrap_or("");
    let error_l = error.to_ascii_lowercase();
    if !error.is_empty() {
        if error_l.contains("ask")
            || error_l.contains("hitl")
            || error_l.contains("approval")
            || error_l.contains("pending")
        {
            return Admission::Ask;
        }
        return Admission::Denied {
            reason: error.to_string(),
        };
    }

    let outcome = body.get("outcome").and_then(|v| v.as_str()).unwrap_or("");
    let verdict = body.get("verdict").and_then(|v| v.as_str()).unwrap_or("");
    let combined = format!("{outcome} {verdict}").to_ascii_lowercase();
    if combined.contains("pending") || combined.contains("ask") || combined.contains("skipped") {
        return Admission::Ask;
    }
    if combined.contains("denied") || combined.contains("failed") || combined.contains("block") {
        return Admission::Denied {
            reason: if outcome.is_empty() {
                verdict.to_string()
            } else {
                outcome.to_string()
            },
        };
    }
    if outcome.eq_ignore_ascii_case("success")
        || verdict.eq_ignore_ascii_case("proceed")
        || body.get("executed").and_then(|v| v.as_bool()) == Some(true)
    {
        return Admission::Proceed;
    }
    if body.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        return Admission::Denied {
            reason: "connector_ok_false".into(),
        };
    }
    Admission::Denied {
        reason: "unrecognized_connector_response".into(),
    }
}

pub fn connector_error(body: &Value) -> Option<String> {
    if let Some(error) = body.get("error").and_then(|value| value.as_str()) {
        if !error.is_empty() {
            return Some(error.to_string());
        }
    }
    if body.get("error").is_some() && !body.get("error").unwrap().is_null() {
        return Some(body.get("error").unwrap().to_string());
    }
    if body.get("ok") == Some(&Value::Bool(false)) {
        return Some("connector_refused".into());
    }
    None
}

pub fn session_id_of(body: &Value) -> Option<String> {
    extract_string(body, &["session_id", "sid"]).or_else(|| {
        body.pointer("/session/session_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

/// Last assistant event on a workbench turn. Empty text is not a reply.
pub fn workbench_reply(body: &Value) -> Option<String> {
    let events = body.get("events")?.as_array()?;
    events.iter().rev().find_map(|event| {
        let kind = event.get("kind").and_then(|value| value.as_str())?;
        if kind != "assistant" {
            return None;
        }
        let content = event.get("content").and_then(|value| value.as_str())?.trim();
        if content.is_empty() {
            None
        } else {
            Some(content.to_string())
        }
    })
}

pub fn extract_string(body: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = body.get(*key).and_then(|v| v.as_str()) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn extract_hitl_id(body: &Value) -> Option<String> {
    extract_string(body, &["request_id", "hitl_id", "audit_id"])
}

pub fn assistant_text(body: &Value) -> Option<String> {
    if let Some(content) = body
        .pointer("/choices/0/message/content")
        .and_then(|value| value.as_str())
    {
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    extract_string(body, &["content", "text", "output"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_outcome_proceeds() {
        assert_eq!(
            classify_admission(&json!({"outcome": "Success", "pate_task_id": "t1"})),
            Admission::Proceed
        );
    }

    #[test]
    fn ask_and_denial_do_not_proceed() {
        assert_eq!(
            classify_admission(&json!({"error": "pate_not_proceed"})),
            Admission::Denied { reason: "pate_not_proceed".into() }
        );
        assert_eq!(
            classify_admission(&json!({"outcome": "Pending"})),
            Admission::Ask
        );
        assert_eq!(
            classify_admission(&json!({"ok": true, "generated_at": "x"})),
            Admission::Denied { reason: "unrecognized_connector_response".into() }
        );
    }

    #[test]
    fn reads_workbench_assistant_event() {
        let body = json!({
            "ok": true,
            "events": [
                {"kind": "user", "content": "classify"},
                {"kind": "assistant", "content": " easy "}
            ]
        });
        assert_eq!(workbench_reply(&body).as_deref(), Some("easy"));
        assert!(workbench_reply(&json!({"events": [{"kind": "assistant", "content": "  "}]})).is_none());
    }

    #[test]
    fn reads_connector_chat_completion_text() {
        let body = json!({
            "model": "gpt-4o-mini",
            "llm_mode": "live",
            "choices": [{ "message": { "role": "assistant", "content": "  Recommend replacement.  " } }]
        });
        assert_eq!(
            assistant_text(&body).as_deref(),
            Some("Recommend replacement.")
        );
    }
}
