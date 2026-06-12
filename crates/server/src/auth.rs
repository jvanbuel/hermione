//! Teacher session management: a login issues an in-memory session token
//! (delivered as a cookie) that maps back to an admin account.
//!
//! Lightweight on purpose — sessions live in memory, so admins re-login after a
//! restart. Run behind a TLS-terminating proxy and set the `Secure` cookie
//! attribute there.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

/// How long a teacher session stays valid.
const SESSION_TTL_SECS: i64 = 12 * 60 * 60;

/// Who is making a request to a protected route.
#[derive(Clone, Copy, Debug)]
pub enum AuthCtx {
    /// A logged-in admin (scoped to their course memberships).
    Admin(Uuid),
    /// No admins exist yet — open dev mode, full access to all courses.
    OpenDev,
}

#[derive(Clone, Default)]
pub struct Auth {
    /// token -> (admin id, unix expiry seconds).
    sessions: Arc<RwLock<HashMap<String, (Uuid, i64)>>>,
}

impl Auth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a session for an admin and returns its token.
    pub async fn create_session(&self, admin_id: Uuid) -> String {
        let token = Uuid::new_v4().to_string();
        self.sessions
            .write()
            .await
            .insert(token.clone(), (admin_id, now() + SESSION_TTL_SECS));
        token
    }

    /// Resolves a session token to its admin id, if still valid.
    pub async fn admin_for(&self, token: Option<&str>) -> Option<Uuid> {
        let token = token?;
        let mut sessions = self.sessions.write().await;
        match sessions.get(token) {
            Some(&(admin_id, expiry)) if expiry > now() => Some(admin_id),
            Some(_) => {
                sessions.remove(token);
                None
            }
            None => None,
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
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
