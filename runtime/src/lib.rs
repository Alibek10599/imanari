#![forbid(unsafe_code)]

pub use serde_json;
pub use sqlx;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Row, Sqlite, SqliteConnection, Transaction};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, Semaphore, watch};

pub const RUNTIME_INTERFACE: &str = "imanari-runtime/0.1";
pub const MAX_SAFE_INT: i64 = 9_007_199_254_740_991;
pub const EXIT_CONFIG: u8 = 77;
pub const EXIT_MIGRATION_REQUIRED: u8 = 78;
const SESSION_TTL: Duration = Duration::from_secs(8 * 3600);
const BOOTSTRAP_TTL: Duration = Duration::from_secs(300);
const MAX_CONCURRENT: usize = 100;
const MUTATIONS_PER_MINUTE: usize = 60;
const MAX_MUTATIONS_PER_MINUTE: usize = 1_000_000;
pub const SEEDED_USERS: [(&str, &str, &str); 2] = [
    ("a", "User A", "00000000-0000-4000-8000-00000000000a"),
    ("b", "User B", "00000000-0000-4000-8000-00000000000b"),
];
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";

pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, ApiError>> + Send>>;
pub type Handler = fn(Ctx, Input) -> HandlerFuture;

pub struct ActionSpec {
    pub name: &'static str,
    pub mutates: bool,
    pub handler: Handler,
}

pub struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub body: &'static str,
}

pub struct AppSpec {
    pub name: &'static str,
    pub source_sha256: &'static str,
    pub semantic_sha256: &'static str,
    pub schema: &'static [&'static str],
    pub schema_fingerprint: &'static str,
    pub schema_shape: &'static str,
    pub database: &'static str,
    pub port: u16,
    pub request_limit_bytes: usize,
    pub request_timeout_ms: u64,
    pub routes: &'static [&'static str],
    pub index_html: &'static str,
    pub assets: &'static [Asset],
    pub actions: &'static [ActionSpec],
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub field: Option<String>,
}

impl ApiError {
    pub fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            field: None,
        }
    }

    pub fn invalid(field: &str, message: impl Into<String>) -> Self {
        ApiError {
            field: Some(field.into()),
            ..ApiError::new(400, "INVALID_INPUT", message)
        }
    }

    pub fn not_found() -> Self {
        ApiError::new(404, "NOT_FOUND", "record not found")
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        let busy = match &e {
            sqlx::Error::PoolTimedOut => true,
            sqlx::Error::Database(d) => matches!(d.code().as_deref(), Some("5" | "6" | "517")),
            _ => false,
        };
        if busy {
            ApiError::new(
                503,
                "UNAVAILABLE",
                "database temporarily unavailable; retry with the same Idempotency-Key",
            )
        } else {
            log(&format!("database error: {e}"));
            ApiError::new(500, "INTERNAL", "internal error")
        }
    }
}

fn log(msg: &str) {
    let _ = writeln!(std::io::stderr(), "[nari-app] {msg}");
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("operating system randomness is available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn new_uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("operating system randomness is available");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub fn is_canonical_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct Input {
    map: Map<String, Value>,
}

impl Input {
    pub fn only(&self, allowed: &[&str]) -> Result<(), ApiError> {
        match self.map.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => Err(ApiError::invalid(k, format!("unknown input field '{k}'"))),
            None => Ok(()),
        }
    }

    fn get(&self, name: &str) -> Result<&Value, ApiError> {
        self.map
            .get(name)
            .ok_or_else(|| ApiError::invalid(name, format!("'{name}' is required")))
    }

    pub fn text(&self, name: &str, min: usize, max: usize) -> Result<String, ApiError> {
        match self.get(name)? {
            Value::String(s) => {
                check_text(name, s, min, max)?;
                Ok(s.clone())
            }
            _ => Err(ApiError::invalid(name, format!("'{name}' must be text"))),
        }
    }

    pub fn boolean(&self, name: &str) -> Result<bool, ApiError> {
        self.get(name)?
            .as_bool()
            .ok_or_else(|| ApiError::invalid(name, format!("'{name}' must be true or false")))
    }

    pub fn uuid(&self, name: &str) -> Result<String, ApiError> {
        match self.get(name)? {
            Value::String(s) if is_canonical_uuid(s) => Ok(s.clone()),
            _ => Err(ApiError::invalid(
                name,
                format!("'{name}' must be a canonical lowercase UUID"),
            )),
        }
    }

    pub fn int(&self, name: &str) -> Result<i64, ApiError> {
        match self.get(name)?.as_i64() {
            Some(n) if (-MAX_SAFE_INT..=MAX_SAFE_INT).contains(&n) => Ok(n),
            _ => Err(ApiError::invalid(
                name,
                format!("'{name}' must be an integer within ±{MAX_SAFE_INT}"),
            )),
        }
    }

    pub fn into_value(self) -> Value {
        Value::Object(self.map)
    }

    pub fn page(&self) -> Result<(i64, Option<String>), ApiError> {
        self.only(&["limit", "after"])?;
        let limit = match self.map.get("limit") {
            None => 50,
            Some(v) => match v.as_i64() {
                Some(n) if (1..=100).contains(&n) => n,
                _ => {
                    return Err(ApiError::invalid(
                        "limit",
                        "'limit' must be an integer in 1..100",
                    ));
                }
            },
        };
        let after = match self.map.get("after") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if is_canonical_uuid(s) => Some(s.clone()),
            Some(_) => {
                return Err(ApiError::invalid(
                    "after",
                    "'after' must be a canonical lowercase UUID",
                ));
            }
        };
        Ok((limit, after))
    }
}

