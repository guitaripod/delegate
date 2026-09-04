use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::{Context, Result, bail};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::config::{Config, Overrides};
use crate::engine::{self, Approver, EventSink, RunRequest};
use crate::events::Envelope;
use crate::packet::{Mode, Packet};
use crate::store::Store;

#[derive(Default)]
pub struct Gate {
    decision: Mutex<Option<bool>>,
    cv: Condvar,
}

impl Gate {
    pub fn resolve(&self, approved: bool) {
        if let Ok(mut d) = self.decision.lock() {
            *d = Some(approved);
        }
        self.cv.notify_all();
    }

    fn wait(&self) -> bool {
        let Ok(mut d) = self.decision.lock() else {
            return false;
        };
        while d.is_none() {
            d = match self.cv.wait(d) {
                Ok(guard) => guard,
                Err(_) => return false,
            };
        }
        d.unwrap_or(false)
    }
}

struct GateApprover(Arc<Gate>);

impl Approver for GateApprover {
    fn approve(&mut self, _tier: &str, _reason: &str) -> bool {
        self.0.wait()
    }
}

struct BroadcastSink(broadcast::Sender<Envelope>);

impl EventSink for BroadcastSink {
    fn emit(&mut self, env: &Envelope) {
        let _ = self.0.send(env.clone());
    }
}

#[derive(Clone)]
struct RunHandle {
    tx: broadcast::Sender<Envelope>,
    gate: Arc<Gate>,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
struct AppState {
    cfg: Arc<Config>,
    store: Arc<Mutex<Store>>,
    runs: Arc<Mutex<HashMap<String, RunHandle>>>,
    expected_auth: Arc<Vec<u8>>,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::BAD_REQUEST, format!("{e:#}"))
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// KEY=VALUE lines; later entries win, `export` prefixes and quotes are tolerated.
pub fn read_env_file(path: &std::path::Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    map
}

pub fn password(cfg: &Config) -> Result<String> {
    if let Ok(v) = std::env::var(&cfg.server.password_env)
        && !v.is_empty()
    {
        return Ok(v);
    }
    if let Some(file) = &cfg.server.env_file {
        let map = read_env_file(&crate::config::expand_home(file));
        if let Some(v) = map.get(&cfg.server.password_env)
            && !v.is_empty()
        {
            return Ok(v.clone());
        }
    }
    bail!(
        "no password: set {} in the environment or in {}",
        cfg.server.password_env,
        cfg.server
            .env_file
            .as_deref()
            .unwrap_or("the configured env file")
    )
}

async fn auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| {
            base64::engine::general_purpose::STANDARD
                .decode(b.trim())
                .ok()
        })
        .map(|decoded| decoded == *state.expected_auth)
        .unwrap_or(false);
    if ok {
        next.run(req).await
    } else {
        let mut resp = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"delegate\""),
        );
        resp
    }
}

#[derive(Serialize)]
struct Health {
    ok: bool,
    version: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Serialize)]
struct Capabilities {
    api: u32,
    version: &'static str,
    host: String,
    features: Vec<&'static str>,
    tiers: Vec<String>,
    classes: Vec<String>,
    modes: Vec<&'static str>,
}

async fn capabilities(State(state): State<AppState>) -> Json<Capabilities> {
    Json(Capabilities {
        api: 1,
        version: env!("CARGO_PKG_VERSION"),
        host: gethostname::gethostname().to_string_lossy().to_string(),
        features: vec![
            "runs", "events", "approve", "cancel", "replay", "stats", "tiers",
        ],
        tiers: state.cfg.order.clone(),
        classes: state.cfg.classes.keys().cloned().collect(),
        modes: vec!["normal", "conserve", "rush"],
    })
}

#[derive(Serialize)]
pub struct TierView {
    pub tier: String,
    pub label: String,
    pub chain: Vec<ChainView>,
}

#[derive(Serialize)]
pub struct ChainView {
    pub runner: String,
    pub model: String,
    pub thinking: Option<String>,
    pub health: Option<String>,
    pub healthy: Option<bool>,
    pub reason: Option<String>,
}

