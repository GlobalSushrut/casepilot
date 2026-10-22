mod accounts;
mod api;
mod app;
mod config;
mod connector;
mod custody;
mod db;
mod engines;
mod error;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = config::Config::from_env();
    let boot = engines::setup::boot_connector();
    if boot.get("started").and_then(|value| value.as_bool()) != Some(true) {
        tracing::error!(
            boot = %boot,
            "Connector OS is not live. CasePilot will show that error and will not treat any case as true."
        );
    }
    let app = app::App::new(&config).expect("open casepilot database");
    let background = app.clone();
    tokio::spawn(async move {
        engines::setup::prepare(&background).await;
        custody::boot().await;
    });
    let bind = config.bind.clone();
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .unwrap_or_else(|err| panic!("bind {bind}: {err}"));
    tracing::info!(
        %bind,
        connector = %config.connector_url,
        "casepilot listening; business effects go through Connector OS"
    );
    axum::serve(listener, api::router(app))
        .await
        .expect("casepilot server");
}
