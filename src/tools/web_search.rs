use anyhow::Result;
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(any(test, feature = "testing"))]
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::Tool;
use crate::text::truncate_with_ascii_ellipsis;

pub type WebSearchValidationResult<T> = std::result::Result<T, String>;

const DEFAULT_MAX_RESULTS: usize = 5;
const MAX_RESULTS_CAP: usize = 5;
const MAX_QUERY_LEN: usize = 400;
const MAX_SUMMARY_LEN: usize = 1_600;
const MAX_SNIPPET_LEN: usize = 280;
const TAVILY_TIMEOUT: Duration = Duration::from_secs(15);
const TAVILY_SEARCH_URL: &str = "https://api.tavily.com/search";
#[cfg(any(test, feature = "testing"))]
const TAVILY_BASE_URL_ENV: &str = "TAVILY_API_BASE_URL";

#[cfg(any(test, feature = "testing"))]
fn tavily_timeout_override() -> &'static Mutex<Option<Duration>> {
    static OVERRIDE: OnceLock<Mutex<Option<Duration>>> = OnceLock::new();
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchStatus {
    Ok,
    NoResults,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchCitationInput {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebSearchCitation {
    pub index: usize,
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebSearchResult {
    pub status: WebSearchStatus,
    pub query: String,
    pub summary: Option<String>,
    pub citations: Vec<WebSearchCitation>,
    pub message: Option<String>,
}

impl WebSearchResult {
    pub fn ok(
        query: &str,
        summary: Option<&str>,
        citations: Vec<WebSearchCitationInput>,
        max_results: usize,
    ) -> WebSearchValidationResult<Self> {
        Ok(Self {
            status: WebSearchStatus::Ok,
            query: normalize_query(query)?,
            summary: truncate_summary(summary),
            citations: normalize_citations(citations, max_results),
            message: None,
        })
    }

    pub fn no_results(query: &str) -> WebSearchValidationResult<Self> {
        Ok(Self {
            status: WebSearchStatus::NoResults,
            query: normalize_query(query)?,
            summary: None,
            citations: Vec::new(),
            message: None,
        })
    }

    pub fn unavailable(query: &str, message: impl Into<String>) -> WebSearchValidationResult<Self> {
        Ok(Self {
            status: WebSearchStatus::Unavailable,
            query: normalize_query(query)?,
            summary: None,
            citations: Vec::new(),
            message: Some(message.into()),
        })
    }

    pub fn to_json_string(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

pub fn normalize_query(query: &str) -> WebSearchValidationResult<String> {
    let query = query.trim().to_string();
    validate_query_length(&query)?;
    Ok(query)
}

pub fn cap_result_count(max_results: usize) -> usize {
    max_results.min(MAX_RESULTS_CAP)
}

pub fn truncate_summary(summary: Option<&str>) -> Option<String> {
    summary.map(|summary| truncate_with_ascii_ellipsis(summary, MAX_SUMMARY_LEN))
}

pub fn truncate_snippet(snippet: Option<&str>) -> Option<String> {
    snippet.map(|snippet| truncate_with_ascii_ellipsis(snippet, MAX_SNIPPET_LEN))
}

pub fn normalize_citations(
    citations: Vec<WebSearchCitationInput>,
    max_results: usize,
) -> Vec<WebSearchCitation> {
    citations
        .into_iter()
        .take(cap_result_count(max_results))
        .enumerate()
        .map(|(index, citation)| WebSearchCitation {
            index: index + 1,
            title: citation.title,
            url: citation.url,
            snippet: truncate_snippet(citation.snippet.as_deref()),
        })
        .collect()
}

fn synthesize_summary(results: &[TavilySearchResult]) -> Option<String> {
    let parts = results
        .iter()
        .filter_map(|result| non_empty_text(result.content.as_deref()))
        .take(3)
        .collect::<Vec<_>>();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn non_empty_text(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn validate_query_length(query: &str) -> WebSearchValidationResult<()> {
    if query.len() > MAX_QUERY_LEN {
        return Err(format!(
            "Error: query must be <= {MAX_QUERY_LEN} characters after trimming"
        ));
    }

    Ok(())
}

pub struct WebSearchTool;

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web and return a normalized JSON string with summary and citations."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "maxLength": MAX_QUERY_LEN,
                    "description": "The web search query text after trimming surrounding whitespace."
                },
                "max_results": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "maximum": MAX_RESULTS_CAP,
                    "description": "Maximum number of citations to return. Pass null to use the default of 5."
                }
            },
            "required": ["query", "max_results"],
            "additionalProperties": false,
        })
    }

    fn execute(&self, args: Value) -> Result<String> {
        let query = match normalize_query(args.get("query").and_then(Value::as_str).unwrap_or("")) {
            Ok(query) => query,
            Err(error) => return Ok(error),
        };

        if query.is_empty() {
            return Ok("Error: query is required".to_string());
        }

        let max_results = match resolve_max_results(&args) {
            Ok(value) => value,
            Err(error) => return Ok(error),
        };

        if let Some(resolved) = resolve_web_search_backend() {
            return resolved.execute(&query, max_results);
        }

        unavailable_web_search_result(&query)
    }
}

