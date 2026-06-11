//! Capability-gated network I/O (TCP connect, HTTP GET/POST).

use fluent_wvr::{Capability, ConcurrencyError};
use tokio::net::{TcpStream, ToSocketAddrs};

/// Capability-gated network operations.
pub struct NetCapability {
    client: reqwest::Client,
}

impl NetCapability {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
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
