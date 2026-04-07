use anyhow::{Context, Result};
use std::net::SocketAddr;

pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3210";
pub const WEB_PUBLIC_ORIGIN_ENV: &str = "KLEY_WEB_PUBLIC_ORIGIN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfig {
    pub bind_addr: SocketAddr,
    pub public_origin: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            bind_addr: DEFAULT_BIND_ADDR
                .parse()
                .expect("default web bind address should be valid"),
            public_origin: None,
        }
    }
}

impl WebConfig {
    pub fn from_args(bind: Option<&str>, public_origin: Option<&str>) -> Result<Self> {
        let mut config = match bind {
            Some(bind) => Self::from_bind(bind)?,
            None => Self::default(),
        };

        config.public_origin = public_origin
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| std::env::var(WEB_PUBLIC_ORIGIN_ENV).ok())
            .map(|value| normalize_public_origin(&value))
            .transpose()?;

        Ok(config)
    }

    pub fn from_bind(bind: &str) -> Result<Self> {
        let bind_addr = bind
            .parse()
            .with_context(|| format!("invalid web bind address: {bind}"))?;
        Ok(Self {
            bind_addr,
            public_origin: None,
        })
    }
}

fn normalize_public_origin(origin: &str) -> Result<String> {
    let mut url = reqwest::Url::parse(origin)
        .with_context(|| format!("invalid web public origin: {origin}"))?;
    match url.scheme() {
        "http" | "https" => {}
        _ => anyhow::bail!("invalid web public origin: {origin} (expected http:// or https://)"),
    }
    if url.host_str().is_none() {
        anyhow::bail!("invalid web public origin: {origin} (missing hostname)");
    }
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

#[cfg(test)]
mod tests {
    use super::{WebConfig, normalize_public_origin};

    #[test]
    fn normalize_public_origin_strips_path_query_and_fragment() {
        let normalized = normalize_public_origin("https://kley.example.com:8443/app/?x=1#frag")
            .expect("public origin should normalize");
        assert_eq!(normalized, "https://kley.example.com:8443");
    }

    #[test]
    fn web_config_reads_public_origin_from_args() {
        let config =
            WebConfig::from_args(Some("127.0.0.1:3210"), Some("https://kley.example.com/app"))
                .expect("config should parse");
        assert_eq!(
            config.public_origin.as_deref(),
            Some("https://kley.example.com")
        );
    }
}
