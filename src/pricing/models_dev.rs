use std::collections::HashMap;
use std::io::Read;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;

#[derive(Debug)]
pub struct ModelsDevCatalog {
    providers: HashMap<String, HashMap<String, ModelCost>>,
}

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub provider_id: String,
    pub model_id: String,
    pub cost: ModelCost,
}

#[derive(Debug, Clone)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
}

const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";

/// Download the live models.dev catalog using blocking reqwest.
pub fn fetch_catalog() -> Result<ModelsDevCatalog> {
    let client = Client::new();
    let resp = client
        .get(MODELS_DEV_API_URL)
        .send()
        .context("failed to request models.dev catalog")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        bail!("models.dev catalog request failed: {status}\n{body}");
    }

    ModelsDevCatalog::from_reader(resp).context("failed to parse models.dev catalog")
}

impl ModelsDevCatalog {
    pub fn from_reader<R: Read>(reader: R) -> Result<Self> {
        let raw: HashMap<String, RawProvider> =
            serde_json::from_reader(reader).context("failed to deserialize models.dev catalog")?;

        let mut providers = HashMap::new();

        for (provider_id, provider) in raw {
            let mut priced_models = HashMap::new();

            for (model_key, model) in provider.models {
                let model_id = model.id.unwrap_or_else(|| model_key.clone());

                if let Some(RawCost {
                    input: Some(input),
                    output: Some(output),
                }) = model.cost
                {
                    priced_models.insert(model_id.clone(), ModelCost { input, output });
                }
            }

            providers.insert(provider_id, priced_models);
        }

        Ok(Self { providers })
    }

    pub fn resolve(&self, provider_id: &str, model_id: &str) -> Option<ModelPricing> {
        self.providers.get(provider_id).and_then(|models| {
            models.get(model_id).map(|cost| ModelPricing {
                provider_id: provider_id.to_string(),
                model_id: model_id.to_string(),
                cost: cost.clone(),
            })
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawProvider {
    models: HashMap<String, RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    id: Option<String>,
    cost: Option<RawCost>,
}

#[derive(Debug, Deserialize)]
struct RawCost {
    input: Option<f64>,
    output: Option<f64>,
}
