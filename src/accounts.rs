use axum::http::HeaderMap;
use tokio::task_local;

task_local! {
    static ACCOUNT: String;
}

pub fn current() -> Option<String> {
    ACCOUNT.try_with(|value| value.clone()).ok()
}

pub async fn scope<T>(account: String, fut: impl std::future::Future<Output = T>) -> T {
    ACCOUNT.scope(account, fut).await
}

pub fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_ascii_lowercase();
    if email.len() > 200 || email.contains([' ', ';', ',', '"', '\n', '\r']) {
        return None;
    }
    let (name, host) = email.split_once('@')?;
    if name.is_empty() || !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
        return None;
    }
    let ok = email
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '_' | '%' | '+' | '-'));
    ok.then_some(email)
}

pub fn from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())?;
    for part in raw.split(';') {
        let part = part.trim();
        let Some(value) = part.strip_prefix("casepilot_account=") else {
            continue;
        };
        return normalize_email(value);
    }
    None
}

pub fn setting_key(name: &str) -> String {
    match current() {
        Some(account) => format!("{account}::{name}"),
        None => name.to_string(),
    }
}
