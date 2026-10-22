use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;

use serde_json::{json, Value};

use crate::app::App;
use crate::error::{AppError, AppResult};

#[derive(Clone, Debug)]
struct Saved {
    connector_url: String,
    connector_api_key: String,
    mode: String,
    model: String,
    service_email: String,
    service_password: String,
    prepare_note: String,
}

pub async fn status(app: &App) -> Value {
    let saved = read_saved(app);
    let probe = probe_connector(app).await;
    let bundle = app.bundle_tarball.is_file();
    let account = crate::accounts::current();
    let lane = account.as_deref().and_then(|email| app.lane(email));
    let ready = probe.authenticated && lane.is_some();
    let reason = if !bundle && !probe.reachable {
        "Connector OS is not running, and this build has no connector-os.tar.gz to install. The desks stay closed."
    } else if !probe.reachable {
        "Connector OS is not reachable. CasePilot will not show a case, a recommendation, or a refund as true."
    } else if !probe.authenticated {
        "Connector is up. CasePilot is not authenticated to it, so the desks stay closed."
    } else if account.is_none() {
        "Enter your email. Each email is a separate trial."
    } else if lane.is_none() {
        "Link your own model. The key is sent to Connector for this email and is not written here."
    } else {
        "This email's model is linked. The rooms can open."
    };
    let reason = reason.to_string();
    json!({
        "bundle_present": bundle,
        "bundle_path": app.bundle_tarball.display().to_string(),
        "connector_url": app.connector().base(),
        "api_key_set": !saved.connector_api_key.is_empty(),
        "reachable": probe.reachable,
        "authenticated": probe.authenticated,
        "connector_real": probe.authenticated,
        "account": account,
        "llm_mode": if lane.is_some() { "live" } else { "disconnected" },
        "api_key_configured": lane.is_some(),
        "llm": lane.as_ref().map(|row| json!({"provider": row.provider, "model": row.model})),
        "desk_mode": saved.mode,
        "model": lane.as_ref().map(|row| row.model.clone()).unwrap_or_default(),
        "ready": ready,
        "reason": reason,
    })
}

pub async fn save_connector(app: &App, url: &str, api_key: &str) -> AppResult<Value> {
    let url = url.trim().trim_end_matches('/').to_string();
    let api_key = api_key.trim().to_string();
    if url.is_empty() || api_key.is_empty() {
        return Err(AppError::bad("connector url and API key are required"));
    }
    app.set_connector(&url, &api_key);
    let mut saved = read_saved(app);
    saved.connector_url = url;
    saved.connector_api_key = api_key;
    write_saved(app, &saved)?;
    let body = status(app).await;
    if body.get("authenticated").and_then(|v| v.as_bool()) != Some(true) {
        return Err(AppError::unavailable(
            "connector_not_real: Connector did not accept this URL and API key. The desks stay closed.",
        ));
    }
    Ok(body)
}

