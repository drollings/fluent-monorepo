//! Process-wide shared HTTP client (and test variants).
//!
//! Use `shared_http_client()` in production code to amortize TLS handshakes
//! and connection pool setup. Use `test_http_client()` in tests to get a
//! client with a consistent User-Agent for log filtering.

use std::sync::OnceLock;
use std::time::Duration;

static SHARED: OnceLock<reqwest::Client> = OnceLock::new();

/// Returns a process-wide shared `reqwest::Client` with sensible defaults.
/// Amortizes TLS handshakes and connection pool setup across all callers.
/// Proxy env is ignored (local inference never proxies) and redirects
/// are not followed automatically — callers that need them (WebFetch)
/// follow manually with a per-hop SSRF re-check.
pub fn shared_http_client() -> &'static reqwest::Client {
    SHARED.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("common-core/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build shared reqwest client")
    })
}

#[cfg(any(test, feature = "test-util"))]
static TEST: OnceLock<reqwest::Client> = OnceLock::new();

/// Returns a shared `reqwest::Client` for test code.
///
/// Two deliberate departures from `shared_http_client()`'s production
/// settings, both because mock tests are their own worst-case caller:
///
/// - **Connection pooling is off** (`pool_max_idle_per_host(0)`). In
///   hyper-util that does not merely stop idle sockets accumulating, it sets
///   `Pool::is_enabled()` to false, so `Client::one_connection_for` skips the
///   `future::select(pool.checkout(key), connect)` race entirely and goes
///   straight to `connect`. That race is unsound across the short-lived
///   per-test runtimes that mock suites are made of: an idle connection
///   parked in a process-wide pool is handed to whichever test asks next, and
///   hyper-util's handoff of a not-yet-ready connection is a `tokio::spawn`
///   on *whatever runtime is current* — in a mock suite, a `#[tokio::test]`
///   runtime that is torn down microseconds later. The next test then parks
///   in `Pooled::poll_ready` with no socket, no connect timer, and a waker
///   owned by a dead runtime: never woken, never recovered. Each mocked
///   request to a loopback server gains nothing from keep-alive, so turning
///   pooling off is free.
/// - **A generous total timeout.** Loopback mocks answer in milliseconds, so
///   a minute without a byte means wedged test plumbing, not a slow server.
///   The bound turns any residual stall into a loud failure instead of
///   hanging the whole suite.
#[cfg(any(test, feature = "test-util"))]
pub fn test_http_client() -> &'static reqwest::Client {
    TEST.get_or_init(|| {
        reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .timeout(Duration::from_secs(60))
            .build()
            .expect("build test reqwest client")
    })
}

// NOTE (ROADMAP_20260903_LLM M11): the CORS contract (`CORS_*`,
// `add_cors_headers`, now owned by `fluent_router::server::cors`) and the
// error envelope (`error_value`, now `fluent_llm::openai::error_response`)
// lived here through M10 as deprecated byte-identical shims; M11 deleted
// them. The generic client + body helpers below stay.

/// JSON body string for `value`.
pub fn json_body(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
