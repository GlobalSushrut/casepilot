use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::services::ServeDir;

use crate::app::App;
use crate::engines::{self, case, continuity, decision, investigation, setup};
use crate::error::{AppError, AppResult};

#[derive(Clone, Debug)]
struct Session {
    role: String,
    principal: String,
    customer_id: String,
}

pub fn router(app: App) -> Router {
    let assets = ServeDir::new(web_dir().join("dist"));
    Router::new()
        .route("/healthz", get(healthz))
        .route("/styles.css", get(styles))
        .route("/", get(home))
        .route("/portal", get(portal_page))
        .route("/workstation", get(workstation_page))
        .route("/supervisor", get(supervisor_page))
        .route("/api/casepilot/setup/status", get(setup_status))
        .route("/api/casepilot/setup/account", post(setup_account))
        .route("/api/casepilot/setup/connector", post(setup_connector))
        .route("/api/casepilot/setup/install", post(setup_install))
        .route("/api/casepilot/setup/llm", post(setup_llm))
        .route("/api/casepilot/setup/agent", post(setup_agent))
        .route("/api/casepilot/catalog", get(catalog_chips).post(catalog_publish))
        .route("/api/casepilot/catalog/ask", post(catalog_ask))
        .route("/api/casepilot/catalog/manage", get(catalog_manage))
        .route("/api/casepilot/work/notifications", get(notifications))
        .route("/api/casepilot/session", get(current_session).post(set_session))
        .route("/api/casepilot/cases", get(list_cases).post(create_case))
        .route("/api/casepilot/cases/:id", get(get_case))
        .route("/api/casepilot/cases/:id/desk", get(case_desk))
        .route("/api/casepilot/cases/:id/chat", post(case_chat))
        .route("/api/casepilot/cases/:id/confirm", post(confirm_label))
        .route("/api/casepilot/cases/:id/info", post(add_info))
        .route("/api/casepilot/cases/:id/decide", post(decide_case))
        .route("/api/casepilot/cases/:id/release", post(release_case))
        .route("/api/casepilot/cases/:id/position", post(save_position))
        .route("/api/casepilot/customers/:id", get(customer_record))
        .route("/api/casepilot/library/search", get(library_search))
        .route("/api/casepilot/cases/:id/investigate", post(investigate))
        .route("/api/casepilot/cases/:id/policy", post(policy_turn))
        .route("/api/casepilot/cases/:id/actions", post(propose_action))
        .route("/api/casepilot/cases/:id/evidence", get(evidence))
        .route("/api/casepilot/cases/:id/lane", get(agent_lane))
        .route("/api/casepilot/cases/:id/cease", post(cease_case))
        .route("/api/casepilot/cases/:id/resume", post(resume_case))
        .route("/api/casepilot/work/queue", get(queue))
        .route("/api/casepilot/approvals", get(pending_approvals))
        .route("/api/casepilot/approvals/:id/approve", post(approve))
        .route("/api/casepilot/approvals/:id/deny", post(deny))
        .route("/api/casepilot/deployment", get(deployment))
        .route("/internal/tools/:tool", post(internal_tool))
        .nest_service("/assets", assets)
        .layer(middleware::from_fn_with_state(app.clone(), account_gate))
        .layer(middleware::from_fn(no_store))
        .with_state(app)
}

async fn account_gate(State(app): State<App>, request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let account = crate::accounts::from_headers(request.headers());
    let open = !path.starts_with("/api/casepilot/")
        || path == "/api/casepilot/setup/status"
        || path == "/api/casepilot/setup/account";
    if !open && account.is_none() {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Enter your email before using CasePilot."})),
        )
            .into_response();
    }
    if !open && path != "/api/casepilot/setup/llm" {
        if let Some(email) = account.as_deref() {
            if app.lane(email).is_none() {
                return (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "Link your own model before using CasePilot."})),
                )
                    .into_response();
            }
        }
    }
    if let Some(account) = account {
        crate::accounts::scope(account, next.run(request)).await
    } else {
        next.run(request).await
    }
}

async fn no_store(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

fn web_dir() -> std::path::PathBuf {
    std::env::var("CASEPILOT_WEB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web"))
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "ok": true })))
}

async fn styles() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/styles.css"),
    )
}