/// Runs when CasePilot starts. Starts Connector if it is down, takes a real
/// credential from the node, then sets the reasoning lane. A vendor key is
/// used only when the process environment already has one, and it is not stored.
pub async fn prepare(app: &App) {
    let mut saved = read_saved(app);
    saved.prepare_note.clear();
    // The built-in key is the one the process was started with. Retry until
    // Connector is up so a slower private service does not leave the desk closed.
    if let Some(key) = env_api_key() {
        saved.connector_api_key = key;
        saved.connector_url = app.connector().base().to_string();
    } else if saved.connector_url.is_empty() {
        saved.connector_url = app.connector().base().to_string();
    }
    if !saved.connector_api_key.is_empty() {
        app.set_connector(&saved.connector_url, &saved.connector_api_key);
    }
    let mut authed = false;
    for attempt in 1..=90 {
        if probe_connector(app).await.authenticated {
            authed = true;
            saved.prepare_note.clear();
            break;
        }
        if attempt == 90 || env_api_key().is_none() {
            break;
        }
        if attempt == 1 || attempt % 5 == 0 {
            tracing::info!(attempt, "waiting for Connector to accept the built-in API key");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    if !authed {
        if env_api_key().is_some() {
            saved.prepare_note = "Connector has not accepted the built-in API key yet. CasePilot keeps sending it.".into();
        } else {
            match enroll(app, &mut saved).await {
                Ok(key) => {
                    saved.connector_api_key = key;
                    app.set_connector(&saved.connector_url, &saved.connector_api_key);
                    saved.prepare_note.clear();
                }
                Err(note) => saved.prepare_note = note,
            }
        }
    }
    if probe_connector(app).await.authenticated && saved.mode.is_empty() {
        ensure_lane(app, &mut saved).await;
    }
    if let Err(err) = write_saved(app, &saved) {
        tracing::error!(error = %err.message, "could not store CasePilot setup");
    }
    let body = status(app).await;
    tracing::info!(
        ready = body.get("ready").and_then(|v| v.as_bool()).unwrap_or(false),
        reason = body.get("reason").and_then(|v| v.as_str()).unwrap_or(""),
        "casepilot setup finished in the backend"
    );
}

pub fn boot_connector() -> Value {
    let script = env_manifest().join("deploy").join("start-bundled-connector.sh");
    if !script.is_file() {
        let url = std::env::var("CONNECTOR_API_URL").unwrap_or_else(|_| "http://127.0.0.1:9091".into());
        let live = Command::new("curl")
            .args(["-fsS", "--max-time", "2", &format!("{}/healthz", url.trim_end_matches('/'))])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        return json!({
            "started": live,
            "mode": "external",
            "stdout": if live { "connector_already_live" } else { "connector_not_live" },
        });
    }
    match Command::new("bash").arg(&script).output() {
        Ok(output) => json!({
            "started": output.status.success(),
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout).trim(),
            "stderr": String::from_utf8_lossy(&output.stderr).trim(),
        }),
        Err(err) => json!({
            "started": false,
            "error": format!("could not start the bundled Connector OS: {err}"),
        }),
    }
}

pub fn install_bundle(app: &App) -> AppResult<Value> {
    if !app.bundle_tarball.is_file() {
        return Err(AppError::unavailable(format!(
            "connector_package_missing: place the release tarball at {}. CasePilot does not invent a Connector install.",
            app.bundle_tarball.display()
        )));
    }
    let script = env_manifest().join("deploy").join("install-bundled.sh");
    let output = Command::new("bash")
        .arg(&script)
        .current_dir(env_manifest())
        .output()
        .map_err(|err| AppError::unavailable(format!("could not run the Connector installer: {err}")))?;
    Ok(json!({
        "exit_code": output.status.code(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
        "installed": output.status.success(),
    }))
}

pub async fn link_model(
    app: &App,
    provider: &str,
    model: &str,
    api_key: &str,
    endpoint: Option<&str>,
) -> AppResult<Value> {
    require_real(app).await?;
    let account = crate::accounts::current().ok_or_else(|| {
        AppError::forbidden("Enter your email before linking a model.")
    })?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return Err(AppError::bad("provider and model are required"));
    }
    let linked = app
        .connector()
        .link_llm(provider, model, api_key.trim(), endpoint)
        .await?;
    if linked.get("error").is_some() || linked.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        return Err(AppError::unavailable(format!(
            "connector_llm_link_failed: {linked}"
        )));
    }
    app.remember_lane(
        &account,
        crate::app::Lane {
            provider: provider.to_string(),
            model: model.to_string(),
            endpoint: endpoint.unwrap_or("").to_string(),
            api_key: api_key.trim().to_string(),
        },
    );
    Ok(status(app).await)
}

pub async fn choose_agent(_app: &App, _model: &str) -> AppResult<Value> {
    Err(AppError::bad(
        "A model name without a vendor key is not a lane. Link the key through Connector. CasePilot does not store it and does not call the vendor.",
    ))
}

pub async fn require_ready(app: &App) -> AppResult<()> {
    let account = crate::accounts::current().ok_or_else(|| {
        AppError::forbidden("Enter your email before using CasePilot.")
    })?;
    if app.lane(&account).is_none() {
        return Err(AppError::forbidden(
            "Link your own model before using CasePilot.",
        ));
    }
    require_real(app).await
}

pub async fn require_real(app: &App) -> AppResult<()> {
    let probe = probe_connector(app).await;
    if probe.authenticated {
        return Ok(());
    }
    Err(AppError::unavailable(
        "connector_not_real: Connector OS is not authenticated and reachable. CasePilot will not create a case, call a model, or write a refund.",
    ))
}

struct Probe {
    reachable: bool,
    authenticated: bool,
    llm: Option<Value>,
}

async fn probe_connector(app: &App) -> Probe {
    let health = app.connector().health().await;
    let reachable = match &health {
        Ok(body) => body.get("error").and_then(|v| v.as_str()) != Some("connector_auth_denied"),
        Err(_) => false,
    };
    let llm = app.connector().llm_status().await.ok();
    let authenticated = llm.as_ref().is_some_and(llm_status_is_real);
    Probe {
        reachable: reachable || authenticated,
        authenticated,
        llm,
    }
}

