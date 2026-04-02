use std::fs::File;
use std::path::PathBuf;

use kley::pricing::ModelsDevCatalog;
use serde_json::error::Category;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/models_dev/api.json")
}

fn load_fixture() -> ModelsDevCatalog {
    let file = File::open(fixture_path()).expect("pricing fixture missing");
    ModelsDevCatalog::from_reader(file).expect("failed to parse pricing fixture")
}

#[test]
fn models_dev_catalog_resolves_exact_provider_and_model() {
    let price = load_fixture()
        .resolve("openai", "gpt-4-mini")
        .expect("expected priced entry");

    assert_eq!(price.provider_id, "openai");
    assert_eq!(price.model_id, "gpt-4-mini");
    assert!(
        (price.cost.input - 0.02).abs() < 1e-9,
        "unexpected input cost"
    );
    assert!(
        (price.cost.output - 0.04).abs() < 1e-9,
        "unexpected output cost"
    );
}

#[test]
fn models_dev_catalog_returns_none_for_missing_price() {
    let catalog = load_fixture();

    assert!(
        catalog.resolve("openai", "gpt-4-mini-partial").is_none(),
        "partial cost should be treated as missing"
    );
    assert!(
        catalog.resolve("openai", "unpriced-model").is_none(),
        "missing cost should return None"
    );
    assert!(
        catalog.resolve("openai", "unknown-model").is_none(),
        "unknown model should still return None"
    );
}

#[test]
fn models_dev_catalog_rejects_invalid_json_payload() {
    let payload = b"{ not json }";
    let err = ModelsDevCatalog::from_reader(&payload[..]);

    assert!(err.is_err(), "invalid JSON should not parse");
    let err = err.unwrap_err();
    let serde_err = err
        .root_cause()
        .downcast_ref::<serde_json::Error>()
        .expect("invalid JSON error should be serde_json::Error");

    assert_eq!(serde_err.classify(), Category::Syntax);
}

#[test]
fn models_dev_catalog_rejects_invalid_shape_payload() {
    let payload = r#"{"openai": {}}"#.as_bytes();
    let err = ModelsDevCatalog::from_reader(payload);

    assert!(err.is_err(), "invalid shape should not parse");
    let err = err.unwrap_err();
    let serde_err = err
        .root_cause()
        .downcast_ref::<serde_json::Error>()
        .expect("invalid shape error should be serde_json::Error");

    assert_eq!(serde_err.classify(), Category::Data);
}
