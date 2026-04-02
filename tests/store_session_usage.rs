//! Tests for the new session usage totals contract.

mod harness;

use std::{
    cell::RefCell,
    fs::File,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use harness::{SessionBuilder, TurnBuilder};
use kley::pricing::ModelsDevCatalog;
use kley::store::{Session, SessionUsageCatalog, SessionUsageSlice, SharedStore, Store, store_run};

fn pricing_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/models_dev/api.json")
}

fn load_models_dev_catalog() -> ModelsDevCatalog {
    let file = File::open(pricing_fixture_path()).expect("pricing fixture missing");
    ModelsDevCatalog::from_reader(file).expect("failed to parse pricing fixture")
}

/// Empty sessions should report zero usage and cost.
#[test]
fn session_usage_totals_empty_session_returns_zero_totals() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    let session_id = session.id.clone();
    let provider = session.provider.clone();
    let catalog = ();
    let pricing_catalog = load_models_dev_catalog();

    let totals =
        Session::usage_totals_with_catalog(&store, &session_id, &catalog, &pricing_catalog)
            .unwrap();

    assert_eq!(totals.session_id, session_id);
    assert_eq!(totals.provider, provider);
    assert_eq!(totals.input_tokens, 0);
    assert_eq!(totals.output_tokens, 0);
    assert_eq!(totals.total_tokens, 0);
    assert_eq!(totals.cost_usd_micros, Some(0));
    assert!(totals.unpriced_models.is_empty());
}

/// Turns with null token counts should still contribute zero usage.
#[test]
fn session_usage_totals_null_tokens_count_as_zero() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    TurnBuilder::new(&session.id)
        .role("assistant")
        .append(&store);

    let session_id = session.id.clone();
    let provider = session.provider.clone();
    let catalog = ();
    let pricing_catalog = load_models_dev_catalog();

    let totals =
        Session::usage_totals_with_catalog(&store, &session_id, &catalog, &pricing_catalog)
            .unwrap();

    assert_eq!(totals.session_id, session_id);
    assert_eq!(totals.provider, provider);
    assert_eq!(totals.input_tokens, 0);
    assert_eq!(totals.output_tokens, 0);
    assert_eq!(totals.total_tokens, 0);
    assert_eq!(totals.cost_usd_micros, Some(0));
    assert!(totals.unpriced_models.is_empty());
}

#[test]
fn session_usage_totals_sum_tokens_across_message_turns() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().create(&store);

    TurnBuilder::new(&session.id).tokens(10, 20).append(&store);
    TurnBuilder::new(&session.id)
        .role("assistant")
        .tokens(5, 2)
        .append(&store);
    TurnBuilder::new(&session.id)
        .kind("tool_call")
        .tokens(1000, 1000)
        .append(&store);

    let pricing_catalog = load_models_dev_catalog();

    let totals =
        Session::usage_totals_with_catalog(&store, &session.id, &(), &pricing_catalog).unwrap();

    assert_eq!(totals.input_tokens, 15);
    assert_eq!(totals.output_tokens, 22);
    assert_eq!(totals.total_tokens, 37);
}

struct RecordingCatalog {
    slices: RefCell<Vec<SessionUsageSlice>>,
}

impl RecordingCatalog {
    fn new() -> Self {
        Self {
            slices: RefCell::new(Vec::new()),
        }
    }

    fn recorded_slices(&self) -> Vec<SessionUsageSlice> {
        self.slices.borrow().clone()
    }
}

impl SessionUsageCatalog for RecordingCatalog {
    fn record_slice(&self, slice: &SessionUsageSlice) {
        self.slices.borrow_mut().push(slice.clone());
    }
}

