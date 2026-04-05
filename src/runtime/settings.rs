use crate::compact::CompactConfig;
use crate::store::{Session, Store, TaskRecord};
use crate::tools::ToolRegistry;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InheritedSettingsSnapshot {
    pub model: String,
    pub provider: String,
    #[serde(default = "default_compact_threshold")]
    pub compact_threshold: usize,
}

fn default_compact_threshold() -> usize {
    CompactConfig::default().threshold_chars.max(1)
}

impl InheritedSettingsSnapshot {
    pub fn from_parent_session(session: &Session) -> Self {
        let mut model = session.model.clone();
        let mut provider = session.provider.clone();
        let mut compact_config = CompactConfig::default();
        if let Some(overrides) = SessionSettingsOverrides::from_session(session) {
            overrides.apply_model_provider_overrides(&mut model, &mut provider);
            overrides.apply_compact_threshold(&mut compact_config);
        }

        Self {
            model,
            provider,
            compact_threshold: compact_config.threshold_chars.max(1),
        }
    }

    pub fn canonical_settings_json(&self) -> String {
        canonical_settings_json(&self.model, &self.provider, self.compact_threshold)
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DelegationToolApprovalMode {
    Auto,
    Ask,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DelegationPolicySnapshot {
    pub allow_autonomous_spawn: bool,
    pub current_depth: u32,
    pub max_depth: u32,
    pub max_concurrency: u32,
    pub budget: u64,
    #[serde(default)]
    pub allowed_providers: Vec<String>,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default)]
    pub approved_tools: Vec<String>,
    pub tool_approval_mode: DelegationToolApprovalMode,
    pub parent_close_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DelegationPolicyRequest {
    pub allow_autonomous_spawn: Option<bool>,
    pub current_depth: Option<u32>,
    pub max_depth: Option<u32>,
    pub max_concurrency: Option<u32>,
    pub budget: Option<u64>,
    pub allowed_providers: Option<Vec<String>>,
    pub allowed_models: Option<Vec<String>>,
    pub approved_tools: Option<Vec<String>>,
    pub tool_approval_mode: Option<DelegationToolApprovalMode>,
    pub parent_close_policy: Option<String>,
}

impl DelegationPolicyRequest {
    fn from_json(requested_policy_json: Option<&str>) -> Result<Self> {
        let Some(raw) = requested_policy_json
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
        else {
            return Ok(Self::default());
        };
        serde_json::from_str(raw).context("failed to parse requested child delegation policy")
    }
}

impl DelegationPolicySnapshot {
    pub(crate) fn from_task(task: &TaskRecord, registry: &ToolRegistry) -> Result<Self> {
        let parsed: Value = serde_json::from_str(&task.policy_snapshot)
            .context("failed to parse persisted task policy snapshot")?;

        let snapshot = if parsed.get("allow_autonomous_spawn").is_some()
            || parsed.get("current_depth").is_some()
            || parsed.get("max_concurrency").is_some()
            || parsed.get("approved_tools").is_some()
        {
            let snapshot: Self = serde_json::from_value(parsed)
                .context("failed to decode persisted delegation policy snapshot")?;
            if snapshot.parent_close_policy != task.parent_close_policy {
                anyhow::bail!(
                    "persisted delegation policy parent_close_policy does not match task column"
                );
            }
            snapshot
        } else {
            let tool_approval_mode = parsed
                .get("mode")
                .and_then(Value::as_str)
                .map(parse_legacy_tool_approval_mode)
                .transpose()?
                .unwrap_or(DelegationToolApprovalMode::Never);
            Self {
                allow_autonomous_spawn: false,
                current_depth: 0,
                max_depth: 0,
                max_concurrency: 0,
                budget: 0,
                allowed_providers: Vec::new(),
                allowed_models: Vec::new(),
                approved_tools: Vec::new(),
                tool_approval_mode,
                parent_close_policy: task.parent_close_policy.clone(),
            }
        };

        snapshot.normalized(registry)
    }

    fn normalized(mut self, registry: &ToolRegistry) -> Result<Self> {
        self.allowed_providers = normalize_string_list(&self.allowed_providers);
        self.allowed_models = normalize_string_list(&self.allowed_models);
        self.approved_tools = normalize_string_list(&self.approved_tools);
        self.parent_close_policy = self.parent_close_policy.trim().to_string();

        if self.parent_close_policy.is_empty() {
            anyhow::bail!("delegation policy parent_close_policy must not be empty");
        }
        if self.current_depth > self.max_depth {
            anyhow::bail!(
                "delegation policy current_depth {} exceeds max_depth {}",
                self.current_depth,
                self.max_depth
            );
        }
        for tool_name in &self.approved_tools {
            if registry.get(tool_name).is_none() {
                anyhow::bail!("delegation policy references unknown approved tool '{tool_name}'");
            }
        }

        Ok(self)
    }

    fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).context("failed to serialize delegation policy snapshot")
    }

