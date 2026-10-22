use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: String,
    pub db_path: String,
    pub connector_url: String,
    pub connector_api_key: String,
    pub refund_auto_limit_cents: i64,
    pub public_base: String,
    /// Model id to request on Connector Talk when the linked router has no model yet.
    pub model: String,
}

fn nonempty(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

fn bind_addr() -> String {
    if let Some(bind) = nonempty("CASEPILOT_BIND") {
        return bind;
    }
    if let Some(port) = nonempty("PORT") {
        return format!("0.0.0.0:{port}");
    }
    "127.0.0.1:8088".into()
}

fn http_service(url_key: &str, hostport_key: &str, fallback: &str) -> String {
    if let Some(url) = nonempty(url_key) {
        return url;
    }
    if let Some(hostport) = nonempty(hostport_key) {
        return format!("http://{hostport}");
    }
    fallback.to_string()
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            bind: bind_addr(),
            db_path: env::var("CASEPILOT_DB").unwrap_or_else(|_| "data/casepilot.db".into()),
            connector_url: http_service(
                "CONNECTOR_API_URL",
                "CONNECTOR_HOSTPORT",
                "http://127.0.0.1:9091",
            ),
            connector_api_key: nonempty("CONNECTOR_API_KEY")
                .unwrap_or_else(|| "dev-token".into()),
            refund_auto_limit_cents: env::var("CASEPILOT_REFUND_AUTO_LIMIT_CENTS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5_000),
            public_base: nonempty("CASEPILOT_PUBLIC_URL")
                .or_else(|| nonempty("RENDER_EXTERNAL_URL"))
                .unwrap_or_else(|| "http://127.0.0.1:8088".into()),
            model: env::var("CASEPILOT_MODEL").unwrap_or_default(),
        }
    }
}