pub fn check_text(name: &str, s: &str, min: usize, max: usize) -> Result<(), ApiError> {
    let n = s.chars().count();
    if n < min || n > max {
        return Err(ApiError::invalid(
            name,
            format!("'{name}' must have {min}..{max} characters"),
        ));
    }
    Ok(())
}

pub fn input_hash(action: &str, fields: &[(&str, Value)]) -> String {
    let mut sorted: Vec<&(&str, Value)> = fields.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut obj = Map::new();
    for (k, v) in sorted {
        obj.insert((*k).to_string(), v.clone());
    }
    let canonical =
        serde_json::to_string(&json!({ "action": action, "input": obj })).unwrap_or_default();
    sha256_hex(canonical.as_bytes())
}

pub fn page<F>(mut rows: Vec<SqliteRow>, limit: i64, convert: F) -> Result<Value, ApiError>
where
    F: Fn(&SqliteRow) -> Result<Value, ApiError>,
{
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let items: Vec<Value> = rows.iter().map(convert).collect::<Result<_, _>>()?;
    let next = if more {
        items
            .last()
            .and_then(|v| v.get("id"))
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(json!({ "items": items, "next_cursor": next }))
}

pub fn col_text(row: &SqliteRow, i: usize) -> Result<Value, ApiError> {
    Ok(Value::String(row.try_get::<String, _>(i)?))
}

pub fn col_int(row: &SqliteRow, i: usize) -> Result<Value, ApiError> {
    Ok(Value::from(row.try_get::<i64, _>(i)?))
}

pub fn col_bool(row: &SqliteRow, i: usize) -> Result<Value, ApiError> {
    Ok(Value::Bool(row.try_get::<i64, _>(i)? != 0))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FaultPoint {
    BeforeCommit,
    AfterCommit,
    OnRequest,
}

struct Shared {
    spec: &'static AppSpec,
    db: SqlitePool,
    auth: Mutex<Auth>,
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    permits: Arc<Semaphore>,
    writer: Arc<AsyncMutex<()>>,
    hosts: Vec<String>,
    origins: Vec<String>,
    instance: String,
    owner_hash: Option<String>,
    owner_token: Option<String>,
    fault: Option<(FaultPoint, String)>,
    mutations_per_minute: usize,
    shutdown: watch::Sender<bool>,
}

struct Session {
    actor: Option<(&'static str, &'static str)>,
    csrf: String,
    controller: bool,
    expires: Instant,
}

struct Auth {
    sessions: HashMap<String, Session>,
    user_tokens: Vec<String>,
    bootstrap: Option<(String, Instant)>,
}

#[derive(Clone)]
pub struct Ctx {
    shared: Arc<Shared>,
    actor: &'static str,
    action: &'static str,
    key: Option<String>,
}

pub enum Begin {
    Replay(Value),
    Fresh(Tx),
}

pub struct Tx {
    tx: Transaction<'static, Sqlite>,
    hash: String,
    _writer: OwnedMutexGuard<()>,
}

impl Tx {
    pub fn conn(&mut self) -> &mut SqliteConnection {
        &mut self.tx
    }
}

impl Ctx {
    pub fn actor(&self) -> &'static str {
        self.actor
    }

    pub fn db(&self) -> &SqlitePool {
        &self.shared.db
    }

    fn fault(&self, point: FaultPoint) {
        if let Some((p, action)) = &self.shared.fault
            && *p == point
            && action == self.action
        {
            log(&format!("fault injection {point:?} in {}", self.action));
            std::process::abort();
        }
    }

    pub async fn begin_mutation(&self, input_hash: String) -> Result<Begin, ApiError> {
        let key = self.key.as_deref().unwrap_or_default();
        let writer = self.shared.writer.clone().lock_owned().await;
        let mut tx = self.shared.db.begin_with("BEGIN IMMEDIATE").await?;
        let found: Option<(String, String)> = sqlx::query_as(
            "SELECT \"input_sha256\", \"response\" FROM \"_nari_receipts\" WHERE \"actor\" = ?1 AND \"action\" = ?2 AND \"key\" = ?3",
        )
        .bind(self.actor)
        .bind(self.action)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((hash, response)) = found {
            tx.rollback().await?;
            if hash != input_hash {
                return Err(ApiError::new(
                    409,
                    "IDEMPOTENCY_CONFLICT",
                    "this Idempotency-Key was already used with different input",
                ));
            }
            let data = serde_json::from_str(&response)
                .map_err(|_| ApiError::new(500, "INTERNAL", "internal error"))?;
            return Ok(Begin::Replay(data));
        }
        Ok(Begin::Fresh(Tx {
            tx,
            hash: input_hash,
            _writer: writer,
        }))
    }

    pub async fn commit_mutation(&self, mut tx: Tx, data: Value) -> Result<Value, ApiError> {
        let key = self.key.as_deref().unwrap_or_default();
        sqlx::query(
            "INSERT INTO \"_nari_receipts\" (\"actor\", \"action\", \"key\", \"input_sha256\", \"response\", \"created_ms\") VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(self.actor)
        .bind(self.action)
        .bind(key)
        .bind(tx.hash.clone())
        .bind(data.to_string())
        .bind(now_ms())
        .execute(&mut *tx.tx)
        .await?;
        self.fault(FaultPoint::BeforeCommit);
        tx.tx.commit().await?;
        self.fault(FaultPoint::AfterCommit);
        Ok(data)
    }

    pub async fn update_miss(
        &self,
        mut tx: Tx,
        exists_sql: &str,
        id: &str,
        expected: i64,
    ) -> ApiError {
        let current: Result<Option<i64>, sqlx::Error> = sqlx::query_scalar(exists_sql)
            .bind(id)
            .bind(self.actor)
            .fetch_optional(tx.conn())
            .await;
        let _ = tx.tx.rollback().await;
        match current {
            Err(e) => e.into(),
            Ok(None) => ApiError::not_found(),
            Ok(Some(rev)) if rev == expected && rev >= MAX_SAFE_INT => ApiError::new(
                409,
                "REVISION_EXHAUSTED",
                "record revision cannot be incremented further",
            ),
            Ok(Some(_)) => ApiError::new(
                409,
                "REVISION_CONFLICT",
                "the record changed since it was loaded; refresh and retry",
            ),
        }
    }
}

fn json_response(status: u16, body: Value) -> Response {
    let mut resp = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        body.to_string(),
    )
        .into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp
}

fn error_response(e: ApiError, request_id: &str) -> Response {
    let mut err = json!({ "code": e.code, "message": e.message });
    if let Some(f) = e.field {
        err["field"] = Value::String(f);
    }
    json_response(e.status, json!({ "error": err, "request_id": request_id }))
}

fn secure_headers(mut resp: Response) -> Response {
    let h = resp.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    resp
}

fn header_str(h: &HeaderMap, name: impl axum::http::header::AsHeaderName) -> Option<&str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn cookie<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn session_cookie(name: &str, token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{name}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_TTL.as_secs()
    ))
    .expect("hex token forms a valid header")
}