async fn home() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn portal_page() -> Html<&'static str> {
    Html(include_str!("../web/portal.html"))
}

async fn workstation_page() -> Html<&'static str> {
    Html(include_str!("../web/workstation.html"))
}

async fn supervisor_page() -> Html<&'static str> {
    Html(include_str!("../web/supervisor.html"))
}

#[derive(Deserialize)]
struct SessionBody {
    role: String,
    principal: String,
    customer_id: Option<String>,
    surface: Option<String>,
}

async fn set_session(Json(body): Json<SessionBody>) -> AppResult<Response> {
    let role = body.role.trim();
    if role != "customer" && role != "operator" && role != "supervisor" {
        return Err(AppError::bad("role must be customer, operator, or supervisor"));
    }
    let principal = body.principal.trim();
    if principal.is_empty() || principal.contains([';', ' ', ',']) {
        return Err(AppError::bad("principal must be a single token"));
    }
    let customer_id = body.customer_id.unwrap_or_default();
    if customer_id.is_empty() {
        return Err(AppError::bad("customer_id is required. A person names themselves."));
    }
    if customer_id.contains([';', ' ', ',']) {
        return Err(AppError::bad("customer_id must be a single token"));
    }
    let surface = body.surface.as_deref().unwrap_or("").trim();
    let prefix = match surface {
        "report" | "desk" | "review" => format!("casepilot_{surface}"),
        _ => "casepilot".to_string(),
    };
    let mut response = Json(json!({
        "role": role,
        "principal": principal,
        "customer_id": customer_id,
        "surface": if prefix == "casepilot" { "shared" } else { surface },
    }))
    .into_response();
    let headers = response.headers_mut();
    for (name, value) in [
        (format!("{prefix}_role"), role),
        (format!("{prefix}_principal"), principal),
        (format!("{prefix}_customer"), customer_id.as_str()),
    ] {
        let cookie = format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax");
        headers.append(
            header::SET_COOKIE,
            cookie
                .parse()
                .map_err(|_| AppError::bad("invalid session cookie"))?,
        );
    }
    Ok(response)
}

async fn current_session(headers: HeaderMap) -> AppResult<Json<Value>> {
    let session = session_from(&headers)?;
    Ok(Json(json!({
        "role": session.role,
        "principal": session.principal,
        "customer_id": session.customer_id,
    })))
}

#[derive(Deserialize)]
struct CreateCase {
    summary: String,
    issue_type: String,
    subject_ref: Option<String>,
}

async fn setup_status(State(app): State<App>) -> Json<Value> {
    let mut body = setup::status(&app).await;
    let catalog = app.connector().llm_catalog().await.unwrap_or_else(|_| json!([]));
    if let Some(obj) = body.as_object_mut() {
        obj.insert("catalog".into(), catalog);
    }
    Json(body)
}

#[derive(Deserialize)]
struct AskBody {
    message: String,
    subject_ref: Option<String>,
}

async fn catalog_ask(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<AskBody>,
) -> AppResult<Json<Value>> {
    let session = require(&headers, "customer")?;
    setup::require_ready(&app).await?;
    Ok(Json(
        engines::catalog::answer_before_type(
            &app,
            &session.customer_id,
            &body.message,
            body.subject_ref.as_deref().unwrap_or(""),
        )
        .await?,
    ))
}

async fn catalog_chips(State(app): State<App>) -> AppResult<Json<Value>> {
    setup::require_real(&app).await?;
    Ok(Json(engines::catalog::customer_view(&app)?))
}

async fn catalog_manage(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    require(&headers, "supervisor")?;
    setup::require_real(&app).await?;
    Ok(Json(engines::catalog::supervisor_view(&app)?))
}

#[derive(Deserialize)]
struct CatalogBody {
    ids: Vec<String>,
}

async fn catalog_publish(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<CatalogBody>,
) -> AppResult<Json<Value>> {
    require(&headers, "supervisor")?;
    Ok(Json(engines::catalog::publish(&app, &body.ids).await?))
}

#[derive(Deserialize)]
struct ConnectorBody {
    connector_url: String,
    api_key: String,
}

async fn setup_connector(State(app): State<App>, Json(body): Json<ConnectorBody>) -> AppResult<Json<Value>> {
    Ok(Json(setup::save_connector(&app, &body.connector_url, &body.api_key).await?))
}