pub fn resolve_max_results(args: &Value) -> WebSearchValidationResult<usize> {
    match args.get("max_results") {
        None | Some(Value::Null) => Ok(DEFAULT_MAX_RESULTS),
        Some(value) => {
            let raw = value.as_u64().ok_or_else(|| {
                format!("Error: max_results must be an integer between 1 and {MAX_RESULTS_CAP}")
            })? as usize;

            if !(1..=MAX_RESULTS_CAP).contains(&raw) {
                return Err(format!(
                    "Error: max_results must be an integer between 1 and {MAX_RESULTS_CAP}"
                ));
            }

            Ok(raw)
        }
    }
}

fn unavailable_web_search_result(query: &str) -> Result<String> {
    WebSearchResult::unavailable(query, "Set TAVILY_API_KEY to enable web_search.")
        .map_err(anyhow::Error::msg)?
        .to_json_string()
}

fn tavily_unavailable_web_search_result(query: &str, message: impl Into<String>) -> Result<String> {
    WebSearchResult::unavailable(query, message)
        .map_err(anyhow::Error::msg)?
        .to_json_string()
}

fn tavily_no_results_web_search_result(query: &str, message: impl Into<String>) -> Result<String> {
    WebSearchResult {
        status: WebSearchStatus::NoResults,
        query: normalize_query(query).map_err(anyhow::Error::msg)?,
        summary: None,
        citations: Vec::new(),
        message: Some(message.into()),
    }
    .to_json_string()
}

fn tavily_search_url() -> String {
    tavily_base_url_override()
        .map(|base| format!("{base}/search"))
        .unwrap_or_else(|| TAVILY_SEARCH_URL.to_string())
}