impl Shared {
    fn guard(&self, h: &HeaderMap, state_changing: bool) -> Result<(), ApiError> {
        let host = header_str(h, header::HOST).unwrap_or_default();
        if h.get_all(header::HOST).iter().count() != 1 || !self.hosts.iter().any(|a| a == host) {
            return Err(ApiError::new(403, "FORBIDDEN", "unexpected Host header"));
        }
        if state_changing {
            if let Some(origin) = header_str(h, header::ORIGIN)
                && !self.origins.iter().any(|o| o == origin)
            {
                return Err(ApiError::new(
                    403,
                    "FORBIDDEN",
                    "cross-origin request rejected",
                ));
            }
            if header_str(h, "sec-fetch-site").is_some_and(|s| s != "same-origin" && s != "none") {
                return Err(ApiError::new(
                    403,
                    "FORBIDDEN",
                    "cross-site request rejected",
                ));
            }
        }
        Ok(())
    }

    fn session<'a>(&self, auth: &'a mut Auth, h: &HeaderMap, name: &str) -> Option<&'a Session> {
        let token = cookie(h, name)?;
        let expired = auth
            .sessions
            .get(token)
            .is_some_and(|s| s.expires <= Instant::now());
        if expired {
            auth.sessions.remove(token);
            return None;
        }
        auth.sessions.get(token)
    }

    fn actor(&self, h: &HeaderMap) -> Result<&'static str, ApiError> {
        let mut auth = self.auth.lock().expect("auth lock");
        let s = self
            .session(&mut auth, h, "nari_session")
            .filter(|s| !s.controller)
            .ok_or_else(|| {
                ApiError::new(
                    401,
                    "UNAUTHENTICATED",
                    "sign in through the local development bootstrap",
                )
            })?;
        let csrf = header_str(h, "x-nari-csrf").unwrap_or_default();
        if !same(csrf, &s.csrf) {
            return Err(ApiError::new(
                403,
                "FORBIDDEN",
                "missing or invalid X-Nari-CSRF header",
            ));
        }
        s.actor
            .map(|(_, id)| id)
            .ok_or_else(|| ApiError::new(401, "UNAUTHENTICATED", "session has no actor"))
    }

    fn rate_limited(&self, actor: &str) -> bool {
        let mut rate = self.rate.lock().expect("rate lock");
        let q = rate.entry(actor.to_string()).or_default();
        let now = Instant::now();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= self.mutations_per_minute {
            return true;
        }
        q.push_back(now);
        false
    }
}

