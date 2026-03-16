//! Schema migrations for the kley database.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Each entry is a SQL migration. They run in order, once, tracked by version.
const MIGRATIONS: &[&str] = &[
    // v1: sessions, turns, contexts
    r#"
    CREATE TABLE sessions (
        id          TEXT PRIMARY KEY,
        title       TEXT,
        status      TEXT NOT NULL DEFAULT 'active',
        model       TEXT NOT NULL,
        provider    TEXT NOT NULL,
        policy      TEXT,
        created_at  TEXT NOT NULL,
        updated_at  TEXT NOT NULL
    );

    CREATE TABLE turns (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id  TEXT NOT NULL REFERENCES sessions(id),
        role        TEXT NOT NULL,
        content     TEXT NOT NULL,
        model       TEXT,
        tokens_in   INTEGER,
        tokens_out  INTEGER,
        turn_number INTEGER NOT NULL,
        created_at  TEXT NOT NULL
    );

    CREATE INDEX idx_turns_session ON turns(session_id, turn_number);

    CREATE TABLE contexts (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id  TEXT NOT NULL REFERENCES sessions(id),
        kind        TEXT NOT NULL,
        content     TEXT NOT NULL,
        created_at  TEXT NOT NULL
    );

    CREATE INDEX idx_contexts_session ON contexts(session_id, kind);
    "#,
    // v2: extensibility — settings, turn kinds, generic artifacts, rate limits
    r#"
    -- JSON blob for model/provider settings needed to resume a session.
    -- Intentionally unstructured so we don't lock into a specific schema.
    ALTER TABLE sessions ADD COLUMN settings TEXT;

    -- Distinguish message turns from future tool_call / observation turns.
    -- Defaults to 'message' for all existing rows.
    ALTER TABLE turns ADD COLUMN kind TEXT NOT NULL DEFAULT 'message';

    -- Generic artifacts attached to a session. The `content` column is JSON
    -- so any artifact type can store its payload without schema changes.
    CREATE TABLE artifacts (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id  TEXT NOT NULL REFERENCES sessions(id),
        kind        TEXT NOT NULL,     -- freeform type tag (e.g. "file", "plan", "report")
        name        TEXT,              -- optional human-readable label
        content     TEXT NOT NULL,     -- JSON payload
        created_at  TEXT NOT NULL
    );

    CREATE INDEX idx_artifacts_session ON artifacts(session_id);

    -- Snapshot of API rate limits, one row per update.
    CREATE TABLE rate_limits (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id      TEXT NOT NULL REFERENCES sessions(id),
        provider        TEXT NOT NULL,
        data            TEXT NOT NULL,  -- JSON blob (remaining tokens, credits, plan, etc.)
        recorded_at     TEXT NOT NULL
    );

    CREATE INDEX idx_rate_limits_session ON rate_limits(session_id);
    "#,
];

/// Run any pending migrations. Idempotent.
pub fn migrate(conn: &Connection) -> Result<()> {
    // Ensure the meta table exists
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _schema_version (
            version INTEGER NOT NULL
        );",
    )
    .context("failed to create _schema_version table")?;

    let current_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM _schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if version <= current_version {
            continue;
        }

        conn.execute_batch(sql)
            .with_context(|| format!("migration v{version} failed"))?;

        conn.execute(
            "INSERT INTO _schema_version (version) VALUES (?1)",
            [version],
        )
        .with_context(|| format!("failed to record migration v{version}"))?;
    }

    Ok(())
}