fn llm_status_is_real(body: &Value) -> bool {
    if body.get("error").and_then(|v| v.as_str()) == Some("connector_auth_denied") {
        return false;
    }
    if body.pointer("/body/error/code").and_then(|v| v.as_str()) == Some("authentication_required") {
        return false;
    }
    body.get("effective_mode").and_then(|v| v.as_str()).is_some()
        || body.get("router_wired").is_some()
}

fn read_saved(app: &App) -> Saved {
    let text = fs::read_to_string(&app.setup_path).unwrap_or_default();
    let value: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    let connector = app.connector();
    Saved {
        connector_url: value
            .get("connector_url")
            .and_then(|v| v.as_str())
            .unwrap_or(connector.base())
            .to_string(),
        connector_api_key: value
            .get("connector_api_key")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        mode: value.get("mode").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        model: value.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        service_email: value
            .get("service_email")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        service_password: value
            .get("service_password")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        prepare_note: value
            .get("prepare_note")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

fn write_saved(app: &App, saved: &Saved) -> AppResult<()> {
    if let Some(parent) = app.setup_path.parent() {
        fs::create_dir_all(parent).map_err(|err| AppError::unavailable(err.to_string()))?;
    }
    let body = json!({
        "connector_url": saved.connector_url,
        "connector_api_key": saved.connector_api_key,
        "mode": saved.mode,
        "model": saved.model,
        "service_email": saved.service_email,
        "service_password": saved.service_password,
        "prepare_note": saved.prepare_note,
    });
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&app.setup_path)
        .map_err(|err| AppError::unavailable(err.to_string()))?;
    use std::io::Write;
    file.write_all(body.to_string().as_bytes())
        .map_err(|err| AppError::unavailable(err.to_string()))?;
    Ok(())
}

fn env_manifest() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn env_api_key() -> Option<String> {
    if let Ok(path) = std::env::var("CONNECTOR_API_KEY_FILE") {
        if let Ok(text) = fs::read_to_string(path) {
            let key = text.trim().to_string();
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    std::env::var("CONNECTOR_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn enroll(app: &App, saved: &mut Saved) -> Result<String, String> {
    let base = app.connector().base().to_string();
    for (email, password) in credential_sources(saved) {
        match login_and_mint(&base, &email, &password).await {
            Ok(key) => return Ok(key),
            Err(err) => tracing::info!(%email, "connector login did not yield a key: {err}"),
        }
    }
    let email = if saved.service_email.is_empty() {
        "casepilot@localhost".to_string()
    } else {
        saved.service_email.clone()
    };
    let password = if saved.service_password.is_empty() {
        format!("cpk_{}", uuid::Uuid::new_v4().simple())
    } else {
        saved.service_password.clone()
    };
    saved.service_email = email.clone();
    saved.service_password = password.clone();
    let _ = write_saved(app, saved);
    let signup = public_post(
        &base,
        "/api/v1/auth/signup",
        None,
        json!({
            "email": email,
            "password": password,
            "name": "CasePilot",
        }),
    )
    .await?;
    if let Some(key) = signup.get("api_key").and_then(|value| value.as_str()) {
        if key.starts_with("cpk_") {
            return Ok(key.to_string());
        }
    }
    if signup.get("status").and_then(|value| value.as_str()) == Some("pending_approval") {
        return Err(
            "Connector held the CasePilot account for approval and did not return a key."
                .to_string(),
        );
    }
    if signup.get("error").and_then(|value| value.as_str()) == Some("email_already_registered") {
        return login_and_mint(&base, &email, &password).await;
    }
    Err(format!("Connector did not enroll CasePilot: {signup}"))
}

fn credential_sources(saved: &Saved) -> Vec<(String, String)> {
    let mut found = Vec::new();
    if let (Ok(email), Ok(password)) = (
        std::env::var("CONNECTOR_BOOTSTRAP_SUPERADMIN_EMAIL"),
        std::env::var("CONNECTOR_BOOTSTRAP_SUPERADMIN_PASSWORD"),
    ) {
        let email = email.trim().to_string();
        let password = password.trim().to_string();
        if !email.is_empty() && !password.is_empty() {
            found.push((email, password));
        }
    }
    if !saved.service_email.is_empty() && !saved.service_password.is_empty() {
        found.push((saved.service_email.clone(), saved.service_password.clone()));
    }
    let mut dirs = vec![env_manifest().join("data").join("connector-home")];
    if let Ok(dir) = std::env::var("CONNECTOR_DATA_DIR") {
        dirs.push(std::path::PathBuf::from(dir));
    }
    if let Some(dir) = listener_data_dir() {
        dirs.push(dir);
    }
    for dir in dirs {
        for name in ["bootstrap-superadmin.secret", "casepilot-bootstrap.env"] {
            let Ok(text) = fs::read_to_string(dir.join(name)) else {
                continue;
            };
            if let Some(pair) = parse_secret(&text) {
                found.push(pair);
            }
        }
    }
    found
}

fn listener_data_dir() -> Option<std::path::PathBuf> {
    let output = Command::new("ss")
        .args(["-ltnp", "sport = :9091"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let pid = text.split("pid=").nth(1)?.split(|c: char| !c.is_ascii_digit()).next()?;
    let environ = fs::read(format!("/proc/{pid}/environ")).ok()?;
    for entry in environ.split(|byte| *byte == 0) {
        let entry = String::from_utf8_lossy(entry);
        if let Some(dir) = entry.strip_prefix("CONNECTOR_DATA_DIR=") {
            if !dir.is_empty() {
                return Some(std::path::PathBuf::from(dir));
            }
        }
    }
    None
}

fn parse_secret(text: &str) -> Option<(String, String)> {
    let mut email = None;
    let mut password = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("email=") {
            email = Some(value.trim().to_string());
        }
        if let Some(value) = line.strip_prefix("password=") {
            password = Some(value.trim().to_string());
        }
    }
    let email = email.filter(|value| !value.is_empty())?;
    let password = password.filter(|value| !value.is_empty())?;
    Some((email, password))
}

async fn login_and_mint(base: &str, email: &str, password: &str) -> Result<String, String> {
    let login = public_post(
        base,
        "/api/v1/auth/login",
        None,
        json!({ "email": email, "password": password }),
    )
    .await?;
    if let Some(error) = login.get("error").and_then(|value| value.as_str()) {
        return Err(error.to_string());
    }
    let token = login
        .get("access_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Connector login did not return an access token".to_string())?;
    let minted = public_post(
        base,
        "/api/v1/auth/api-keys",
        Some(token),
        json!({ "name": "casepilot" }),
    )
    .await?;
    if let Some(key) = minted.get("api_key").and_then(|value| value.as_str()) {
        if key.starts_with("cpk_") {
            return Ok(key.to_string());
        }
    }
    Ok(token.to_string())
}

async fn public_post(
    base: &str,
    path: &str,
    bearer: Option<&str>,
    body: Value,
) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|err| err.to_string())?;
    let mut request = client.post(format!("{base}{path}")).json(&body);
    if let Some(bearer) = bearer {
        request = request.bearer_auth(bearer);
    }
    let response = request.send().await.map_err(|err| format!("connector_unreachable: {err}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status.is_server_error() {
        return Err(format!("connector_http_{}: {text}", status.as_u16()));
    }
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text, "http_status": status.as_u16() })))
}