async fn drain(body: Body, limit: usize) -> Result<axum::body::Bytes, ApiError> {
    axum::body::to_bytes(body, limit).await.map_err(|_| {
        ApiError::new(
            413,
            "PAYLOAD_TOO_LARGE",
            format!("request body exceeds {limit} bytes"),
        )
    })
}

fn parse_json(
    bytes: Result<axum::body::Bytes, ApiError>,
    h: &HeaderMap,
) -> Result<Map<String, Value>, ApiError> {
    let bytes = bytes?;
    let ctype = header_str(h, header::CONTENT_TYPE).unwrap_or_default();
    if !ctype
        .split(';')
        .next()
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(ApiError::new(
            400,
            "INVALID_INPUT",
            "Content-Type must be application/json",
        ));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(ApiError::new(
            400,
            "INVALID_INPUT",
            "request body must be a JSON object",
        )),
        Err(_) => Err(ApiError::new(
            400,
            "INVALID_INPUT",
            "request body is not valid JSON",
        )),
    }
}

async fn action_route(
    State(app): State<Arc<Shared>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let request_id = new_uuid();
    let result = match app.permits.clone().try_acquire_owned() {
        Ok(permit) => {
            let bytes = drain(body, app.spec.request_limit_bytes).await;
            let r = handle_action(app, &name, &headers, bytes).await;
            drop(permit);
            r
        }
        Err(_) => Err(ApiError::new(
            429,
            "RATE_LIMITED",
            "server is at its concurrent request limit",
        )),
    };
    let resp = match result {
        Ok(data) => json_response(200, json!({ "data": data, "request_id": request_id })),
        Err(e) => error_response(e, &request_id),
    };
    secure_headers(resp)
}

async fn handle_action(
    app: Arc<Shared>,
    name: &str,
    h: &HeaderMap,
    body: Result<axum::body::Bytes, ApiError>,
) -> Result<Value, ApiError> {
    app.guard(h, true)?;
    let spec = app
        .spec
        .actions
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| ApiError::new(404, "UNKNOWN_ACTION", "no such action"))?;
    let actor = app.actor(h)?;
    let key = if spec.mutates {
        let key = header_str(h, "idempotency-key").unwrap_or_default();
        if key.is_empty() || key.len() > 128 || !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ApiError::new(
                400,
                "INVALID_INPUT",
                "mutations require an Idempotency-Key header of 1..128 visible ASCII characters",
            ));
        }
        if app.rate_limited(actor) {
            return Err(ApiError::new(
                429,
                "RATE_LIMITED",
                "too many mutations; retry later",
            ));
        }
        Some(key.to_string())
    } else {
        None
    };
    let map = parse_json(body, h)?;
    let ctx = Ctx {
        shared: app.clone(),
        actor,
        action: spec.name,
        key,
    };
    ctx.fault(FaultPoint::OnRequest);
    let timeout = Duration::from_millis(app.spec.request_timeout_ms);
    match tokio::time::timeout(timeout, (spec.handler)(ctx, Input { map })).await {
        Ok(r) => r,
        Err(_) => Err(ApiError::new(
            503,
            "TIMEOUT",
            "request timed out; the write may or may not have committed, retry with the same Idempotency-Key",
        )),
    }
}

async fn bootstrap_route(
    State(app): State<Arc<Shared>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let rid = new_uuid();
    let bytes = drain(body, 1024).await;
    let result: Result<Response, ApiError> = async {
        app.guard(&headers, true)?;
        let map = parse_json(bytes, &headers)?;
        let token = map.get("token").and_then(Value::as_str).unwrap_or_default();
        let mut auth = app.auth.lock().expect("auth lock");
        let valid =
            matches!(&auth.bootstrap, Some((t, exp)) if same(t, token) && *exp > Instant::now());
        if !valid {
            return Err(ApiError::new(
                401,
                "UNAUTHENTICATED",
                "bootstrap token is invalid, used or expired",
            ));
        }
        auth.bootstrap = None;
        let controller = random_hex(32);
        auth.sessions.insert(
            controller.clone(),
            Session {
                actor: None,
                csrf: random_hex(32),
                controller: true,
                expires: Instant::now() + SESSION_TTL,
            },
        );
        let user = auth.user_tokens[0].clone();
        let mut resp = json_response(200, json!({ "data": { "ok": true }, "request_id": rid }));
        resp.headers_mut()
            .append(header::SET_COOKIE, session_cookie("nari_dev", &controller));
        resp.headers_mut()
            .append(header::SET_COOKIE, session_cookie("nari_session", &user));
        Ok(resp)
    }
    .await;
    secure_headers(result.unwrap_or_else(|e| error_response(e, &rid)))
}

