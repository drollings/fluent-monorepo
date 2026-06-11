//! Capability-gated network I/O (TCP connect, HTTP GET/POST).

use fluent_wvr::{Capability, ConcurrencyError};
use tokio::net::{TcpStream, ToSocketAddrs};

/// Capability-gated network operations.
pub struct NetCapability;

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
        let url = url.to_string();
        let result = tokio::task::spawn_blocking(move || -> Result<String, ConcurrencyError> {
            let response = ureq::get(&url).call().map_err(|e| {
                ConcurrencyError::Io(std::io::Error::other(e.to_string()))
            })?;
            let body = response.into_body().read_to_string().map_err(|e| {
                ConcurrencyError::Io(std::io::Error::other(e.to_string()))
            })?;
            Ok(body)
        })
        .await
        .map_err(|e| {
            ConcurrencyError::Io(std::io::Error::other(e))
        })?;
        result
    }

    pub async fn http_post(
        &self,
        url: &str,
        body: &str,
    ) -> Result<String, ConcurrencyError> {
        let url = url.to_string();
        let body = body.to_string();
        let result = tokio::task::spawn_blocking(move || -> Result<String, ConcurrencyError> {
            let response = ureq::post(&url)
                .header("Content-Type", "application/json")
                .send(body.as_bytes())
                .map_err(|e| {
                    ConcurrencyError::Io(std::io::Error::other(e.to_string()))
                })?;
            let response_body = response.into_body().read_to_string().map_err(|e| {
                ConcurrencyError::Io(std::io::Error::other(e.to_string()))
            })?;
            Ok(response_body)
        })
        .await
        .map_err(|e| {
            ConcurrencyError::Io(std::io::Error::other(e))
        })?;
        result
    }
}
