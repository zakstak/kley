use crate::compact::CompactConfig;
use crate::store::Session;
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SessionSettingsOverrides {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub compact_threshold: Option<usize>,
}

impl SessionSettingsOverrides {
    pub fn from_session(session: &Session) -> Option<Self> {
        Self::from_json(session.settings.as_deref()?)
    }

    pub fn from_json(settings_json: &str) -> Option<Self> {
        let json = serde_json::from_str::<Value>(settings_json).ok()?;
        Some(Self {
            model: json
                .get("model")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            provider: json
                .get("provider")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            compact_threshold: json
                .get("compact_threshold")
                .and_then(|value| value.as_u64())
                .and_then(|threshold| usize::try_from(threshold).ok())
                .map(|threshold| threshold.max(1)),
        })
    }

    pub fn apply_model_provider_overrides(&self, model: &mut String, provider: &mut String) {
        if let Some(settings_model) = &self.model {
            *model = settings_model.clone();
        }
        if let Some(settings_provider) = &self.provider {
            *provider = settings_provider.clone();
        }
    }

    pub fn apply_compact_threshold(&self, compact_config: &mut CompactConfig) {
        if let Some(compact_threshold) = self.compact_threshold {
            compact_config.threshold_chars = compact_threshold;
        }
    }
}

pub(crate) fn canonical_settings_json(
    model: &str,
    provider: &str,
    compact_threshold: usize,
) -> String {
    json!({
        "model": model,
        "provider": provider,
        "compact_threshold": compact_threshold.max(1),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_settings_json_includes_compact_threshold() {
        let settings_json = canonical_settings_json("test-model", "test", 0);
        let settings = SessionSettingsOverrides::from_json(&settings_json).unwrap();

        assert_eq!(settings.model.as_deref(), Some("test-model"));
        assert_eq!(settings.provider.as_deref(), Some("test"));
        assert_eq!(settings.compact_threshold, Some(1));
    }

    #[test]
    fn settings_overrides_ignore_invalid_threshold_and_preserve_partial_fields() {
        let settings = SessionSettingsOverrides::from_json(
            r#"{"model":"test-model","provider":"test","compact_threshold":"nope"}"#,
        )
        .unwrap();

        assert_eq!(settings.model.as_deref(), Some("test-model"));
        assert_eq!(settings.provider.as_deref(), Some("test"));
        assert_eq!(settings.compact_threshold, None);
    }
}
