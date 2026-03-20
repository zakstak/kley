use anyhow::{Context, Result};
use std::net::SocketAddr;

pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:3210";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebConfig {
    pub bind_addr: SocketAddr,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            bind_addr: DEFAULT_BIND_ADDR
                .parse()
                .expect("default web bind address should be valid"),
        }
    }
}

impl WebConfig {
    pub fn from_bind_arg(bind: Option<&str>) -> Result<Self> {
        match bind {
            Some(bind) => Self::from_bind(bind),
            None => Ok(Self::default()),
        }
    }

    pub fn from_bind(bind: &str) -> Result<Self> {
        let bind_addr = bind
            .parse()
            .with_context(|| format!("invalid web bind address: {bind}"))?;
        Ok(Self { bind_addr })
    }
}
