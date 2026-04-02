//! Turn (message) persistence.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use super::Store;
use super::session::SessionUsageSlice;

/// A persisted conversation turn.
#[derive(Debug, Clone)]
pub struct Turn {
    pub id: i64,
    pub session_id: String,
    /// Turn kind: "message", "tool_call", "observation" (extensible).
    pub kind: String,
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub turn_number: i64,
    pub created_at: DateTime<Utc>,
}

/// Data needed to append a new turn.
pub struct NewTurn {
    pub session_id: String,
    /// Turn kind: "message" for normal chat, extensible for "tool_call", "observation" later.
    pub kind: String,
    pub role: String,
    pub content: String,
    pub model: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
}

impl Turn {
    /// Append a turn to a session.
    ///
    /// Automatically assigns the next `turn_number` atomically via a subquery,
    /// avoiding a TOCTOU race between a separate SELECT and INSERT.
    pub fn append(store: &Store, new: NewTurn) -> Result<Turn> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        store
            .conn()
            .execute(
                "INSERT INTO turns (session_id, kind, role, content, model, tokens_in, tokens_out, turn_number, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                         (SELECT COALESCE(MAX(turn_number), 0) + 1 FROM turns WHERE session_id = ?1),
                         ?8)",
                (
                    &new.session_id,
                    &new.kind,
                    &new.role,
                    &new.content,
                    &new.model,
                    &new.tokens_in,
                    &new.tokens_out,
                    &now_str,
                ),
            )
            .context("failed to insert turn")?;

        let id = store.conn().last_insert_rowid();

        // Read back the assigned turn_number.
        let turn_number: i64 = store
            .conn()
            .query_row("SELECT turn_number FROM turns WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .context("failed to read back turn_number")?;

        Ok(Turn {
            id,
            session_id: new.session_id,
            kind: new.kind,
            role: new.role,
            content: new.content,
            model: new.model,
            tokens_in: new.tokens_in,
            tokens_out: new.tokens_out,
            turn_number,
            created_at: now,
        })
    }

    /// List all turns for a session, ordered by turn_number.
    pub fn list_for_session(store: &Store, session_id: &str) -> Result<Vec<Turn>> {
        let mut stmt = store.conn().prepare(
            "SELECT id, session_id, kind, role, content, model, tokens_in, tokens_out, turn_number, created_at
             FROM turns WHERE session_id = ?1 ORDER BY turn_number",
        )?;

        let turns = stmt
            .query_map([session_id], Self::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(turns)
    }

    /// Shared row mapper for all queries.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Turn> {
        let created_str: String = row.get(9)?;

        Ok(Turn {
            id: row.get(0)?,
            session_id: row.get(1)?,
            kind: row.get(2)?,
            role: row.get(3)?,
            content: row.get(4)?,
            model: row.get(5)?,
            tokens_in: row.get(6)?,
            tokens_out: row.get(7)?,
            turn_number: row.get(8)?,
            created_at: DateTime::parse_from_rfc3339(&created_str)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
        })
    }
}

pub(crate) fn message_usage_slices_by_effective_model(
    store: &Store,
    session_id: &str,
) -> Result<Vec<SessionUsageSlice>> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT COALESCE(turns.model, sessions.model) AS effective_model,
                COALESCE(SUM(COALESCE(turns.tokens_in, 0)), 0) AS tokens_in,
                COALESCE(SUM(COALESCE(turns.tokens_out, 0)), 0) AS tokens_out
             FROM turns
             JOIN sessions ON sessions.id = turns.session_id
             WHERE turns.session_id = ?1
               AND turns.kind = 'message'
               AND (turns.tokens_in IS NOT NULL OR turns.tokens_out IS NOT NULL)
             GROUP BY effective_model
             ORDER BY effective_model",
        )
        .context("failed to prepare usage slice aggregation")?;

    let rows = stmt
        .query_map([session_id], |row| {
            let model: String = row.get(0)?;
            let input: i64 = row.get(1)?;
            let output: i64 = row.get(2)?;
            Ok(SessionUsageSlice {
                effective_model: model,
                input_tokens: if input < 0 { 0 } else { input as u64 },
                output_tokens: if output < 0 { 0 } else { output as u64 },
            })
        })
        .context("failed to query usage slices")?;

    let slices = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(slices)
}