async fn select_route(State(app): State<Arc<Shared>>, headers: HeaderMap, body: Body) -> Response {
    let rid = new_uuid();
    let bytes = drain(body, 1024).await;
    let result: Result<Response, ApiError> = async {
        app.guard(&headers, true)?;
        {
            let mut auth = app.auth.lock().expect("auth lock");
            let s = app
                .session(&mut auth, &headers, "nari_dev")
                .filter(|s| s.controller)
                .ok_or_else(|| {
                    ApiError::new(
                        401,
                        "UNAUTHENTICATED",
                        "development controller session required",
                    )
                })?;
            if !same(
                header_str(&headers, "x-nari-csrf").unwrap_or_default(),
                &s.csrf,
            ) {
                return Err(ApiError::new(
                    403,
                    "FORBIDDEN",
                    "missing or invalid X-Nari-CSRF header",
                ));
            }
        }
        let map = parse_json(bytes, &headers)?;
        let user = map.get("user").and_then(Value::as_str).unwrap_or_default();
        let idx = SEEDED_USERS
            .iter()
            .position(|(k, _, _)| *k == user)
            .ok_or_else(|| ApiError::invalid("user", "unknown development user"))?;
        let token = app.auth.lock().expect("auth lock").user_tokens[idx].clone();
        let mut resp = json_response(200, json!({ "data": { "user": user }, "request_id": rid }));
        resp.headers_mut()
            .append(header::SET_COOKIE, session_cookie("nari_session", &token));
        Ok(resp)
    }
    .await;
    secure_headers(result.unwrap_or_else(|e| error_response(e, &rid)))
}

async fn session_route(State(app): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let rid = new_uuid();
    if let Err(e) = app.guard(&headers, false) {
        return secure_headers(error_response(e, &rid));
    }
    let mut auth = app.auth.lock().expect("auth lock");
    let user = app
        .session(&mut auth, &headers, "nari_session")
        .filter(|s| !s.controller)
        .map(|s| {
            let (label, id) = s.actor.unwrap_or(("", ""));
            json!({ "id": id, "label": label, "csrf": s.csrf })
        });
    let dev = app
        .session(&mut auth, &headers, "nari_dev")
        .filter(|s| s.controller)
        .map(|s| {
            let users: Vec<Value> = SEEDED_USERS
                .iter()
                .map(|(k, label, id)| json!({ "key": k, "label": label, "id": id }))
                .collect();
            json!({ "csrf": s.csrf, "users": users })
        });
    let resp = if user.is_none() && dev.is_none() {
        error_response(ApiError::new(401, "UNAUTHENTICATED", "no session"), &rid)
    } else {
        json_response(
            200,
            json!({ "data": { "user": user, "dev": dev }, "request_id": rid }),
        )
    };
    secure_headers(resp)
}

async fn health_route(State(app): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if let Err(e) = app.guard(&headers, false) {
        return secure_headers(error_response(e, &new_uuid()));
    }
    secure_headers(json_response(
        200,
        json!({
            "status": "ready",
            "app": app.spec.name,
            "instance": app.instance,
            "pid": std::process::id(),
            "owner_sha256": app.owner_hash,
            "source_sha256": app.spec.source_sha256,
            "runtime": RUNTIME_INTERFACE,
        }),
    ))
}

async fn shutdown_route(State(app): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let rid = new_uuid();
    let token = header_str(&headers, "x-nari-owner").unwrap_or_default();
    let ok = app.guard(&headers, true).is_ok()
        && app.owner_token.as_deref().is_some_and(|t| same(t, token));
    if !ok {
        return secure_headers(error_response(
            ApiError::new(403, "FORBIDDEN", "owner token required"),
            &rid,
        ));
    }
    let _ = app.shutdown.send(true);
    secure_headers(json_response(
        200,
        json!({ "data": { "stopping": true }, "request_id": rid }),
    ))
}

fn page_response(content_type: &'static str, body: &'static str) -> Response {
    let mut resp = body.into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    secure_headers(resp)
}

async fn static_route(
    State(app): State<Arc<Shared>>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    let rid = new_uuid();
    if let Err(e) = app.guard(&headers, false) {
        return secure_headers(error_response(e, &rid));
    }
    let path = uri.path();
    if method == Method::GET {
        if app.spec.routes.contains(&path) {
            return page_response("text/html; charset=utf-8", app.spec.index_html);
        }
        if let Some(a) = app.spec.assets.iter().find(|a| a.path == path) {
            return page_response(a.content_type, a.body);
        }
    }
    secure_headers(error_response(
        ApiError::new(404, "NOT_FOUND", "not found"),
        &rid,
    ))
}

pub struct Config {
    pub mutations_per_minute: usize,
    pub database: PathBuf,
    pub port: u16,
    pub owner_token: Option<String>,
    pub supervised: bool,
    pub fault: Option<String>,
}

