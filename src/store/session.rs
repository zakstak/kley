//! Session persistence.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fmt;

/// Lightweight error for row-mapping failures (implements `std::error::Error`).
#[derive(Debug)]
struct RowParseError(String);

impl fmt::Display for RowParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RowParseError {}

use super::Store;

/// Session status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Paused,
    Completed,
    Failed,
    Aborted,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SessionStatus::Active => "active",
            SessionStatus::Paused => "paused",
            SessionStatus::Completed => "completed",
            SessionStatus::Failed => "failed",
            SessionStatus::Aborted => "aborted",
        };
        write!(f, "{s}")
    }
}

impl std::str::FromStr for SessionStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(SessionStatus::Active),
            "paused" => Ok(SessionStatus::Paused),
            "completed" => Ok(SessionStatus::Completed),
            "failed" => Ok(SessionStatus::Failed),
            "aborted" => Ok(SessionStatus::Aborted),
            other => anyhow::bail!("unknown session status: {other:?}"),
        }
    }
}

/// A persisted session.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub title: Option<String>,
    pub status: SessionStatus,
    pub model: String,
    pub provider: String,
    /// Approval / sandbox policy tag (e.g. "auto-approve", "ask").
    pub policy: Option<String>,
    /// Freeform JSON blob for model/provider settings needed to resume.
    /// Intentionally unstructured to avoid lock-in.
    pub settings: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Data needed to create a new session.
pub struct NewSession {
    pub model: String,
    pub provider: String,
}

impl Session {
    /// Create a new session and persist it.
    pub fn create(store: &Store, new: NewSession) -> Result<Session> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO sessions (id, status, model, provider, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (&id, "active", &new.model, &new.provider, &now_str, &now_str),
            )
            .context("failed to insert session")?;

        Ok(Session {
            id,
            title: None,
            status: SessionStatus::Active,
            model: new.model,
            provider: new.provider,
            policy: None,
            settings: None,
            created_at: now,
            updated_at: now,
        })
    }

    /// Get a session by ID.
    pub fn get(store: &Store, id: &str) -> Result<Session> {
        store
            .conn()
            .query_row(
                "SELECT id, title, status, model, provider, policy, settings, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                [id],
                Self::from_row,
            )
            .context("session not found")
    }

    /// Get the most recently created session.
    pub fn get_latest(store: &Store) -> Result<Option<Session>> {
        let mut stmt = store.conn().prepare(
            "SELECT id, title, status, model, provider, policy, settings, created_at, updated_at
             FROM sessions ORDER BY created_at DESC LIMIT 1",
        )?;

        let mut rows = stmt.query_map([], Self::from_row)?;

        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// List sessions, most recent first.
    pub fn list(store: &Store, limit: u32) -> Result<Vec<Session>> {
        let mut stmt = store.conn().prepare(
            "SELECT id, title, status, model, provider, policy, settings, created_at, updated_at
             FROM sessions ORDER BY created_at DESC LIMIT ?1",
        )?;

        let sessions = stmt
            .query_map([limit], Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(sessions)
    }

    /// Update the status of a session.
    pub fn update_status(store: &Store, id: &str, status: SessionStatus) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        store.conn().execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            (&status.to_string(), &now, id),
        )?;
        Ok(())
    }

    /// Set the title of a session.
    pub fn update_title(store: &Store, id: &str, title: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        store.conn().execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            (title, &now, id),
        )?;
        Ok(())
    }

    /// Save settings JSON (for session resume).
    pub fn update_settings(store: &Store, id: &str, settings: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        store.conn().execute(
            "UPDATE sessions SET settings = ?1, updated_at = ?2 WHERE id = ?3",
            (settings, &now, id),
        )?;
        Ok(())
    }

    /// Shared row mapper for all queries.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
        let status_str: String = row.get(2)?;
        let created_str: String = row.get(7)?;
        let updated_str: String = row.get(8)?;

        Ok(Session {
            id: row.get(0)?,
            title: row.get(1)?,
            status: status_str.parse::<SessionStatus>().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(RowParseError(e.to_string())),
                )
            })?,
            model: row.get(3)?,
            provider: row.get(4)?,
            policy: row.get(5)?,
            settings: row.get(6)?,
            created_at: DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            updated_at: DateTime::parse_from_rfc3339(&updated_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
        })
    }
}
