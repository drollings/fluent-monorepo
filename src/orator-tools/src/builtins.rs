//! The v1 tool catalog: two read-only tools, one reversible desktop
//! tool on a sink, and three effectful tools behind the Guarded executor.
//! Screen tools stay out; every name here is defined exactly
//! once and pinned by the doc-guard test.

use std::sync::Arc;

use crate::sandbox::Guarded;
use crate::tool::{Tool, ToolError, ToolRegistry, ToolTier};

fn schema(name: &str, description: &str, properties: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "description": description,
        "parameters": {
            "type": "object",
            "properties": properties,
        },
    })
}

fn arg(name: &str, args: &serde_json::Value) -> Result<String, ToolError> {
    args.get(name)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| ToolError::Args(name.to_string(), "missing string argument".to_string()))
}

/// Local date and time (T0). No arguments.
pub struct TimeNow;

impl Tool for TimeNow {
    fn name(&self) -> &'static str {
        "time_now"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T0
    }
    fn description(&self) -> &'static str {
        "Current local date and time."
    }
    fn schema(&self) -> serde_json::Value {
        schema("time_now", self.description(), &serde_json::json!({}))
    }
    fn execute(&self, _args: serde_json::Value) -> Result<String, ToolError> {
        Ok(chrono::Local::now().format("%Y-%m-%d %H:%M %Z").to_string())
    }
}

/// Arithmetic over `expr` (T0): `+ - * / % ^`, parens, unary minus.
/// Anything else is an argument error, never executed anywhere.
pub struct Calc;

impl Tool for Calc {
    fn name(&self) -> &'static str {
        "calc"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T0
    }
    fn description(&self) -> &'static str {
        "Evaluate an arithmetic expression."
    }
    fn schema(&self) -> serde_json::Value {
        schema(
            "calc",
            self.description(),
            &serde_json::json!({"expr": {"type": "string"}}),
        )
    }
    fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let expr = arg("expr", &args)?;
        eval(&expr).map(|value| format!("{value}"))
    }
}

fn eval(expr: &str) -> Result<f64, ToolError> {
    let mut parser = Parser {
        chars: expr.chars().peekable(),
    };
    let value = parser.sum().map_err(|why| ToolError::Args("expr".to_string(), why))?;
    parser.skip_ws();
    if parser.chars.peek().is_some() {
        return Err(ToolError::Args(
            "expr".to_string(),
            "trailing characters".to_string(),
        ));
    }
    Ok(value)
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.chars.peek().is_some_and(|c| c.is_whitespace()) {
            self.chars.next();
        }
    }

    fn sum(&mut self) -> Result<f64, String> {
        let mut value = self.product()?;
        loop {
            self.skip_ws();
            match self.chars.peek() {
                Some('+') => {
                    self.chars.next();
                    value += self.product()?;
                }
                Some('-') => {
                    self.chars.next();
                    value -= self.product()?;
                }
                _ => return Ok(value),
            }
        }
    }

    fn product(&mut self) -> Result<f64, String> {
        let mut value = self.power()?;
        loop {
            self.skip_ws();
            match self.chars.peek() {
                Some('*') => {
                    self.chars.next();
                    value *= self.power()?;
                }
                Some('/') => {
                    self.chars.next();
                    let divisor = self.power()?;
                    if divisor == 0.0 {
                        return Err("division by zero".to_string());
                    }
                    value /= divisor;
                }
                Some('%') => {
                    self.chars.next();
                    value %= self.power()?;
                }
                _ => return Ok(value),
            }
        }
    }

    fn power(&mut self) -> Result<f64, String> {
        let base = self.unary()?;
        self.skip_ws();
        if self.chars.peek() == Some(&'^') {
            self.chars.next();
            let exp = self.power()?;
            return Ok(base.powf(exp));
        }
        Ok(base)
    }

    fn unary(&mut self) -> Result<f64, String> {
        self.skip_ws();
        if self.chars.peek() == Some(&'-') {
            self.chars.next();
            return Ok(-self.unary()?);
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<f64, String> {
        self.skip_ws();
        match self.chars.peek() {
            Some('(') => {
                self.chars.next();
                let value = self.sum()?;
                self.skip_ws();
                if self.chars.next() != Some(')') {
                    return Err("missing )".to_string());
                }
                Ok(value)
            }
            Some(c) if c.is_ascii_digit() || *c == '.' => {
                let mut text = String::new();
                while self
                    .chars
                    .peek()
                    .is_some_and(|c| c.is_ascii_digit() || *c == '.')
                {
                    text.push(self.chars.next().expect("peeked"));
                }
                text.parse().map_err(|_| format!("bad number {text:?}"))
            }
            Some(c) => Err(format!("unexpected {c:?}")),
            None => Err("empty expression".to_string()),
        }
    }
}

/// Desktop notification (T1). Delivery rides the injected sink; the
/// desktop surface wires the real one later.
pub struct Notify {
    sink: Arc<dyn Fn(String) + Send + Sync>,
}

impl Notify {
    /// Notifier pushing rendered text into `sink`.
    pub fn new(sink: Arc<dyn Fn(String) + Send + Sync>) -> Self {
        Self { sink }
    }
}

impl Tool for Notify {
    fn name(&self) -> &'static str {
        "notify"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T1
    }
    fn description(&self) -> &'static str {
        "Show a desktop notification."
    }
    fn schema(&self) -> serde_json::Value {
        schema(
            "notify",
            self.description(),
            &serde_json::json!({"text": {"type": "string"}}),
        )
    }
    fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let text = arg("text", &args)?;
        (self.sink)(text);
        Ok("notified".to_string())
    }
}