impl Config {
    pub fn from_env(spec: &AppSpec) -> Result<Config, String> {
        let database = std::env::var("NARI_DATABASE").unwrap_or_else(|_| spec.database.to_string());
        let port = match std::env::var("NARI_PORT") {
            Ok(p) => p.parse().map_err(|_| format!("invalid NARI_PORT '{p}'"))?,
            Err(_) => spec.port,
        };
        Ok(Config {
            database: PathBuf::from(database),
            port,
            owner_token: std::env::var("NARI_OWNER_TOKEN")
                .ok()
                .filter(|t| !t.is_empty()),
            supervised: std::env::var("NARI_SUPERVISED").is_ok_and(|v| v == "1"),
            fault: std::env::var("NARI_FAULT").ok().filter(|f| !f.is_empty()),
            mutations_per_minute: match std::env::var("NARI_MUTATIONS_PER_MINUTE") {
                Ok(v) => v
                    .parse()
                    .ok()
                    .filter(|n| (1..=MAX_MUTATIONS_PER_MINUTE).contains(n))
                    .ok_or_else(|| format!("invalid NARI_MUTATIONS_PER_MINUTE '{v}'"))?,
                Err(_) => MUTATIONS_PER_MINUTE,
            },
        })
    }
}

fn parse_fault(f: &str) -> Result<Option<(FaultPoint, String)>, String> {
    let (point, action) = f.split_once(':').unwrap_or((f, ""));
    let p = match point {
        "crash_before_commit" => FaultPoint::BeforeCommit,
        "crash_after_commit" => FaultPoint::AfterCommit,
        "crash_on_request" => FaultPoint::OnRequest,
        "crash_on_start" => return Ok(None),
        _ => return Err(format!("unknown NARI_FAULT '{f}'")),
    };
    Ok(Some((p, action.to_string())))
}

