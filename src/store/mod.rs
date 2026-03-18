#![allow(dead_code)]
//! SQLite-backed persistence for sessions and turns.

mod schema;
mod session;
mod turn;

pub use session::{NewSession, Session, SessionStatus};
pub use turn::{NewTurn, Turn};

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Shared handle for use across async tasks.
///
/// `rusqlite::Connection` is `!Send`, so we wrap `Store` in a `Mutex`.
/// The lock is only ever acquired inside `spawn_blocking`, so it never
/// blocks the async runtime.
pub type SharedStore = Arc<Mutex<Store>>;

/// Thin wrapper around a SQLite connection.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (or create) the database at `~/.kley/kley.db`.
    pub fn open() -> Result<Self> {
        let dir = db_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;

        let db_path = dir.join("kley.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open {}", db_path.display()))?;

        // WAL mode for better concurrent read performance
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "foreign_keys", "on")?;

        schema::migrate(&conn)?;

        Ok(Store { conn })
    }

    /// Open an in-memory database (for tests).
    #[cfg(any(test, feature = "testing"))]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "on")?;
        schema::migrate(&conn)?;
        Ok(Store { conn })
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

/// Run a blocking store operation on the Tokio blocking thread pool.
///
/// Acquires the mutex inside `spawn_blocking` so the async runtime is never
/// blocked. Use from async `axum` handlers:
/// ```ignore
/// let sessions = store_run(&shared, |s| Session::list(s, 20)).await?;
/// ```
pub async fn store_run<F, T>(shared: &SharedStore, f: F) -> Result<T>
where
    F: FnOnce(&Store) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let shared = Arc::clone(shared);
    tokio::task::spawn_blocking(move || {
        let store = shared
            .lock()
            .map_err(|e| anyhow::anyhow!("store mutex poisoned: {e}"))?;
        f(&store)
    })
    .await
    .context("blocking task panicked")?
}

fn db_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".kley"))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_memory() {
        let store = Store::open_memory().expect("should open in-memory db");
        // Verify tables exist by querying them
        let count: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .expect("sessions table should exist");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_session_round_trip() {
        let store = Store::open_memory().unwrap();

        let new = NewSession {
            model: "gpt-4.1".into(),
            provider: "openai".into(),
        };
        let session = Session::create(&store, new).unwrap();
        assert_eq!(session.status, SessionStatus::Active);
        assert_eq!(session.model, "gpt-4.1");

        // Retrieve it
        let fetched = Session::get(&store, &session.id).unwrap();
        assert_eq!(fetched.id, session.id);
        assert_eq!(fetched.model, "gpt-4.1");

        // Update status
        Session::update_status(&store, &session.id, SessionStatus::Completed).unwrap();
        let fetched = Session::get(&store, &session.id).unwrap();
        assert_eq!(fetched.status, SessionStatus::Completed);
    }

    #[test]
    fn test_turn_round_trip() {
        let store = Store::open_memory().unwrap();

        let session = Session::create(
            &store,
            NewSession {
                model: "gpt-4.1".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();

        // Append some turns
        Turn::append(
            &store,
            NewTurn {
                session_id: session.id.clone(),
                kind: "message".into(),
                role: "user".into(),
                content: "Hello".into(),
                model: None,
                tokens_in: None,
                tokens_out: None,
            },
        )
        .unwrap();

        Turn::append(
            &store,
            NewTurn {
                session_id: session.id.clone(),
                kind: "message".into(),
                role: "assistant".into(),
                content: "Hi there!".into(),
                model: Some("gpt-4.1".into()),
                tokens_in: Some(10),
                tokens_out: Some(25),
            },
        )
        .unwrap();

        let turns = Turn::list_for_session(&store, &session.id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].turn_number, 1);
        assert_eq!(turns[0].tokens_in, None);
        assert_eq!(turns[1].role, "assistant");
        assert_eq!(turns[1].turn_number, 2);
        assert_eq!(turns[1].tokens_in, Some(10));
        assert_eq!(turns[1].tokens_out, Some(25));
    }

    #[test]
    fn test_session_latest() {
        let store = Store::open_memory().unwrap();

        let s1 = Session::create(
            &store,
            NewSession {
                model: "gpt-4.1".into(),
                provider: "openai".into(),
            },
        )
        .unwrap();

        let s2 = Session::create(
            &store,
            NewSession {
                model: "glm-4.7".into(),
                provider: "zai".into(),
            },
        )
        .unwrap();

        let latest = Session::get_latest(&store)
            .unwrap()
            .expect("should have a session");
        assert_eq!(latest.id, s2.id);

        // s1 should still be retrievable
        let fetched = Session::get(&store, &s1.id).unwrap();
        assert_eq!(fetched.provider, "openai");
    }

    #[test]
    fn test_session_list() {
        let store = Store::open_memory().unwrap();

        for _ in 0..3 {
            Session::create(
                &store,
                NewSession {
                    model: "gpt-4.1".into(),
                    provider: "openai".into(),
                },
            )
            .unwrap();
        }

        let sessions = Session::list(&store, 10).unwrap();
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn test_session_status_from_str() {
        assert_eq!(
            "active".parse::<SessionStatus>().unwrap(),
            SessionStatus::Active
        );
        assert_eq!(
            "completed".parse::<SessionStatus>().unwrap(),
            SessionStatus::Completed
        );
        assert!("garbage".parse::<SessionStatus>().is_err());
    }

    #[tokio::test]
    async fn test_store_run_async() {
        let store: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

        let count = store_run(&store, |s| {
            let n: i64 = s
                .conn()
                .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
            Ok(n)
        })
        .await
        .unwrap();

        assert_eq!(count, 0);
    }
}