#[cfg(any(test, feature = "testing"))]
fn tavily_base_url_override() -> Option<String> {
    std::env::var(TAVILY_BASE_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(not(any(test, feature = "testing")))]
fn tavily_base_url_override() -> Option<String> {
    None
}

fn tavily_client_timeout() -> Duration {
    #[cfg(any(test, feature = "testing"))]
    {
        if let Some(timeout) = *tavily_timeout_override().lock().unwrap() {
            return timeout;
        }
    }

    TAVILY_TIMEOUT
}

fn build_tavily_client() -> Client {
    Client::builder()
        .timeout(tavily_client_timeout())
        .build()
        .expect("Tavily client should build")
}

fn resolve_web_search_backend() -> Option<ResolvedWebSearchBackend> {
    let api_key = std::env::var("TAVILY_API_KEY").ok()?;
    let trimmed_api_key = api_key.trim();
    if trimmed_api_key.is_empty() {
        return None;
    }

    Some(ResolvedWebSearchBackend::Tavily(TavilyBackend::new(
        trimmed_api_key.to_string(),
    )))
}

trait WebSearchBackend {
    fn execute(&self, query: &str, max_results: usize) -> Result<String>;
}

struct TavilyBackend {
    api_key: String,
}

impl TavilyBackend {
    fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[cfg(feature = "testing")]
#[doc(hidden)]
pub mod test_support {
    use std::time::Duration;

    use super::tavily_timeout_override;

    pub struct TavilyTimeoutOverrideGuard {
        previous: Option<Duration>,
    }

    pub fn override_tavily_timeout_for_test(timeout: Duration) -> TavilyTimeoutOverrideGuard {
        let mut override_slot = tavily_timeout_override().lock().unwrap();
        let previous = override_slot.replace(timeout);
        TavilyTimeoutOverrideGuard { previous }
    }

    impl Drop for TavilyTimeoutOverrideGuard {
        fn drop(&mut self) {
            *tavily_timeout_override().lock().unwrap() = self.previous.take();
        }
    }
}

#[derive(Debug, Serialize)]
struct TavilySearchRequest<'a> {
    query: &'a str,
    search_depth: &'static str,
    max_results: usize,
    include_answer: &'static str,
    include_raw_content: bool,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResponse {
    answer: Option<String>,
    results: Vec<TavilySearchResult>,
}

#[derive(Debug, Deserialize)]
struct TavilySearchResult {
    title: String,
    url: String,
    content: Option<String>,
}

fn tavily_http_status_message(status: StatusCode) -> String {
    format!(
        "Tavily search is unavailable right now (HTTP {}).",
        status.as_u16()
    )
}

fn normalize_tavily_response(
    query: &str,
    max_results: usize,
    response: TavilySearchResponse,
) -> Result<String> {
    if response.results.is_empty() {
        return tavily_no_results_web_search_result(query, "Tavily returned no results.");
    }

    let summary = non_empty_text(response.answer.as_deref())
        .or_else(|| synthesize_summary(&response.results));
    let citations = response
        .results
        .into_iter()
        .take(MAX_RESULTS_CAP)
        .map(|result| WebSearchCitationInput {
            title: result.title,
            url: result.url,
            snippet: non_empty_text(result.content.as_deref()),
        })
        .collect();

    WebSearchResult::ok(query, summary.as_deref(), citations, max_results)
        .map_err(anyhow::Error::msg)?
        .to_json_string()
}

impl WebSearchBackend for TavilyBackend {
    fn execute(&self, query: &str, max_results: usize) -> Result<String> {
        let api_key = self.api_key.clone();
        let query = query.to_string();

        std::thread::spawn(move || {
            let client = build_tavily_client();
            let request = TavilySearchRequest {
                query: &query,
                search_depth: "basic",
                max_results,
                include_answer: "basic",
                include_raw_content: false,
            };

            let response = match client
                .post(tavily_search_url())
                .header("Authorization", format!("Bearer {api_key}"))
                .json(&request)
                .send()
            {
                Ok(response) => response,
                Err(error) if error.is_timeout() => {
                    return tavily_unavailable_web_search_result(
                        &query,
                        "Tavily search timed out before responding.",
                    );
                }
                Err(error) => {
                    return tavily_unavailable_web_search_result(
                        &query,
                        format!("Tavily search transport failed: {error}"),
                    );
                }
            };

            let status = response.status();
            if matches!(status.as_u16(), 401 | 403 | 429 | 432 | 433) || status.is_server_error() {
                return tavily_unavailable_web_search_result(
                    &query,
                    tavily_http_status_message(status),
                );
            }

            if !status.is_success() {
                return tavily_unavailable_web_search_result(
                    &query,
                    tavily_http_status_message(status),
                );
            }

            let response: TavilySearchResponse = match response.json() {
                Ok(response) => response,
                Err(_) => {
                    return tavily_unavailable_web_search_result(
                        &query,
                        "Tavily search returned malformed JSON.",
                    );
                }
            };

            normalize_tavily_response(&query, max_results, response)
        })
        .join()
        .map_err(|_| anyhow::anyhow!("Tavily worker thread panicked"))?
    }
}

enum ResolvedWebSearchBackend {
    Tavily(TavilyBackend),
}

impl ResolvedWebSearchBackend {
    fn execute(&self, query: &str, max_results: usize) -> Result<String> {
        match self {
            ResolvedWebSearchBackend::Tavily(backend) => backend.execute(query, max_results),
        }
    }
}