pub fn tier_views(cfg: &Config, probe: bool) -> Vec<TierView> {
    cfg.order
        .iter()
        .map(|name| {
            let tier = &cfg.tiers[name];
            TierView {
                tier: name.clone(),
                label: tier.label.clone().unwrap_or_default(),
                chain: tier
                    .chain
                    .iter()
                    .map(|entry| {
                        let (healthy, reason) = match (&entry.health, probe) {
                            (Some(url), true) => {
                                match crate::health::check(url, cfg.health_timeout_ms) {
                                    Ok(()) => (Some(true), None),
                                    Err(r) => (Some(false), Some(r)),
                                }
                            }
                            _ => (None, None),
                        };
                        ChainView {
                            runner: entry.runner.clone(),
                            model: entry.display_model(),
                            thinking: entry.thinking.clone(),
                            health: entry.health.clone(),
                            healthy,
                            reason,
                        }
                    })
                    .collect(),
            }
        })
        .collect()
}

async fn tiers(State(state): State<AppState>) -> Json<Vec<TierView>> {
    let cfg = state.cfg.clone();
    let views = tokio::task::spawn_blocking(move || tier_views(&cfg, true))
        .await
        .unwrap_or_default();
    Json(views)
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

async fn list_runs(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let store = state
        .store
        .lock()
        .map_err(|_| internal("store lock poisoned"))?;
    let rows = store
        .list_runs(q.limit.unwrap_or(50).min(500))
        .map_err(internal)?;
    Ok(Json(rows).into_response())
}

#[derive(Deserialize)]
struct StartBody {
    packet: Packet,
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    ceiling: Option<String>,
    #[serde(default)]
    mode: Option<Mode>,
    #[serde(default)]
    attempts: Option<u32>,
}

#[derive(Serialize)]
struct Started {
    run_id: String,
}

fn start_run(state: &AppState, packet: Packet, overrides: Overrides) -> Result<String> {
    packet.validate()?;
    state.cfg.plan(&packet, &overrides)?;
    engine::resolve_repo(&packet)?;
    let run_id = ulid::Ulid::generate().to_string();
    let (tx, _rx) = broadcast::channel(1024);
    let handle = RunHandle {
        tx: tx.clone(),
        gate: Arc::new(Gate::default()),
        cancel: Arc::new(AtomicBool::new(false)),
    };
    state
        .runs
        .lock()
        .map_err(|_| anyhow::anyhow!("runs lock poisoned"))?
        .insert(run_id.clone(), handle.clone());
    let state2 = state.clone();
    let rid = run_id.clone();
    tokio::task::spawn_blocking(move || {
        let mut sink = BroadcastSink(tx);
        let mut approver = GateApprover(handle.gate.clone());
        let result = engine::execute(
            &state2.cfg,
            &state2.store,
            RunRequest {
                run_id: rid.clone(),
                packet,
                overrides,
                keep_worktree: false,
            },
            &mut sink,
            &mut approver,
            &handle.cancel,
        );
        if let Err(e) = result {
            tracing::error!(run = %rid, "run failed to start: {e:#}");
        }
        if let Ok(mut runs) = state2.runs.lock() {
            runs.remove(&rid);
        }
    });
    Ok(run_id)
}

async fn create_run(
    State(state): State<AppState>,
    Json(body): Json<StartBody>,
) -> Result<Response, ApiError> {
    let overrides = Overrides {
        tier: body.tier,
        ceiling: body.ceiling,
        mode: body.mode,
        attempts: body.attempts,
    };
    let run_id = start_run(&state, body.packet, overrides)?;
    Ok((StatusCode::ACCEPTED, Json(Started { run_id })).into_response())
}

#[derive(Serialize)]
struct RunDetail {
    run: crate::store::RunRow,
    attempts: Vec<crate::store::AttemptRow>,
    live: bool,
}

async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let store = state
        .store
        .lock()
        .map_err(|_| internal("store lock poisoned"))?;
    let run = store
        .get_run(&id)
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{e:#}")))?;
    let attempts = store.attempts(&run.id).map_err(internal)?;
    let live = state
        .runs
        .lock()
        .map(|r| r.contains_key(&run.id))
        .unwrap_or(false);
    Ok(Json(RunDetail {
        run,
        attempts,
        live,
    })
    .into_response())
}

#[derive(Deserialize)]
struct EventsQuery {
    after: Option<u64>,
}

fn sse_event(env: &Envelope) -> Event {
    let data = serde_json::to_string(env).unwrap_or_else(|_| "{}".to_string());
    Event::default()
        .id(env.seq.to_string())
        .event("run")
        .data(data)
}

type EventStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    let after = q.after.unwrap_or(0);
    let (run_id, live_rx) = {
        let runs = state
            .runs
            .lock()
            .map_err(|_| internal("runs lock poisoned"))?;
        let store = state
            .store
            .lock()
            .map_err(|_| internal("store lock poisoned"))?;
        let run = store
            .get_run(&id)
            .map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{e:#}")))?;
        (run.id.clone(), runs.get(&run.id).map(|h| h.tx.subscribe()))
    };
    let past = {
        let store = state
            .store
            .lock()
            .map_err(|_| internal("store lock poisoned"))?;
        store.events(&run_id, after).map_err(internal)?
    };
    let last_seq = past.last().map(|e| e.seq).unwrap_or(after);
    let past_stream = tokio_stream::iter(past.into_iter().map(|e| Ok(sse_event(&e))));
    let stream: EventStream = match live_rx {
        Some(rx) => {
            let live = BroadcastStream::new(rx)
                .filter_map(move |item| item.ok().filter(|e| e.seq > last_seq))
                .map(|e| Ok(sse_event(&e)));
            Box::pin(past_stream.chain(live))
        }
        None => Box::pin(past_stream),
    };
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

#[derive(Deserialize)]
struct ApproveBody {
    approved: bool,
}

async fn approve(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ApproveBody>,
) -> Result<Response, ApiError> {
    let handle = {
        let runs = state
            .runs
            .lock()
            .map_err(|_| internal("runs lock poisoned"))?;
        runs.get(&id).cloned()
    };
    match handle {
        Some(h) => {
            h.gate.resolve(body.approved);
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        None => Err(ApiError(
            StatusCode::NOT_FOUND,
            "run is not live".to_string(),
        )),
    }
}

async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let handle = {
        let runs = state
            .runs
            .lock()
            .map_err(|_| internal("runs lock poisoned"))?;
        runs.get(&id).cloned()
    };
    match handle {
        Some(h) => {
            h.cancel.store(true, Ordering::Relaxed);
            h.gate.resolve(false);
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        None => Err(ApiError(
            StatusCode::NOT_FOUND,
            "run is not live".to_string(),
        )),
    }
}

#[derive(Deserialize, Default)]
struct ReplayBody {
    #[serde(default)]
    tier: Option<String>,
    #[serde(default)]
    ceiling: Option<String>,
    #[serde(default)]
    mode: Option<Mode>,
    #[serde(default)]
    attempts: Option<u32>,
}

async fn replay(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<ReplayBody>>,
) -> Result<Response, ApiError> {
    let body = body.map(|b| b.0).unwrap_or_default();
    let packet = {
        let store = state
            .store
            .lock()
            .map_err(|_| internal("store lock poisoned"))?;
        store
            .get_run(&id)
            .map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{e:#}")))?
            .packet
    };
    let overrides = Overrides {
        tier: body.tier,
        ceiling: body.ceiling,
        mode: body.mode,
        attempts: body.attempts,
    };
    let run_id = start_run(&state, packet, overrides)?;
    Ok((StatusCode::ACCEPTED, Json(Started { run_id })).into_response())
}

#[derive(Deserialize)]
struct StatsQuery {
    class: Option<String>,
}

async fn stats(
    State(state): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> Result<Response, ApiError> {
    let store = state
        .store
        .lock()
        .map_err(|_| internal("store lock poisoned"))?;
    let rows = store.stats(q.class.as_deref()).map_err(internal)?;
    Ok(Json(rows).into_response())
}

pub async fn serve(cfg: Config, listen: Option<String>) -> Result<()> {
    let pass = password(&cfg)?;
    let expected = format!("{}:{}", cfg.server.user, pass).into_bytes();
    let store = Store::open(&cfg.db_path())?;
    let addr: SocketAddr = listen
        .unwrap_or_else(|| cfg.server.listen.clone())
        .parse()
        .context("invalid listen address")?;
    let state = AppState {
        cfg: Arc::new(cfg),
        store: Arc::new(Mutex::new(store)),
        runs: Arc::new(Mutex::new(HashMap::new())),
        expected_auth: Arc::new(expected),
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/tiers", get(tiers))
        .route("/v1/runs", get(list_runs).post(create_run))
        .route("/v1/runs/{id}", get(get_run))
        .route("/v1/runs/{id}/events", get(events))
        .route("/v1/runs/{id}/approve", post(approve))
        .route("/v1/runs/{id}/cancel", post(cancel))
        .route("/v1/runs/{id}/replay", post(replay))
        .route("/v1/stats", get(stats))
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("delegate {} listening on {addr}", env!("CARGO_PKG_VERSION"));
    axum::serve(listener, app).await.context("server error")?;
    Ok(())
}