    fn ensure_autonomous_spawn_allowed(&self) -> Result<()> {
        if !self.allow_autonomous_spawn {
            anyhow::bail!(
                "autonomous spawn denied: persisted policy does not allow autonomous spawning"
            );
        }
        if self.current_depth >= self.max_depth {
            anyhow::bail!(
                "autonomous spawn denied: delegation depth limit {} reached at depth {}",
                self.max_depth,
                self.current_depth
            );
        }
        if self.budget == 0 {
            anyhow::bail!("autonomous spawn denied: delegation budget exhausted");
        }
        Ok(())
    }

    fn derive_child(
        &self,
        request: DelegationPolicyRequest,
        registry: &ToolRegistry,
    ) -> Result<Self> {
        if request.current_depth.is_some() {
            anyhow::bail!("child delegation policy cannot override current_depth");
        }

        let child_max_depth = request.max_depth.unwrap_or(self.max_depth);
        if child_max_depth > self.max_depth {
            anyhow::bail!(
                "child delegation policy cannot widen max_depth beyond {}",
                self.max_depth
            );
        }

        let child_max_concurrency = request.max_concurrency.unwrap_or(self.max_concurrency);
        if child_max_concurrency > self.max_concurrency {
            anyhow::bail!(
                "child delegation policy cannot widen max_concurrency beyond {}",
                self.max_concurrency
            );
        }

        let child_budget = request.budget.unwrap_or(self.budget);
        if child_budget > self.budget {
            anyhow::bail!(
                "child delegation policy cannot widen budget beyond {}",
                self.budget
            );
        }

        let child_allowed_providers = request
            .allowed_providers
            .as_ref()
            .map(|values| normalize_string_list(values))
            .unwrap_or_else(|| self.allowed_providers.clone());
        ensure_subset(
            &child_allowed_providers,
            &self.allowed_providers,
            "allowed_providers",
        )?;

        let child_allowed_models = request
            .allowed_models
            .as_ref()
            .map(|values| normalize_string_list(values))
            .unwrap_or_else(|| self.allowed_models.clone());
        ensure_subset(
            &child_allowed_models,
            &self.allowed_models,
            "allowed_models",
        )?;

        let child_approved_tools = request
            .approved_tools
            .as_ref()
            .map(|values| normalize_string_list(values))
            .unwrap_or_else(|| self.approved_tools.clone());
        ensure_subset(
            &child_approved_tools,
            &self.approved_tools,
            "approved_tools",
        )?;
        for tool_name in &child_approved_tools {
            if registry.get(tool_name).is_none() {
                anyhow::bail!(
                    "child delegation policy references unknown approved tool '{tool_name}'"
                );
            }
        }

        let child_tool_approval_mode = request
            .tool_approval_mode
            .unwrap_or(self.tool_approval_mode);
        if tool_approval_rank(child_tool_approval_mode)
            > tool_approval_rank(self.tool_approval_mode)
        {
            anyhow::bail!(
                "child delegation policy cannot widen tool_approval_mode beyond {:?}",
                self.tool_approval_mode
            );
        }

        let child_allow_autonomous_spawn = request
            .allow_autonomous_spawn
            .unwrap_or(self.allow_autonomous_spawn);
        if child_allow_autonomous_spawn && !self.allow_autonomous_spawn {
            anyhow::bail!(
                "child delegation policy cannot enable autonomous spawning when parent forbids it"
            );
        }

        let child_parent_close_policy = request
            .parent_close_policy
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(self.parent_close_policy.as_str())
            .to_string();
        if child_parent_close_policy != self.parent_close_policy {
            anyhow::bail!(
                "child delegation policy cannot change parent_close_policy from '{}'",
                self.parent_close_policy
            );
        }

        let child_current_depth = self.current_depth.saturating_add(1);
        if child_current_depth > child_max_depth {
            anyhow::bail!(
                "child delegation policy max_depth {} is below inherited child depth {}",
                child_max_depth,
                child_current_depth
            );
        }

        Self {
            allow_autonomous_spawn: child_allow_autonomous_spawn,
            current_depth: child_current_depth,
            max_depth: child_max_depth,
            max_concurrency: child_max_concurrency,
            budget: child_budget,
            allowed_providers: child_allowed_providers,
            allowed_models: child_allowed_models,
            approved_tools: child_approved_tools,
            tool_approval_mode: child_tool_approval_mode,
            parent_close_policy: child_parent_close_policy,
        }
        .normalized(registry)
    }

    pub(crate) fn allows_tool_call(&self, tool_name: &str) -> bool {
        match self.tool_approval_mode {
            DelegationToolApprovalMode::Never | DelegationToolApprovalMode::Ask => false,
            DelegationToolApprovalMode::Auto => {
                self.approved_tools.is_empty()
                    || self
                        .approved_tools
                        .iter()
                        .any(|approved| approved == tool_name)
            }
        }
    }
}

