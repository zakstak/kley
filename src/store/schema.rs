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
    r#"
    CREATE TABLE tasks (
        task_id                  TEXT PRIMARY KEY,
        parent_task_id           TEXT REFERENCES tasks(task_id) ON DELETE SET NULL,
        title                    TEXT,
        priority                 INTEGER NOT NULL,
        policy_snapshot          TEXT NOT NULL,
        parent_close_policy      TEXT NOT NULL,
        recovery_checkpoint      TEXT,
        created_at               TEXT NOT NULL,
        updated_at               TEXT NOT NULL
    );

    CREATE INDEX idx_tasks_parent ON tasks(parent_task_id);
    CREATE INDEX idx_tasks_priority ON tasks(priority, created_at);

    CREATE TABLE task_edges (
        task_id                  TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
        depends_on_task_id       TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
        created_at               TEXT NOT NULL,
        PRIMARY KEY (task_id, depends_on_task_id),
        CHECK (task_id <> depends_on_task_id)
    );

    CREATE INDEX idx_task_edges_depends_on ON task_edges(depends_on_task_id);

    CREATE TABLE task_attempts (
        attempt_id               TEXT PRIMARY KEY,
        task_id                  TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
        session_id               TEXT REFERENCES sessions(id) ON DELETE SET NULL,
        status                   TEXT NOT NULL,
        recovery_checkpoint      TEXT,
        created_at               TEXT NOT NULL,
        updated_at               TEXT NOT NULL
    );

    CREATE INDEX idx_task_attempts_task ON task_attempts(task_id, created_at);
    CREATE INDEX idx_task_attempts_session ON task_attempts(session_id);

    CREATE TABLE task_events (
        sequence                 INTEGER PRIMARY KEY AUTOINCREMENT,
        task_id                  TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
        attempt_id               TEXT NOT NULL REFERENCES task_attempts(attempt_id) ON DELETE CASCADE,
        session_id               TEXT REFERENCES sessions(id) ON DELETE SET NULL,
        event_type               TEXT NOT NULL,
        payload                  TEXT NOT NULL,
        recorded_at              TEXT NOT NULL
    );

    CREATE INDEX idx_task_events_task_sequence ON task_events(task_id, sequence);
    CREATE INDEX idx_task_events_attempt_sequence ON task_events(attempt_id, sequence);
    "#,
];

pub fn migrate(conn: &Connection) -> Result<()> {
    // Ensure the meta table exists
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _schema_version (\n            version INTEGER NOT NULL\n        );",
    )
    .context("failed to create _schema_version table")?;

    let current_version: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM _schema_version",
            [],
            |row| row.get(0),
        )
        .context("failed to read schema version")?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn migrate_with_corrupted_schema_version_returns_error() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _schema_version (version TEXT NOT NULL);\n            INSERT INTO _schema_version (version) VALUES ('not-a-number');",
        )
        .unwrap();

        let err = migrate(&conn).unwrap_err();
        assert!(err.to_string().contains("failed to read schema version"));
    }
}
