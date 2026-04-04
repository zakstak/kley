use axum::http::header::{HOST, ORIGIN};
use axum::http::{HeaderMap, Uri};

pub(super) fn is_websocket_origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(origin_uri) = origin.parse::<Uri>() else {
        return false;
    };
    let Some(origin_scheme) = origin_uri.scheme_str() else {
        return false;
    };
    if !matches!(origin_scheme, "http" | "https") {
        return false;
    }

    origin_uri
        .authority()
        .map(|authority| authority.as_str().eq_ignore_ascii_case(host))
        .unwrap_or(false)
}
