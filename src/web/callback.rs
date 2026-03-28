use axum::extract::Query;
use axum::response::Html;
use serde::Deserialize;
use serde_json::json;

fn callback_input_url(code: &str, state: &str) -> String {
    let mut url = reqwest::Url::parse("http://localhost:1455/auth/callback")
        .expect("callback URL should be valid");
    url.query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", state);
    url.to_string()
}

#[derive(Debug, Deserialize)]
pub struct OpenAiCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

pub async fn openai_callback(Query(query): Query<OpenAiCallbackQuery>) -> Html<String> {
    let payload = if let Some(error) = query.error {
        json!({
            "type": "kley.openai.callback",
            "error": error,
            "error_description": query.error_description,
        })
    } else {
        let callback_input = match (query.code, query.state) {
            (Some(code), Some(state)) => callback_input_url(&code, &state),
            _ => String::new(),
        };
        json!({
            "type": "kley.openai.callback",
            "callback_input": callback_input,
        })
    };

    let payload_json = serde_json::to_string(&payload).unwrap_or_else(|_| {
        r#"{"type":"kley.openai.callback","error":"serialization_failed"}"#.to_string()
    });

    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>OpenAI login callback</title>
</head>
<body>
  <p id="status">OpenAI login callback received. You can close this tab.</p>
  <pre id="fallback" style="display:none;white-space:pre-wrap"></pre>
  <script>
    (function () {{
      const payload = {payload_json};
      if (window.opener && !window.opener.closed) {{
        window.opener.postMessage(payload, "*");
        setTimeout(() => window.close(), 150);
        return;
      }}

      const fallback = document.getElementById("fallback");
      const status = document.getElementById("status");
      if (fallback) {{
        fallback.style.display = "block";
        fallback.textContent = payload.callback_input
          ? `Automatic handoff unavailable. Return to the app and click Open browser login again.\n\nCallback URL:\n${{payload.callback_input}}`
          : "Automatic handoff unavailable. Return to the app and retry login.";
      }}
      if (status) {{
        status.textContent = "Automatic handoff unavailable.";
      }}
    }})();
  </script>
</body>
</html>"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn callback_page_posts_message_payload() {
        let html = openai_callback(Query(OpenAiCallbackQuery {
            code: Some("code-123".into()),
            state: Some("state-abc".into()),
            error: None,
            error_description: None,
        }))
        .await
        .0;

        assert!(html.contains("window.opener.postMessage"));
        assert!(html.contains("kley.openai.callback"));
        assert!(html.contains("code-123"));
        assert!(html.contains("state-abc"));
    }

    #[test]
    fn callback_input_url_encodes_reserved_characters() {
        let encoded = callback_input_url("a+b&c=d", "state/with?symbols=#");
        assert!(encoded.contains("code=a%2Bb%26c%3Dd"), "{encoded}");
        assert!(
            encoded.contains("state=state%2Fwith%3Fsymbols%3D%23"),
            "{encoded}"
        );
    }
}