async fn setup_install(State(_app): State<App>) -> AppResult<Json<Value>> {
    let body = setup::boot_connector();
    if body.get("started").and_then(|value| value.as_bool()) != Some(true) {
        return Err(AppError::unavailable(format!(
            "connector_not_live: {body}"
        )));
    }
    Ok(Json(body))
}

#[derive(Deserialize)]
struct LlmBody {
    provider: String,
    model: String,
    api_key: String,
    endpoint: Option<String>,
}

#[derive(Deserialize)]
struct AccountBody {
    email: String,
}

async fn setup_account(Json(body): Json<AccountBody>) -> AppResult<Response> {
    let Some(email) = crate::accounts::normalize_email(&body.email) else {
        return Err(AppError::bad("Enter a real email address. Each email is a separate trial."));
    };
    let mut response = Json(json!({ "account": email })).into_response();
    let cookie = format!("casepilot_account={email}; Path=/; HttpOnly; SameSite=Lax");
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie
            .parse()
            .map_err(|_| AppError::bad("invalid email cookie"))?,
    );
    Ok(response)
}

async fn setup_llm(State(app): State<App>, Json(body): Json<LlmBody>) -> AppResult<Json<Value>> {
    Ok(Json(
        setup::link_model(
            &app,
            &body.provider,
            &body.model,
            &body.api_key,
            body.endpoint.as_deref(),
        )
        .await?,
    ))
}

#[derive(Deserialize)]
struct AgentBody {
    model: String,
}

async fn setup_agent(State(app): State<App>, Json(body): Json<AgentBody>) -> AppResult<Json<Value>> {
    Ok(Json(setup::choose_agent(&app, &body.model).await?))
}

async fn notifications(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let session = session_from(&headers)?;
    setup::require_real(&app).await?;
    let audience = if session.role == "customer" { "customer" } else { "operator" };
    Ok(Json(json!({
        "connector_real": true,
        "notifications": app.db.notifications_for(audience)?,
        "events": app.db.recent_actions()?,
    })))
}

#[derive(Deserialize)]
struct InfoBody {
    detail: String,
}

async fn case_desk(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let session = session_from(&headers)?;
    if session.role == "customer" {
        return Err(AppError::forbidden("operator or supervisor session required"));
    }
    setup::require_real(&app).await?;
    let mut body = case::desk(&app, &id)?;
    body["custody"] = crate::custody::seal(&app, &id).await;
    body["position_draft"] = json!(app.db.setting(&format!("position_draft:{id}")).unwrap_or_default());
    body["confirmation"] = app.db.latest_confirmation(&id).unwrap_or(Value::Null);
    Ok(Json(body))
}

#[derive(Deserialize)]
struct ChatBody {
    message: String,
}

async fn case_chat(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChatBody>,
) -> AppResult<Json<Value>> {
    require_staff(&headers)?;
    setup::require_real(&app).await?;
    Ok(Json(engines::triage::chat(&app, &id, &body.message).await?))
}

async fn confirm_label(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let session = require(&headers, "operator")?;
    setup::require_real(&app).await?;
    Ok(Json(engines::triage::confirm_label(&app, &id, &session.principal)?))
}

async fn add_info(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<InfoBody>,
) -> AppResult<Json<Value>> {
    require(&headers, "customer")?;
    setup::require_real(&app).await?;
    Ok(Json(engines::triage::add_info(&app, &id, &body.detail).await?))
}

#[derive(Deserialize)]
struct DecideBody {
    choice: String,
    note: String,
}

async fn decide_case(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<DecideBody>,
) -> AppResult<Json<Value>> {
    let session = require(&headers, "operator")?;
    setup::require_real(&app).await?;
    Ok(Json(
        engines::triage::decide(&app, &id, &session.principal, &body.choice, &body.note).await?,
    ))
}

async fn create_case(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<CreateCase>,
) -> AppResult<Json<Value>> {
    setup::require_ready(&app).await?;
    let session = require(&headers, "customer")?;
    Ok(Json(
        case::intake(
            &app,
            &session.customer_id,
            &body.summary,
            &body.issue_type,
            body.subject_ref.as_deref().unwrap_or(""),
        )
        .await?,
    ))
}

#[derive(Deserialize)]
struct ReleaseBody {
    text: String,
}