/// Shell command through the Guarded executor (T2). Always asks.
pub struct ShellTool {
    guarded: Arc<Guarded>,
}

impl ShellTool {
    /// Shell tool running under `guarded`.
    pub fn new(guarded: Arc<Guarded>) -> Self {
        Self { guarded }
    }
}

impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T2
    }
    fn description(&self) -> &'static str {
        "Run a shell command under the Guarded sandbox."
    }
    fn schema(&self) -> serde_json::Value {
        schema(
            "shell",
            self.description(),
            &serde_json::json!({"command": {"type": "string"}}),
        )
    }
    fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let command = arg("command", &args)?;
        let argv = vec!["sh".to_string(), "-c".to_string(), command];
        let result = common_core::runtime::block_on(self.guarded.run("shell", &argv))?;
        if result.timed_out {
            return Err(ToolError::Timeout("shell".to_string(), result.ms));
        }
        Ok(crate::tool::truncate_tool_output(&result.output))
    }
}

/// File read inside the jail (T1). Content re-enters the prompt as
/// untrusted data and taints the turn.
pub struct FileRead {
    guarded: Arc<Guarded>,
}

impl FileRead {
    /// Reader bound to `guarded`'s jail root.
    pub fn new(guarded: Arc<Guarded>) -> Self {
        Self { guarded }
    }
}

impl Tool for FileRead {
    fn name(&self) -> &'static str {
        "file_read"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T1
    }
    fn description(&self) -> &'static str {
        "Read a file inside the allowed root."
    }
    fn schema(&self) -> serde_json::Value {
        schema(
            "file_read",
            self.description(),
            &serde_json::json!({"path": {"type": "string"}}),
        )
    }
    fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let path = arg("path", &args)?;
        let mut file = self
            .guarded
            .open_within(
                std::path::Path::new(&path),
                std::fs::OpenOptions::new().read(true),
            )
            .map_err(|error| ToolError::Denied(error.to_string()))?;
        use std::io::Read as _;
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|error| ToolError::Failed("file_read".to_string(), error.to_string()))?;
        Ok(crate::tool::truncate_tool_output(&text))
    }
}

