//! HTTP API. Every route except `/healthz` requires `Authorization: Bearer <PCNC_API_TOKEN>`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pcnc_core::model::{OperationResult, ProvisionRequest};
use pcnc_core::{Engine, EngineError};
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::audit::Audit;

pub struct AppState {
    pub engine: Engine,
    pub audit: Audit,
    pub token: String,
    pub info: serde_json::Value,
    pub now: Box<dyn Fn() -> String + Send + Sync>,
    pub rate: Mutex<(Instant, u32)>,
    pub rate_limit: u32,
}

pub fn router(state: Arc<AppState>) -> Router {
    let v1 = Router::new()
        .route("/info", get(info))
        .route("/shared-mailboxes", get(shared_mailboxes))
        .route("/identities", get(list))
        .route(
            "/identities/{id}",
            get(get_identity).put(provision).delete(purge),
        )
        .route("/identities/{id}/inspect", get(inspect))
        .route("/identities/{id}/disable", post(disable))
        .route("/identities/{id}/enable", post(enable))
        .route("/identities/{id}/rotate", post(rotate))
        .layer(middleware::from_fn_with_state(state.clone(), guard));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/v1", v1)
        .with_state(state)
}

struct ApiError(StatusCode, &'static str, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1, "message": self.2 }))).into_response()
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        let status = match &e {
            EngineError::BadRequest { .. } => StatusCode::BAD_REQUEST,
            EngineError::NotFound => StatusCode::NOT_FOUND,
            EngineError::Conflict { .. } => StatusCode::CONFLICT,
            EngineError::Backend(err) => {
                tracing::error!(error = format!("{err:#}"), "backend failure");
                StatusCode::BAD_GATEWAY
            }
        };
        let message = match &e {
            EngineError::Backend(err) => format!("{err:#}"),
            other => other.to_string(),
        };
        ApiError(status, e.code(), message)
    }
}

/// Bearer auth for everything under /v1, plus a global rate limit on mutating calls.
async fn guard(State(s): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !bool::from(presented.as_bytes().ct_eq(s.token.as_bytes())) {
        return ApiError(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or wrong bearer token".into(),
        )
        .into_response();
    }
    if req.method() != Method::GET {
        let mut w = s.rate.lock().unwrap();
        if w.0.elapsed() > Duration::from_secs(60) {
            *w = (Instant::now(), 0);
        }
        if w.1 >= s.rate_limit {
            return ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "too many changes; retry in a minute".into(),
            )
            .into_response();
        }
        w.1 += 1;
    }
    next.run(req).await
}

fn check_id(id: &str) -> Result<(), ApiError> {
    let ok = (1..=128).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:-".contains(c));
    if ok {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_external_id",
            "external id must be 1-128 of [A-Za-z0-9._:-]".into(),
        ))
    }
}

fn audited(
    s: &AppState,
    op: &str,
    id: &str,
    r: Result<OperationResult, EngineError>,
) -> Result<Json<OperationResult>, ApiError> {
    let now = (s.now)();
    match r {
        Ok(res) => {
            s.audit.record(&now, "api", op, id, Ok(&res.steps));
            Ok(Json(res))
        }
        Err(e) => {
            s.audit.record(&now, "api", op, id, Err(e.code()));
            Err(e.into())
        }
    }
}

async fn info(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(s.info.clone())
}

async fn shared_mailboxes(State(s): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(s.info["sharedMailboxes"].clone())
}

async fn list(State(s): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(json!(s.engine.list()?)))
}

async fn get_identity(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_id(&id)?;
    Ok(Json(json!(s.engine.get(&id)?)))
}

async fn inspect(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    check_id(&id)?;
    Ok(Json(json!(s.engine.inspect(&id).await?)))
}

async fn provision(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<ProvisionRequest>,
) -> Result<Json<OperationResult>, ApiError> {
    check_id(&id)?;
    audited(&s, "provision", &id, s.engine.provision(&id, &req).await)
}

async fn disable(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<OperationResult>, ApiError> {
    check_id(&id)?;
    audited(&s, "disable", &id, s.engine.disable(&id).await)
}

async fn enable(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<OperationResult>, ApiError> {
    check_id(&id)?;
    audited(&s, "enable", &id, s.engine.enable(&id).await)
}

async fn rotate(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<OperationResult>, ApiError> {
    check_id(&id)?;
    audited(&s, "rotate", &id, s.engine.rotate(&id).await)
}

async fn purge(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<OperationResult>, ApiError> {
    check_id(&id)?;
    audited(&s, "purge", &id, s.engine.purge(&id).await)
}
