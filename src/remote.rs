//! Server mode: the PCs fetch their policy from `screentime-server`.
//!
//! Plain HTTP and JSON, no authentication. A PC polls `POST /api/v1/sync`,
//! reporting its users' status and getting back the policy (the config TOML
//! and today's overrides), which it caches in its state directory and keeps
//! using whenever the server can't be reached.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::Status;
use crate::state::{self, DayUsage};

pub const DEFAULT_PORT: u16 = 8470;

/// PC -> server: who it is and how its users are doing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub host: String,
    pub users: Vec<Report>,
}

/// One user's status on a PC, on the PC's date.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub date: NaiveDate,
    pub status: Status,
}

/// Server -> PC: the policy to enforce.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    /// Config TOML, in the format of /etc/screentime/config.toml.
    pub config: String,
    /// Today's admin overrides per user (`used_secs` is unused).
    pub overrides: BTreeMap<String, DayUsage>,
}

/// The last policy a PC got from the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cached {
    /// When this policy (as it is) was first received.
    pub received_at: DateTime<Local>,
    pub policy: Policy,
}

pub fn cache_path() -> PathBuf {
    state::dir().join("policy.json")
}

impl Cached {
    /// None if there's no cache yet.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .with_context(|| format!("parsing {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        state::write_atomic(path, serde_json::to_string_pretty(self)?.as_bytes())
    }
}

/// A user's status as the server sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteStatus {
    #[serde(flatten)]
    pub status: Status,
    /// The PC that last reported this user, and when.
    pub host: Option<String>,
    pub last_seen: Option<DateTime<Local>>,
}

/// "http://pi.lan:8470", "pi.lan:8470" or "pi.lan" -> "http://pi.lan:8470".
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = if url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    };
    let host_part = url.split_once("://").map_or("", |(_, h)| h);
    if host_part.contains(':') {
        url
    } else {
        format!("{url}:{DEFAULT_PORT}")
    }
}

/// Blocking HTTP client for the server's API.
#[derive(Clone)]
pub struct Client {
    base: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(url: &str, timeout: Duration) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .build()
            .into();
        Client {
            base: normalize_url(url),
            agent,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// The body of a 2xx reply, or the server's error message.
    fn finish(
        &self,
        reply: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<String> {
        let mut reply = reply.with_context(|| format!("contacting {}", self.base))?;
        let status = reply.status();
        let body = reply.body_mut().read_to_string().unwrap_or_default();
        if !status.is_success() {
            let msg = body.trim();
            if msg.is_empty() {
                bail!("{}: {status}", self.base);
            }
            bail!("{msg}");
        }
        Ok(body)
    }

    pub fn get_text(&self, path: &str) -> Result<String> {
        self.finish(self.agent.get(format!("{}{path}", self.base)).call())
    }

    pub fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Ok(serde_json::from_str(&self.get_text(path)?)?)
    }

    pub fn put_text(&self, path: &str, body: &str) -> Result<String> {
        self.finish(
            self.agent
                .put(format!("{}{path}", self.base))
                .content_type("text/plain; charset=utf-8")
                .send(body),
        )
    }

    pub fn post_json<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let text = self.finish(
            self.agent
                .post(format!("{}{path}", self.base))
                .send_json(body),
        )?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn sync(&self, req: &SyncRequest) -> Result<Policy> {
        self.post_json("/api/v1/sync", req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(normalize_url("pi.lan"), "http://pi.lan:8470");
        assert_eq!(normalize_url("pi.lan:9000/"), "http://pi.lan:9000");
        assert_eq!(normalize_url("http://10.0.0.2"), "http://10.0.0.2:8470");
        assert_eq!(
            normalize_url("https://st.example.com:443"),
            "https://st.example.com:443"
        );
    }
}
