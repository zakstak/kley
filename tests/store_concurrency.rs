//! Integration tests for SharedStore + store_run under async concurrency.

mod harness;

use std::sync::{Arc, Mutex};

use kley::store::{self, NewSession, Session, SharedStore, Store};

// ── Basic store_run ─────────────────────────────────────────────────────────

#[tokio::test]
async fn store_run_basic_query() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    let sessions = store::store_run(&shared, |s| Session::list(s, 100))
        .await
        .unwrap();
    assert_eq!(sessions.len(), 0);
}

// ── Concurrent store_run calls don't corrupt data ───────────────────────────

#[tokio::test]
async fn concurrent_store_run_sessions() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    let mut handles = Vec::new();

    for i in 0..10 {
        let store = Arc::clone(&shared);
        let handle = tokio::spawn(async move {
            store::store_run(&store, move |s| {
                Session::create(
                    s,
                    NewSession {
                        model: format!("model-{i}"),
                        provider: "test".into(),
                    },
                )?;
                Ok(())
            })
            .await
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    // All 10 sessions should exist
    let sessions = store::store_run(&shared, |s| Session::list(s, 100))
        .await
        .unwrap();
    assert_eq!(sessions.len(), 10);
}

// ── Sequential store_run operations compose correctly ───────────────────────

#[tokio::test]
async fn sequential_store_run_create_then_read() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    // Create a session
    let session_id = store::store_run(&shared, |s| {
        let session = Session::create(
            s,
            NewSession {
                model: "gpt-4.1".into(),
                provider: "openai".into(),
            },
        )?;
        Ok(session.id)
    })
    .await
    .unwrap();

    // Read it back in a separate store_run
    let fetched = store::store_run(&shared, move |s| Session::get(s, &session_id))
        .await
        .unwrap();

    assert_eq!(fetched.model, "gpt-4.1");
    assert_eq!(fetched.provider, "openai");
}

// ── Multiple concurrent reads are safe ──────────────────────────────────────

#[tokio::test]
async fn concurrent_reads_after_writes() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));

    // Insert some data first
    store::store_run(&shared, |s| {
        for _ in 0..5 {
            Session::create(
                s,
                NewSession {
                    model: "m".into(),
                    provider: "p".into(),
                },
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();

    // Fire off 20 concurrent reads
    let mut handles = Vec::new();
    for _ in 0..20 {
        let store = Arc::clone(&shared);
        handles.push(tokio::spawn(async move {
            store::store_run(&store, |s| Session::list(s, 100)).await
        }));
    }

    for handle in handles {
        let sessions = handle.await.unwrap().unwrap();
        assert_eq!(sessions.len(), 5, "each read should see all 5 sessions");
    }
}
