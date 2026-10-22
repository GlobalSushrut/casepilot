use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::config::Config;
use crate::connector::ConnectorClient;
use crate::db::Db;
use tokio::sync::OwnedMutexGuard;

use crate::error::{AppError, AppResult};

#[derive(Clone)]
pub struct Lane {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub api_key: String,
}

pub struct ModelPermit {
    _guard: OwnedMutexGuard<()>,
    pub model: String,
}

#[derive(Clone)]
pub struct App {
    pub db: Db,
    pub connector_slot: Arc<RwLock<ConnectorClient>>,
    pub model_slot: Arc<RwLock<String>>,
    pub refund_auto_limit_cents: i64,
    pub public_base: String,
    pub setup_path: PathBuf,
    pub bundle_tarball: PathBuf,
    lanes: Arc<Mutex<HashMap<String, Lane>>>,
    model_gate: Arc<tokio::sync::Mutex<()>>,
}

impl App {
    pub fn new(config: &Config) -> crate::error::AppResult<Self> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let app = Self {
            db: Db::open(&config.db_path)?,
            connector_slot: Arc::new(RwLock::new(ConnectorClient::new(
                &config.connector_url,
                &config.connector_api_key,
            ))),
            model_slot: Arc::new(RwLock::new(config.model.trim().to_string())),
            refund_auto_limit_cents: config.refund_auto_limit_cents,
            public_base: config.public_base.trim_end_matches('/').to_string(),
            setup_path: setup_path(&config.db_path),
            bundle_tarball: root.join("deploy").join("bundle").join("connector-os.tar.gz"),
            lanes: Arc::new(Mutex::new(HashMap::new())),
            model_gate: Arc::new(tokio::sync::Mutex::new(())),
        };
        app.load_setup();
        Ok(app)
    }

    pub fn connector(&self) -> ConnectorClient {
        self.connector_slot
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub fn set_connector(&self, url: &str, api_key: &str) {
        let mut slot = self
            .connector_slot
            .write()
            .unwrap_or_else(|err| err.into_inner());
        *slot = ConnectorClient::new(url, api_key);
    }

    pub fn model(&self) -> String {
        self.model_slot
            .read()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub fn remember_lane(&self, email: &str, lane: Lane) {
        let mut lanes = self.lanes.lock().unwrap_or_else(|err| err.into_inner());
        lanes.insert(email.to_string(), lane);
    }

    pub fn lane(&self, email: &str) -> Option<Lane> {
        self.lanes
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(email)
            .cloned()
    }

    /// Hold the process lane, point Connector at this email's model, and keep
    /// that binding until the permit is dropped. Another email waits.
    pub async fn bind_model(&self, email: &str) -> AppResult<ModelPermit> {
        let lane = self
            .lane(email)
            .ok_or_else(|| AppError::forbidden("Link your own model before using CasePilot."))?;
        let guard = self.model_gate.clone().lock_owned().await;
        let endpoint = if lane.endpoint.trim().is_empty() {
            None
        } else {
            Some(lane.endpoint.as_str())
        };
        let linked = self
            .connector()
            .link_llm(&lane.provider, &lane.model, &lane.api_key, endpoint)
            .await?;
        if linked.get("error").is_some() || linked.get("ok").and_then(|v| v.as_bool()) == Some(false)
        {
            return Err(AppError::unavailable(format!(
                "connector_llm_link_failed: {linked}"
            )));
        }
        Ok(ModelPermit {
            _guard: guard,
            model: lane.model,
        })
    }

    pub fn set_model(&self, model: &str) {
        let mut slot = self.model_slot.write().unwrap_or_else(|err| err.into_inner());
        *slot = model.trim().to_string();
    }

    fn load_setup(&self) {
        let Ok(text) = std::fs::read_to_string(&self.setup_path) else {
            return;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return;
        };
        // The process environment is the deployment key. A file left on the
        // disk must not point this process at a different Connector.
        if env_connector().is_none() {
            if let (Some(url), Some(key)) = (
                value.get("connector_url").and_then(|v| v.as_str()),
                value.get("connector_api_key").and_then(|v| v.as_str()),
            ) {
                if !url.is_empty() && !key.is_empty() {
                    self.set_connector(url, key);
                }
            }
        }
        if let Some(model) = value.get("model").and_then(|v| v.as_str()) {
            if !model.is_empty() {
                self.set_model(model);
            }
        }
    }
}

fn setup_path(db_path: &str) -> PathBuf {
    if let Ok(path) = env::var("CASEPILOT_SETUP") {
        let path = path.trim();
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    let db = PathBuf::from(db_path);
    db.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("setup.json")
}

fn env_connector() -> Option<()> {
    let key = env::var("CONNECTOR_API_KEY")
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let url = env::var("CONNECTOR_API_URL")
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
        || env::var("CONNECTOR_HOSTPORT")
            .ok()
            .is_some_and(|value| !value.trim().is_empty());
    (key || url).then_some(())
}
