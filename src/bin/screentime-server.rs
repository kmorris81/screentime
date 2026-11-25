//! screentime-server: holds the policy for PCs running in server mode.
//!
//! It keeps one config TOML (the same format as /etc/screentime/config.toml,
//! covering every kid) and today's overrides per user. PCs poll
//! `POST /api/v1/sync`, reporting their users' status and getting the policy
//! back; the `screentime --server URL` CLI changes it. Plain HTTP and JSON,
//! no authentication. Everything is kept in DATA_DIR/server.json.
//!
//!   screentime-server [--listen ADDR] [--data DIR]
//!
//! Defaults: 0.0.0.0:8470 and $SCREENTIME_SERVER_DATA, else /data. "Today"
//! is the server's local date, so set its time zone (TZ) to the PCs'.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use screentime::Status;
use screentime::config::Config;
use screentime::remote::{DEFAULT_PORT, Policy, RemoteStatus, SyncRequest};
use screentime::state::{self, Change};

/// A user's last report from a PC.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Seen {
    host: String,
    at: DateTime<Local>,
    date: NaiveDate,
    status: Status,
}

/// What's persisted.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    /// The policy's config TOML.
    config: String,
    /// Today's overrides per user (`used_secs` unused).
    overrides: state::State,
    seen: BTreeMap<String, Seen>,
}

struct Server {
    store: Store,
    /// `store.config`, parsed.
    cfg: Config,
    path: PathBuf,
}

impl Server {
    fn save(&self) {
        let json = serde_json::to_string_pretty(&self.store).expect("serializable");
        if let Err(e) = state::write_atomic(&self.path, json.as_bytes()) {
            eprintln!("saving {}: {e:#}", self.path.display());
        }
    }

    /// The user's status from the schedule, today's overrides, and the
    /// usage their PC last reported.
    fn status(&self, user: &str) -> Option<RemoteStatus> {
        let schedule = self.cfg.users.get(user)?;
        let now = Local::now();
        let mut usage = self.store.overrides.peek(user, now.date_naive());
        let seen = self.store.seen.get(user);
        if let Some(seen) = seen.filter(|s| s.date == now.date_naive()) {
            usage.used_secs = seen.status.used_secs;
        }
        let mut status = schedule.evaluate(user, now.naive_local(), &usage);
        // Counting only if the PC said so within the last two polls or so.
        status.counting =
            seen.is_some_and(|s| s.status.counting && (now - s.at) < chrono::Duration::seconds(75));
        Some(RemoteStatus {
            status,
            host: seen.map(|s| s.host.clone()),
            last_seen: seen.map(|s| s.at),
        })
    }
}

type Shared = Arc<Mutex<Server>>;
type ApiResult<T> = Result<T, (StatusCode, String)>;

fn bad_request(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

fn not_managed(user: &str) -> (StatusCode, String) {
    (
        StatusCode::NOT_FOUND,
        format!("{user} is not in the server's policy"),
    )
}

/// PC: here's how my users are doing; what's the policy?
async fn sync(State(sh): State<Shared>, Json(req): Json<SyncRequest>) -> Json<Policy> {
    let mut sh = sh.lock().await;
    let now = Local::now();
    for report in req.users {
        let user = report.status.user.clone();
        let first = !sh.store.seen.contains_key(&user);
        if first {
            eprintln!("{user}: first report from {}", req.host);
        }
        sh.store.seen.insert(
            user,
            Seen {
                host: req.host.clone(),
                at: now,
                date: report.date,
                status: report.status,
            },
        );
    }
    // Reports change every poll; they're only for display, so they're
    // written with the next real change (or at shutdown), not every time.
    let today = now.date_naive();
    let overrides = sh
        .store
        .overrides
        .users
        .iter()
        .filter(|(_, o)| o.date == today)
        .map(|(u, o)| (u.clone(), o.clone()))
        .collect();
    Json(Policy {
        config: sh.store.config.clone(),
        overrides,
    })
}

async fn get_config(State(sh): State<Shared>) -> String {
    sh.lock().await.store.config.clone()
}

async fn put_config(State(sh): State<Shared>, body: String) -> ApiResult<String> {
    let cfg = Config::parse(&body).map_err(|e| bad_request(format!("{e:#}")))?;
    if cfg.server_url.is_some() {
        return Err(bad_request("the policy must not set server_url"));
    }
    let mut sh = sh.lock().await;
    sh.store.config = body;
    sh.cfg = cfg;
    sh.save();
    let users: Vec<&String> = sh.cfg.users.keys().collect();
    eprintln!("policy updated; users {users:?}");
    Ok(format!(
        "policy saved ({} user(s)); PCs pick it up within a minute",
        users.len()
    ))
}

async fn all_status(State(sh): State<Shared>) -> Json<Vec<RemoteStatus>> {
    let sh = sh.lock().await;
    Json(sh.cfg.users.keys().filter_map(|u| sh.status(u)).collect())
}

async fn user_status(
    State(sh): State<Shared>,
    Path(user): Path<String>,
) -> ApiResult<Json<RemoteStatus>> {
    let sh = sh.lock().await;
    sh.status(&user).map(Json).ok_or_else(|| not_managed(&user))
}

async fn change(
    State(sh): State<Shared>,
    Path(user): Path<String>,
    Json(change): Json<Change>,
) -> ApiResult<Json<RemoteStatus>> {
    let mut sh = sh.lock().await;
    if !sh.cfg.users.contains_key(&user) {
        return Err(not_managed(&user));
    }
    let now = Local::now().naive_local();
    change
        .check(now.time())
        .map_err(|e| bad_request(format!("{e:#}")))?;
    change.apply(sh.store.overrides.today(&user, now.date()));
    sh.save();
    eprintln!("{user}: {}", change.describe());
    Ok(Json(sh.status(&user).expect("checked above")))
}

fn app(shared: Shared) -> Router {
    Router::new()
        .route("/api/v1/sync", post(sync))
        .route("/api/v1/config", get(get_config).put(put_config))
        .route("/api/v1/status", get(all_status))
        .route("/api/v1/status/{user}", get(user_status))
        .route("/api/v1/users/{user}/change", post(change))
        .with_state(shared)
}

fn load(path: &std::path::Path) -> Result<Store> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut listen = format!("0.0.0.0:{DEFAULT_PORT}");
    let mut data = std::env::var_os("SCREENTIME_SERVER_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/data"));
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().context("--listen needs ADDR:PORT")?,
            "--data" => data = args.next().context("--data needs a directory")?.into(),
            _ => bail!("usage: screentime-server [--listen ADDR:PORT] [--data DIR]"),
        }
    }

    std::fs::create_dir_all(&data).with_context(|| format!("creating {}", data.display()))?;
    let path = data.join("server.json");
    let store = load(&path)?;
    let cfg = Config::parse(&store.config).context("the stored policy")?;
    if cfg.users.is_empty() {
        eprintln!("no policy yet: upload one with `screentime --server URL config set FILE`");
    } else {
        eprintln!("policy for {:?}", cfg.users.keys().collect::<Vec<_>>());
    }
    let shared = Arc::new(Mutex::new(Server { store, cfg, path }));

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("listening on {listen}"))?;
    eprintln!("listening on {listen}");
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    axum::serve(listener, app(shared.clone()))
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        })
        .await?;
    shared.lock().await.save();
    Ok(())
}