fn check_db_path(path: &std::path::Path) -> Result<(), String> {
    let checked = if path.is_absolute() { 1 } else { usize::MAX };
    for ancestor in path.ancestors().skip(1).take(checked) {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        if std::fs::symlink_metadata(ancestor).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!(
                "database path component {} is a symlink",
                ancestor.display()
            ));
        }
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("database {} is a symlink", path.display()));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityShape {
    pub name: String,
    pub owner: String,
    pub create: Vec<String>,
    pub fields: Vec<FieldShape>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FieldShape {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: Value,
    pub add: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MigrationStep {
    pub summary: String,
    pub sql: Vec<String>,
}

fn type_label(t: &Value) -> String {
    let name = t["name"].as_str().unwrap_or("?");
    match (t["min_length"].as_i64(), t["max_length"].as_i64()) {
        (Some(min), Some(max)) => format!("{name} {min}..{max}"),
        _ => name.to_string(),
    }
}

fn widens(old: &Value, new: &Value) -> bool {
    let bounds = |t: &Value| {
        (t["name"] == "Text").then(|| (t["min_length"].as_i64(), t["max_length"].as_i64()))
    };
    match (bounds(old), bounds(new)) {
        (Some((Some(omin), Some(omax))), Some((Some(nmin), Some(nmax)))) => {
            nmin <= omin && nmax >= omax
        }
        _ => false,
    }
}

pub fn migration_plan(
    old: &[EntityShape],
    new: &[EntityShape],
) -> Result<Vec<MigrationStep>, Vec<String>> {
    let mut steps = Vec::new();
    let mut blocked = Vec::new();
    for o in old.iter().filter(|o| !new.iter().any(|n| n.name == o.name)) {
        blocked.push(format!(
            "entity {} was removed, which would drop its stored rows",
            o.name
        ));
    }
    for n in new {
        let Some(o) = old.iter().find(|o| o.name == n.name) else {
            steps.push(MigrationStep {
                summary: format!("create entity {}", n.name),
                sql: n.create.clone(),
            });
            continue;
        };
        if o.owner != n.owner {
            blocked.push(format!(
                "owner field of {} changed from {} to {}, which would change who owns stored rows",
                n.name, o.owner, n.owner
            ));
        }
        for of in o
            .fields
            .iter()
            .filter(|f| !n.fields.iter().any(|g| g.name == f.name))
        {
            blocked.push(format!(
                "field {}.{} was removed, which would drop its stored values",
                n.name, of.name
            ));
        }
        for nf in &n.fields {
            let path = format!("{}.{}", n.name, nf.name);
            match (o.fields.iter().find(|f| f.name == nf.name), &nf.add) {
                (None, Some(sql)) => steps.push(MigrationStep {
                    summary: format!("add field {path}"),
                    sql: vec![sql.clone()],
                }),
                (None, None) => blocked.push(format!(
                    "field {path} was added without a default, so existing rows have no value for it"
                )),
                (Some(of), _) if of.ty == nf.ty => {}
                (Some(of), _) if widens(&of.ty, &nf.ty) => steps.push(MigrationStep {
                    summary: format!(
                        "widen {path} from {} to {}",
                        type_label(&of.ty),
                        type_label(&nf.ty)
                    ),
                    sql: Vec::new(),
                }),
                (Some(of), _) => blocked.push(format!(
                    "type of {path} changed from {} to {}, which stored values may not satisfy",
                    type_label(&of.ty),
                    type_label(&nf.ty)
                )),
            }
        }
    }
    if blocked.is_empty() {
        Ok(steps)
    } else {
        Err(blocked)
    }
}

pub enum OpenError {
    Config(String),
    MigrationRequired(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for OpenError {
    fn from(e: sqlx::Error) -> Self {
        OpenError::Db(e)
    }
}

pub async fn open_database(
    spec: &AppSpec,
    path: &std::path::Path,
) -> Result<SqlitePool, OpenError> {
    check_db_path(path).map_err(OpenError::Config)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| OpenError::Config(format!("cannot create {}: {e}", parent.display())))?;
        check_db_path(path).map_err(OpenError::Config)?;
    }
    let existed = path.exists();
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5))
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    let has_meta: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_nari_meta'",
    )
    .fetch_optional(&pool)
    .await?;
    if has_meta.is_none() {
        let tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table'")
                .fetch_one(&pool)
                .await?;
        if existed && tables > 0 {
            return Err(OpenError::MigrationRequired(
                "database contains tables but no Imanari schema fingerprint".into(),
            ));
        }
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("CREATE TABLE \"_nari_meta\" (\"key\" TEXT PRIMARY KEY NOT NULL, \"value\" TEXT NOT NULL) STRICT")
            .execute(&mut *tx)
            .await?;
        sqlx::query("CREATE TABLE \"_nari_receipts\" (\"actor\" TEXT NOT NULL, \"action\" TEXT NOT NULL, \"key\" TEXT NOT NULL, \"input_sha256\" TEXT NOT NULL, \"response\" TEXT NOT NULL, \"created_ms\" INTEGER NOT NULL, PRIMARY KEY (\"actor\", \"action\", \"key\")) STRICT")
            .execute(&mut *tx)
            .await?;
        for stmt in spec.schema {
            sqlx::query(stmt).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO \"_nari_meta\" (\"key\", \"value\") VALUES ('schema_fingerprint', ?1), ('app', ?2), ('schema_shape', ?3)")
            .bind(spec.schema_fingerprint)
            .bind(spec.name)
            .bind(spec.schema_shape)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(pool);
    }
    let meta = |key: &'static str| {
        sqlx::query_scalar::<_, String>("SELECT \"value\" FROM \"_nari_meta\" WHERE \"key\" = ?1")
            .bind(key)
            .fetch_optional(&pool)
    };
    let Some(stored) = meta("schema_fingerprint").await? else {
        return Err(OpenError::MigrationRequired(
            "schema fingerprint missing".into(),
        ));
    };
    let stored_shape = meta("schema_shape").await?;
    if stored == spec.schema_fingerprint {
        if stored_shape.is_none() {
            sqlx::query(
                "INSERT INTO \"_nari_meta\" (\"key\", \"value\") VALUES ('schema_shape', ?1)",
            )
            .bind(spec.schema_shape)
            .execute(&pool)
            .await?;
        }
        return Ok(pool);
    }
    let Some(old) = stored_shape else {
        return Err(OpenError::MigrationRequired(format!(
            "stored schema fingerprint {stored} differs from compiled schema {}, and the database predates recorded schema shapes; start the app once with the schema it was created with so the shape is recorded, then apply the change",
            spec.schema_fingerprint
        )));
    };
    let old: Vec<EntityShape> = serde_json::from_str(&old).map_err(|e| {
        OpenError::MigrationRequired(format!("stored schema shape is unreadable: {e}"))
    })?;
    let new: Vec<EntityShape> = serde_json::from_str(spec.schema_shape)
        .map_err(|e| OpenError::Config(format!("compiled schema shape is invalid: {e}")))?;
    let steps = migration_plan(&old, &new)
        .map_err(|blocked| OpenError::MigrationRequired(blocked.join("; ")))?;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let claimed = sqlx::query(
        "UPDATE \"_nari_meta\" SET \"value\" = ?1 WHERE \"key\" = 'schema_fingerprint' AND \"value\" = ?2",
    )
    .bind(spec.schema_fingerprint)
    .bind(&stored)
    .execute(&mut *tx)
    .await?;
    if claimed.rows_affected() != 1 {
        return Err(OpenError::MigrationRequired(
            "schema fingerprint changed while migrating".into(),
        ));
    }
    for sql in steps.iter().flat_map(|s| &s.sql) {
        sqlx::query(sql).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE \"_nari_meta\" SET \"value\" = ?1 WHERE \"key\" = 'schema_shape'")
        .bind(spec.schema_shape)
        .execute(&mut *tx)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS \"_nari_schema_history\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"from_fingerprint\" TEXT NOT NULL, \"to_fingerprint\" TEXT NOT NULL, \"steps\" TEXT NOT NULL, \"applied_ms\" INTEGER NOT NULL) STRICT")
        .execute(&mut *tx)
        .await?;
    let summaries: Vec<&str> = steps.iter().map(|s| s.summary.as_str()).collect();
    sqlx::query("INSERT INTO \"_nari_schema_history\" (\"from_fingerprint\", \"to_fingerprint\", \"steps\", \"applied_ms\") VALUES (?1, ?2, ?3, ?4)")
        .bind(&stored)
        .bind(spec.schema_fingerprint)
        .bind(json!(summaries).to_string())
        .bind(now_ms())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    if summaries.is_empty() {
        log("schema fingerprint updated; no table changes were needed");
    } else {
        log(&format!("schema migrated: {}", summaries.join(", ")));
    }
    Ok(pool)
}