pub(crate) fn spawn_autonomous_child_task_with_policy(
    store: &Store,
    registry: &ToolRegistry,
    parent_task_id: &str,
    child_task_id: &str,
    title: Option<String>,
    priority: i64,
    requested_policy_json: Option<&str>,
) -> Result<TaskRecord> {
    let parent_task = TaskRecord::get(store, parent_task_id)?;
    let parent_policy = DelegationPolicySnapshot::from_task(&parent_task, registry)?;
    parent_policy.ensure_autonomous_spawn_allowed()?;

    let child_policy = parent_policy.derive_child(
        DelegationPolicyRequest::from_json(requested_policy_json)?,
        registry,
    )?;

    let now = chrono::Utc::now().to_rfc3339();
    let policy_snapshot = child_policy.to_json()?;
    let parent_close_policy = child_policy.parent_close_policy.clone();
    let inserted_rows = store
        .conn()
        .execute(
            "INSERT INTO tasks (task_id, parent_task_id, title, priority, policy_snapshot, parent_close_policy, recovery_checkpoint, owner_session_id, created_at, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9
             WHERE (
                 SELECT COUNT(*)
                 FROM tasks child
                 WHERE child.parent_task_id = ?2
                   AND COALESCE((
                       SELECT json_extract(task_events.payload, '$.to')
                       FROM task_events
                       WHERE task_events.task_id = child.task_id
                         AND task_events.event_type = 'task.state.transition'
                       ORDER BY task_events.sequence DESC
                       LIMIT 1
                    ), 'queued') NOT IN ('cancelled', 'failed', 'completed')
             ) < ?10",
            (
                child_task_id,
                parent_task_id,
                &title,
                priority,
                &policy_snapshot,
                &parent_close_policy,
                Option::<String>::None,
                &parent_task.owner_session_id,
                &now,
                i64::from(parent_policy.max_concurrency),
            ),
        )
        .context("failed to insert autonomous child task")?;

    if inserted_rows == 0 {
        anyhow::bail!(
            "autonomous spawn denied: max_concurrency {} already reached for task {}",
            parent_policy.max_concurrency,
            parent_task_id
        );
    }

    TaskRecord::get(store, child_task_id)
}

fn parse_legacy_tool_approval_mode(raw: &str) -> Result<DelegationToolApprovalMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(DelegationToolApprovalMode::Auto),
        "ask" => Ok(DelegationToolApprovalMode::Ask),
        "never" => Ok(DelegationToolApprovalMode::Never),
        other => anyhow::bail!("unknown legacy delegation tool approval mode: {other}"),
    }
}

fn normalize_string_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn ensure_subset(child: &[String], parent: &[String], field_name: &str) -> Result<()> {
    let child_set = child.iter().collect::<BTreeSet<_>>();
    let parent_set = parent.iter().collect::<BTreeSet<_>>();
    if child_set.is_subset(&parent_set) {
        return Ok(());
    }

    let widened = child_set
        .difference(&parent_set)
        .map(|value| value.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    anyhow::bail!(
        "child delegation policy cannot widen {field_name}; unauthorized values: {widened}"
    )
}

fn tool_approval_rank(mode: DelegationToolApprovalMode) -> u8 {
    match mode {
        DelegationToolApprovalMode::Never => 0,
        DelegationToolApprovalMode::Ask => 1,
        DelegationToolApprovalMode::Auto => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_settings_json_includes_compact_threshold() {
        let settings_json = canonical_settings_json("gpt-5.3-codex-spark", "openai", 0);
        let settings = SessionSettingsOverrides::from_json(&settings_json).unwrap();

        assert_eq!(settings.model.as_deref(), Some("gpt-5.3-codex-spark"));
        assert_eq!(settings.provider.as_deref(), Some("openai"));
        assert_eq!(settings.compact_threshold, Some(1));
    }

    #[test]
    fn settings_overrides_ignore_invalid_threshold_and_preserve_partial_fields() {
        let settings = SessionSettingsOverrides::from_json(
            r#"{"model":"gpt-5.3-codex-spark","provider":"openai","compact_threshold":"nope"}"#,
        )
        .unwrap();

        assert_eq!(settings.model.as_deref(), Some("gpt-5.3-codex-spark"));
        assert_eq!(settings.provider.as_deref(), Some("openai"));
        assert_eq!(settings.compact_threshold, None);
    }

    #[test]
    fn delegation_policy_normalization_dedupes_lists() {
        let registry = crate::tools::default_registry(std::env::temp_dir());
        let task = TaskRecord {
            task_id: "task-1".to_string(),
            parent_task_id: None,
            title: None,
            priority: 0,
            policy_snapshot: serde_json::json!({
                "allow_autonomous_spawn": true,
                "current_depth": 0,
                "max_depth": 2,
                "max_concurrency": 1,
                "budget": 5,
                "allowed_providers": ["openai", " openai "],
                "allowed_models": ["model-a", "model-a"],
                "approved_tools": ["read_file", " read_file "],
                "tool_approval_mode": "ask",
                "parent_close_policy": "request_cancel_descendants"
            })
            .to_string(),
            parent_close_policy: "request_cancel_descendants".to_string(),
            recovery_checkpoint: None,
            owner_session_id: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let snapshot = DelegationPolicySnapshot::from_task(&task, &registry).unwrap();
        assert_eq!(snapshot.allowed_providers, vec!["openai".to_string()]);
        assert_eq!(snapshot.allowed_models, vec!["model-a".to_string()]);
        assert_eq!(snapshot.approved_tools, vec!["read_file".to_string()]);
    }
}
