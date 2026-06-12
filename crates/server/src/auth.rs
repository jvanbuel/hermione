//! Minimal teacher authentication: a shared password grants a session cookie.
//!
//! This is deliberately lightweight (single shared teacher password, in-memory
//! session tokens) — enough to keep students' terminals private behind a login,
//! without standing up a full identity system. Behind a TLS-terminating proxy
//! in production; set the `Secure` cookie attribute there.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

/// How long a teacher session stays valid.
const SESSION_TTL_SECS: i64 = 12 * 60 * 60;

#[derive(Clone)]
pub struct Auth {
    /// `None` disables teacher auth (dev mode).
    password: Option<String>,
    /// token -> unix expiry seconds.
    sessions: Arc<RwLock<HashMap<String, i64>>>,
}

impl Auth {
    pub fn new(password: Option<String>) -> Self {
        Self {
            password,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Whether a login is required to view protected resources.
    pub fn enabled(&self) -> bool {
        self.password.is_some()
    }

    /// Verifies the password and, on success, returns a fresh session token.
    pub async fn login(&self, password: &str) -> Option<String> {
        let expected = self.password.as_deref()?;
        if !constant_time_eq(password.as_bytes(), expected.as_bytes()) {
            return None;
        }
        let token = Uuid::new_v4().to_string();
        let expiry = now() + SESSION_TTL_SECS;
        self.sessions.write().await.insert(token.clone(), expiry);
        Some(token)
    }

    /// True if auth is disabled, or the token is a live session.
    pub async fn validate(&self, token: Option<&str>) -> bool {
        if !self.enabled() {
            return true;
        }
        let Some(token) = token else { return false };
        let mut sessions = self.sessions.write().await;
        match sessions.get(token) {
            Some(&expiry) if expiry > now() => true,
            Some(_) => {
                sessions.remove(token); // expired
                false
            }
            None => false,
        }
    }

    pub async fn logout(&self, token: &str) {
        self.sessions.write().await.remove(token);
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Length-independent constant-time comparison to avoid timing leaks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
