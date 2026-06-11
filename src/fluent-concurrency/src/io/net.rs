//! Capability-gated network I/O (TCP connect, HTTP GET/POST).

use std::time::Duration;

use fluent_wvr::{Capability, ConcurrencyError};
use tokio::net::{TcpStream, ToSocketAddrs};

/// Capability-gated network operations.
pub struct NetCapability {
    client: reqwest::Client,
}

impl NetCapability {
    pub fn new() -> Self {
        Self::with_config(&NetConfig::default())
    }

    pub fn with_config(config: &NetConfig) -> Self {
        let mut builder = reqwest::Client::builder()
            .pool_max_idle_per_host(config.max_idle_per_host)
            .pool_idle_timeout(config.idle_timeout)
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout);

        if let Some(user_agent) = &config.user_agent {
            builder = builder.user_agent(user_agent);
        }

        let client = builder
            .build()
            .expect("failed to build reqwest client");

        Self { client }
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }
}

pub struct NetConfig {
    pub max_idle_per_host: usize,
    pub idle_timeout: Duration,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub user_agent: Option<String>,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            max_idle_per_host: 4,
            idle_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            user_agent: None,
        }
    }
}

impl Default for NetCapability {
    fn default() -> Self {
        Self::new()
    }
}

impl Capability for NetCapability {
    fn name(&self) -> &'static str {
        "net"
    }
}

impl NetCapability {
    pub async fn tcp_connect(
        &self,
        addr: impl ToSocketAddrs,
    ) -> Result<TcpStream, ConcurrencyError> {
        Ok(TcpStream::connect(addr).await?)
    }

    pub async fn http_get(&self, url: &str) -> Result<String, ConcurrencyError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;
        let body = response
            .text()
            .await
            .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;
        Ok(body)
    }

    pub async fn http_post(
        &self,
        url: &str,
        body: &str,
    ) -> Result<String, ConcurrencyError> {
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;
        let response_body = response
            .text()
            .await
            .map_err(|e| ConcurrencyError::Io(std::io::Error::other(e)))?;
        Ok(response_body)
    }
}