async fn ensure_lane(app: &App, saved: &mut Saved) {
    let model = std::env::var("CASEPILOT_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    let vendor = std::env::var("CASEPILOT_LLM_API_KEY")
        .or_else(|_| std::env::var("CONNECTOR_LLM_API_KEY"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(vendor) = vendor {
        let provider = std::env::var("CASEPILOT_LLM_PROVIDER").unwrap_or_else(|_| "openai".to_string());
        let endpoint = std::env::var("CASEPILOT_LLM_ENDPOINT").ok();
        match link_model(app, provider.trim(), &model, &vendor, endpoint.as_deref()).await {
            Ok(_) => saved.prepare_note.clear(),
            Err(err) => {
                saved.prepare_note = format!("The model link failed, so the desks stay closed. {}", err.message);
            }
        }
        let fresh = read_saved(app);
        saved.mode = fresh.mode;
        saved.model = fresh.model;
        return;
    }
    saved.mode.clear();
    saved.model.clear();
    saved.prepare_note = format!(
        "No vendor key is set. Link {model} from the open screen. The key is posted to Connector and is not written here."
    );
}

#[cfg(test)]
mod tests {
    use super::parse_secret;

    #[test]
    fn parses_bootstrap_secret() {
        let (email, password) = parse_secret("email=casepilot@localhost\npassword=secret\n").unwrap();
        assert_eq!(email, "casepilot@localhost");
        assert_eq!(password, "secret");
    }
}