/// File write inside the jail (T2). Always asks.
pub struct FileWrite {
    guarded: Arc<Guarded>,
}

impl FileWrite {
    /// Writer bound to `guarded`'s jail root.
    pub fn new(guarded: Arc<Guarded>) -> Self {
        Self { guarded }
    }
}

impl Tool for FileWrite {
    fn name(&self) -> &'static str {
        "file_write"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T2
    }
    fn description(&self) -> &'static str {
        "Write a file inside the allowed root."
    }
    fn schema(&self) -> serde_json::Value {
        schema(
            "file_write",
            self.description(),
            &serde_json::json!({"path": {"type": "string"}, "content": {"type": "string"}}),
        )
    }
    fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let path = arg("path", &args)?;
        let content = arg("content", &args)?;
        let candidate = self
            .guarded
            .within(std::path::Path::new(&path))
            .map_err(|error| ToolError::Denied(error.to_string()))?;
        if let Some(parent) = candidate.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ToolError::Failed("file_write".to_string(), error.to_string()))?;
        }
        let mut file = self
            .guarded
            .open_within(
                std::path::Path::new(&path),
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true),
            )
            .map_err(|error| ToolError::Denied(error.to_string()))?;
        use std::io::Write as _;
        file.write_all(content.as_bytes())
            .map_err(|error| ToolError::Failed("file_write".to_string(), error.to_string()))?;
        Ok("written".to_string())
    }
}

/// Opt-in web fetch (T2): 15 s timeout, at most 5 redirects, HTML
/// stripped, truncated at 6 000 chars with a total-length marker.
/// Fetched text re-enters the prompt as untrusted data.
pub struct WebFetch;

impl Tool for WebFetch {
    fn name(&self) -> &'static str {
        "web_fetch"
    }
    fn tier(&self) -> ToolTier {
        ToolTier::T2
    }
    fn description(&self) -> &'static str {
        "Fetch a URL as text (15 s, 5 redirects, 6k chars)."
    }
    fn schema(&self) -> serde_json::Value {
        schema(
            "web_fetch",
            self.description(),
            &serde_json::json!({"url": {"type": "string"}}),
        )
    }
    fn execute(&self, args: serde_json::Value) -> Result<String, ToolError> {
        let url = arg("url", &args)?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ToolError::Args("url".to_string(), "http(s) only".to_string()));
        }
        common_core::runtime::block_on(fetch(&url))
    }
}

/// Fetch cap in chars; the marker carries the total.
pub const FETCH_CHARS: usize = crate::tool::TOOL_OUTPUT_CHARS;

async fn fetch(url: &str) -> Result<String, ToolError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| ToolError::Failed("web_fetch".to_string(), error.to_string()))?;
    let body = client
        .get(url)
        .send()
        .await
        .map_err(|error| ToolError::Failed("web_fetch".to_string(), error.to_string()))?
        .text()
        .await
        .map_err(|error| ToolError::Failed("web_fetch".to_string(), error.to_string()))?;
    let text = common_core::string::collapse_whitespace(&common_core::string::strip_html(&body));
    Ok(crate::tool::truncate_tool_output(&text))
}

/// The v1 catalog. `guarded` backs the effectful tools; `notify_sink`
/// receives notification text.
#[must_use]
pub fn default_registry(
    guarded: &Arc<Guarded>,
    notify_sink: Arc<dyn Fn(String) + Send + Sync>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in [
        Arc::new(TimeNow) as Arc<dyn Tool>,
        Arc::new(Calc),
        Arc::new(Notify::new(notify_sink)),
        Arc::new(ShellTool::new(Arc::clone(guarded))),
        Arc::new(FileRead::new(Arc::clone(guarded))),
        Arc::new(FileWrite::new(Arc::clone(guarded))),
        Arc::new(WebFetch),
    ] {
        registry.register(tool).expect("v1 names are unique");
    }
    registry
}

#[cfg(test)]
#[path = "../tests/builtins.rs"]
mod tests;