#[test]
fn session_usage_totals_groups_usage_by_effective_model() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new().model("session-model").create(&store);

    TurnBuilder::new(&session.id).tokens(8, 12).append(&store);
    TurnBuilder::new(&session.id)
        .model("turn-model")
        .tokens(3, 5)
        .append(&store);

    let catalog = RecordingCatalog::new();
    let pricing_catalog = load_models_dev_catalog();

    let totals =
        Session::usage_totals_with_catalog(&store, &session.id, &catalog, &pricing_catalog)
            .unwrap();

    assert_eq!(totals.input_tokens, 11);
    assert_eq!(totals.output_tokens, 17);
    assert_eq!(totals.total_tokens, 28);

    let mut slices = catalog.recorded_slices();
    slices.sort_by(|a, b| a.effective_model.cmp(&b.effective_model));

    assert_eq!(slices.len(), 2);
    assert_eq!(slices[0].effective_model, "session-model");
    assert_eq!(slices[0].input_tokens, 8);
    assert_eq!(slices[0].output_tokens, 12);
    assert_eq!(slices[1].effective_model, "turn-model");
    assert_eq!(slices[1].input_tokens, 3);
    assert_eq!(slices[1].output_tokens, 5);
}

#[test]
fn session_usage_totals_compute_cost_micros_from_models_dev_prices() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new()
        .provider("openai")
        .model("gpt-4-mini")
        .create(&store);

    TurnBuilder::new(&session.id)
        .tokens(1_000, 1_500)
        .append(&store);

    let pricing_catalog = load_models_dev_catalog();

    let totals =
        Session::usage_totals_with_catalog(&store, &session.id, &(), &pricing_catalog).unwrap();

    assert_eq!(totals.provider, "openai");
    assert_eq!(totals.input_tokens, 1_000);
    assert_eq!(totals.output_tokens, 1_500);
    assert_eq!(totals.total_tokens, 2_500);
    assert_eq!(totals.cost_usd_micros, Some(80));
    assert!(totals.unpriced_models.is_empty());
}

#[test]
fn session_usage_totals_return_unpriced_models_when_catalog_entry_is_missing() {
    let store = Store::open_memory().unwrap();
    let session = SessionBuilder::new()
        .provider("openai")
        .model("gpt-4-mini")
        .create(&store);

    TurnBuilder::new(&session.id)
        .tokens(500, 600)
        .append(&store);
    TurnBuilder::new(&session.id)
        .model("unknown-model")
        .tokens(20, 25)
        .append(&store);
    TurnBuilder::new(&session.id)
        .model("unpriced-model")
        .tokens(30, 40)
        .append(&store);

    let pricing_catalog = load_models_dev_catalog();

    let totals =
        Session::usage_totals_with_catalog(&store, &session.id, &(), &pricing_catalog).unwrap();

    assert_eq!(totals.cost_usd_micros, None);
    assert_eq!(
        totals.unpriced_models,
        vec!["unknown-model".to_string(), "unpriced-model".to_string()]
    );
}

#[tokio::test]
async fn session_usage_totals_can_run_inside_store_run() {
    let shared: SharedStore = Arc::new(Mutex::new(Store::open_memory().unwrap()));
    let pricing_catalog = load_models_dev_catalog();

    let session_id = store_run(&shared, |s| {
        let session = SessionBuilder::new()
            .provider("openai")
            .model("gpt-4-mini")
            .create(s);
        Ok(session.id.clone())
    })
    .await
    .unwrap();

    let session_id_for_turn = session_id.clone();
    store_run(&shared, move |s| {
        TurnBuilder::new(&session_id_for_turn)
            .tokens(1_000, 1_500)
            .append(s);
        Ok(())
    })
    .await
    .unwrap();

    let session_id_for_totals = session_id.clone();
    let totals = store_run(&shared, move |s| {
        Session::usage_totals_with_catalog(s, &session_id_for_totals, &(), &pricing_catalog)
    })
    .await
    .unwrap();

    assert_eq!(totals.session_id, session_id);
    assert_eq!(totals.provider, "openai");
    assert_eq!(totals.input_tokens, 1_000);
    assert_eq!(totals.output_tokens, 1_500);
    assert_eq!(totals.total_tokens, 2_500);
    assert_eq!(totals.cost_usd_micros, Some(80));
    assert!(totals.unpriced_models.is_empty());
}