async fn release_case(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ReleaseBody>,
) -> AppResult<Json<Value>> {
    let session = require_staff(&headers)?;
    setup::require_ready(&app).await?;
    let text = engines::triage::release(&app, &id, &session.principal, &body.text).await?;
    let _ = crate::custody::seal(&app, &id).await;
    let confirmation = app.db.latest_confirmation(&id).unwrap_or(Value::Null);
    Ok(Json(json!({ "result": "released", "reply": text, "confirmation": confirmation })))
}

#[derive(Deserialize)]
struct PositionBody {
    text: String,
}

async fn save_position(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PositionBody>,
) -> AppResult<Json<Value>> {
    require_staff(&headers)?;
    setup::require_ready(&app).await?;
    let _ = app.db.get_case(&id)?;
    app.db.set_setting(&format!("position_draft:{id}"), body.text.trim())?;
    Ok(Json(json!({ "saved": true })))
}

async fn customer_record(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    require_staff(&headers)?;
    setup::require_real(&app).await?;
    Ok(Json(app.db.customer_record(&id)?))
}

async fn library_search(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> AppResult<Json<Value>> {
    let session = session_from(&headers)?;
    if session.role == "customer" {
        return Err(AppError::forbidden("the desk searches the library"));
    }
    setup::require_real(&app).await?;
    Ok(Json(crate::custody::search(&query.q).await))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn list_cases(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    setup::require_real(&app).await?;
    let session = session_from(&headers)?;
    let customer = if session.role == "customer" {
        session.customer_id.as_str()
    } else if session.role == "operator" || session.role == "supervisor" {
        session.customer_id.as_str()
    } else {
        return Err(AppError::forbidden("sign in required"));
    };
    Ok(Json(case::list_for_customer(&app, customer)?))
}

async fn get_case(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    setup::require_real(&app).await?;
    let session = session_from(&headers)?;
    let detail = case::detail(&app, &id)?;
    if session.role == "customer" {
        let owner = detail["case"]["customer_id"].as_str().unwrap_or("");
        if owner != session.customer_id {
            return Err(AppError::forbidden("case belongs to another customer"));
        }
    }
    Ok(Json(detail))
}

async fn investigate(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    require_staff(&headers)?;
    setup::require_real(&app).await?;
    Ok(Json(investigation::investigate(&app, &id).await?))
}

async fn policy_turn(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    require(&headers, "operator")?;
    setup::require_real(&app).await?;
    Ok(Json(decision::policy(&app, &id).await?))
}

#[derive(Deserialize)]
struct ProposeBody {
    effect_type: String,
    order_id: String,
    amount_cents: Option<i64>,
}

async fn propose_action(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ProposeBody>,
) -> AppResult<Json<Value>> {
    require(&headers, "operator")?;
    setup::require_real(&app).await?;
    Ok(Json(
        engines::action::propose(
            &app,
            &id,
            &body.effect_type,
            &body.order_id,
            body.amount_cents,
        )
        .await?,
    ))
}

async fn agent_lane(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let session = session_from(&headers)?;
    if session.role != "operator" && session.role != "supervisor" {
        return Err(AppError::forbidden("operator or supervisor session required"));
    }
    setup::require_real(&app).await?;
    Ok(Json(engines::talk::lane(&app, &id).await?))
}

async fn evidence(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    require(&headers, "operator")?;
    setup::require_real(&app).await?;
    Ok(Json(engines::verification::evidence(&app, &id)?))
}

#[derive(Deserialize)]
struct QueueQuery {
    view: Option<String>,
    sort: Option<String>,
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn queue(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<QueueQuery>,
) -> AppResult<Json<Value>> {
    require_staff(&headers)?;
    setup::require_real(&app).await?;
    if query.view.as_deref() == Some("customers") {
        return Ok(Json(app.db.customer_page(
            query.sort.as_deref().unwrap_or("latest"),
            query.q.as_deref().unwrap_or(""),
            query.limit.unwrap_or(40),
            query.offset.unwrap_or(0),
        )?));
    }
    Ok(Json(case::queue(&app)?))
}

#[derive(Deserialize)]
struct ApprovalQuery {
    connector: Option<String>,
}

async fn pending_approvals(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApprovalQuery>,
) -> AppResult<Json<Value>> {
    require(&headers, "supervisor")?;
    setup::require_real(&app).await?;
    let mut body = case::approvals(&app)?;
    let mut connector_hitl = Vec::new();
    if query.connector.as_deref() != Some("1") {
        body["connector_hitl"] = json!(connector_hitl);
        return Ok(Json(body));
    }
    for case_row in app.db.queue()? {
        let Some(pid) = app
            .db
            .assignment(&case_row.case_id, case_row.generation, "worker")?
            .and_then(|row| row.connector_pid)
        else {
            continue;
        };
        match app.connector().hitl_pending(&pid).await {
            Ok(raw) => connector_hitl.push(json!({ "connector_pid": pid, "pending": raw })),
            Err(err) => connector_hitl.push(json!({ "connector_pid": pid, "error": err.message })),
        }
    }
    body["connector_hitl"] = json!(connector_hitl);
    Ok(Json(body))
}

async fn approve(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let session = require(&headers, "supervisor")?;
    setup::require_real(&app).await?;
    Ok(Json(decision::approve(&app, &id, &session.principal).await?))
}

async fn deny(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let session = require(&headers, "supervisor")?;
    setup::require_real(&app).await?;
    Ok(Json(decision::deny(&app, &id, &session.principal).await?))
}

async fn cease_case(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let session = session_from(&headers)?;
    if session.role != "operator" && session.role != "supervisor" {
        return Err(AppError::forbidden("operator or supervisor session required"));
    }
    setup::require_real(&app).await?;
    Ok(Json(continuity::cease(&app, &id).await?))
}

async fn resume_case(
    State(app): State<App>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    require(&headers, "supervisor")?;
    setup::require_real(&app).await?;
    Ok(Json(continuity::resume(&app, &id).await?))
}

async fn deployment(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    require(&headers, "supervisor")?;
    setup::require_real(&app).await?;
    Ok(Json(continuity::deployment(&app).await?))
}

async fn internal_tool(
    State(app): State<App>,
    Path(tool): Path<String>,
    body: Option<Json<Value>>,
) -> impl IntoResponse {
    let nonce = body
        .as_ref()
        .and_then(|Json(value)| value.get("dispatch_nonce"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let known = app
        .db
        .action_id_for_nonce(nonce)
        .ok()
        .flatten()
        .is_some();
    (
        StatusCode::CONFLICT,
        Json(json!({
            "ok": false,
            "tool": tool,
            "known_nonce": known,
            "applied": false,
            "error": "bridge_callback_not_an_admission",
            "honesty": "Connector OS admits this tool inside the platform process and does not POST this URL. CasePilot applies a business effect only after /api/v1/tools/mcp/invoke returns Success or proceed.",
        })),
    )
}

fn require(headers: &HeaderMap, role: &str) -> AppResult<Session> {
    let session = session_from(headers)?;
    if session.role != role {
        return Err(AppError::forbidden(format!("{role} session required")));
    }
    Ok(session)
}

fn require_staff(headers: &HeaderMap) -> AppResult<Session> {
    let session = session_from(headers)?;
    if session.role != "operator" && session.role != "supervisor" {
        return Err(AppError::forbidden("operator or supervisor session required"));
    }
    Ok(session)
}

fn session_from(headers: &HeaderMap) -> AppResult<Session> {
    let raw = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let surface = headers
        .get("x-casepilot-surface")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let prefix = match surface {
        "report" | "desk" | "review" => format!("casepilot_{surface}"),
        _ => "casepilot".to_string(),
    };
    let mut role = None;
    let mut principal = None;
    let mut customer_id = None;
    for part in raw.split(';') {
        let mut pair = part.trim().splitn(2, '=');
        let key = pair.next().unwrap_or("");
        let value = pair.next().unwrap_or("").trim();
        if key == format!("{prefix}_role") {
            role = Some(value.to_string());
        } else if key == format!("{prefix}_principal") {
            principal = Some(value.to_string());
        } else if key == format!("{prefix}_customer") {
            customer_id = Some(value.to_string());
        }
    }
    Ok(Session {
        role: role.ok_or_else(|| AppError::forbidden("sign in required"))?,
        principal: principal.ok_or_else(|| AppError::forbidden("sign in required"))?,
        customer_id: customer_id.ok_or_else(|| AppError::bad("sign in with your name"))?,
    })
}