pub fn run(spec: &'static AppSpec) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log(&format!("cannot start async runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(serve(spec))
}

async fn serve(spec: &'static AppSpec) -> ExitCode {
    let cfg = match Config::from_env(spec) {
        Ok(c) => c,
        Err(e) => {
            log(&e);
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    let fault = match cfg.fault.as_deref().map(parse_fault).transpose() {
        Ok(f) => f.flatten(),
        Err(e) => {
            log(&e);
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    if cfg.fault.as_deref() == Some("crash_on_start") {
        log("fault injection crash_on_start");
        std::process::abort();
    }
    let db = match open_database(spec, &cfg.database).await {
        Ok(db) => db,
        Err(OpenError::MigrationRequired(m)) => {
            log(&format!("MIGRATION_REQUIRED: {m}"));
            println!(
                "NARI_STOP {}",
                json!({ "reason": "MIGRATION_REQUIRED", "message": m })
            );
            return ExitCode::from(EXIT_MIGRATION_REQUIRED);
        }
        Err(OpenError::Config(m)) => {
            log(&m);
            return ExitCode::from(EXIT_CONFIG);
        }
        Err(OpenError::Db(e)) => {
            log(&format!("cannot open database: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", cfg.port)).await {
        Ok(l) => l,
        Err(e) => {
            log(&format!("cannot bind 127.0.0.1:{}: {e}", cfg.port));
            return ExitCode::FAILURE;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(cfg.port);
    let mut auth = Auth {
        sessions: HashMap::new(),
        user_tokens: Vec::new(),
        bootstrap: Some((random_hex(32), Instant::now() + BOOTSTRAP_TTL)),
    };
    for (_, label, id) in SEEDED_USERS {
        let token = random_hex(32);
        auth.sessions.insert(
            token.clone(),
            Session {
                actor: Some((label, id)),
                csrf: random_hex(32),
                controller: false,
                expires: Instant::now() + SESSION_TTL,
            },
        );
        auth.user_tokens.push(token);
    }
    let bootstrap = auth
        .bootstrap
        .as_ref()
        .map(|(t, _)| t.clone())
        .unwrap_or_default();
    let (tx, mut rx) = watch::channel(false);
    let shared = Arc::new(Shared {
        spec,
        db,
        auth: Mutex::new(auth),
        rate: Mutex::new(HashMap::new()),
        permits: Arc::new(Semaphore::new(MAX_CONCURRENT)),
        writer: Arc::new(AsyncMutex::new(())),
        hosts: vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")],
        origins: vec![
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
        ],
        instance: new_uuid(),
        owner_hash: cfg.owner_token.as_deref().map(|t| sha256_hex(t.as_bytes())),
        owner_token: cfg.owner_token.clone(),
        fault,
        mutations_per_minute: cfg.mutations_per_minute,
        shutdown: tx,
    });
    let router = Router::new()
        .route("/api/actions/{name}", post(action_route))
        .route("/_nari/bootstrap", post(bootstrap_route))
        .route("/_nari/dev/select", post(select_route))
        .route("/_nari/session", get(session_route))
        .route("/_nari/health", get(health_route))
        .route("/_nari/control/shutdown", post(shutdown_route))
        .fallback(static_route)
        .with_state(shared.clone());
    if cfg.supervised {
        let stop = shared.shutdown.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 64];
            while let Ok(n) = stdin.read(&mut buf).await {
                if n == 0 {
                    break;
                }
            }
            log("supervisor pipe closed; shutting down");
            let _ = stop.send(true);
        });
    }
    let stop = shared.shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop.send(true);
    });
    #[cfg(unix)]
    {
        let stop = shared.shutdown.clone();
        tokio::spawn(async move {
            if let Ok(mut s) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                s.recv().await;
                let _ = stop.send(true);
            }
        });
    }
    println!("NARI_BOOTSTRAP http://127.0.0.1:{port}/#bootstrap={bootstrap}");
    println!(
        "NARI_READY {}",
        json!({ "port": port, "instance": shared.instance, "pid": std::process::id(), "source_sha256": spec.source_sha256 })
    );
    let _ = std::io::stdout().flush();
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = rx.wait_for(|v| *v).await;
        })
        .await;
    shared.db.close().await;
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(&format!("server error: {e}"));
            ExitCode::FAILURE
        }
    }
}
