//! Server admin commands (`ps`, `stop`, `speedtest`, `coordinator-rollback`)
//! driving a running Coral Router through its HTTP API.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::{json, Value};

use super::SpeedtestArgs;
use super::{cli_err, load_config, router_base_url, TableFormat};
use crate::cli::gguf::resolve_model;
use crate::cli::ps_table::{collect_ps_rows, render_ps_markdown, render_ps_table};
use crate::cli::{CliContext, CliError, CliResult};
use crate::config::RouterConfig;

pub async fn ps(
    ctx: &CliContext,
    api_url: Option<&str>,
    config_path: Option<&Path>,
    format: TableFormat,
) -> CliResult {
    let base = router_base_url(api_url, config_path);
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    let config = load_config(config_path);

    // Both reads are independent, and each fans out server-side to a
    // sequential per-manager poll of every managed `llama-server` — a busy
    // server stalls its poll, so the two fetches run concurrently rather
    // than stacking their stalls back to back.
    let models_url = format!("{base}/v1/models");
    let instances_url = format!("{base}/instances");
    let (models, instances) = tokio::join!(
        get_json(&client, &models_url),
        get_json(&client, &instances_url),
    );
    let models = models.map_err(|e| {
        cli_err(format!(
            "Error: llama-server not reachable. Is it running? {e}"
        ))
    })?;
    // Every known model is listed — `loaded`/`sleeping` (resident) and
    // `unloaded` (registered, builds on first demand) alike — so a fresh
    // boot shows the fleet before any traffic flows. Each instance row
    // carries its own `state`; the per-model block is informational, never
    // a residency filter.
    let listed: Vec<Value> = models
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if listed.is_empty() {
        println!("(no loaded models)");
        return Ok(());
    }

    let instances = instances.unwrap_or_else(|_| json!({ "instances": [] }));
    let instance_list = instances
        .get("instances")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Retained snapshots ride the same envelope (`snapshots`, each tagged
    // with its owning `model`): an offloaded model with snapshots renders
    // distinctly even when it has no instance detail left.
    let snapshots = instances
        .get("snapshots")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // One compact table over the whole fleet (see `ps_table`): every
    // instance is a row carrying its context window, weights and KV-cache
    // quantization, sleep policy, state, and resident memory — aligned
    // plain-text columns by default, Markdown pipe tables on `--markdown`.
    let (rows, totals) = collect_ps_rows(
        config.as_ref(),
        &ctx.gguf_dir,
        &listed,
        &instance_list,
        &snapshots,
    );
    if rows.is_empty() {
        println!("(no loaded models)");
        return Ok(());
    }
    match format {
        TableFormat::Text => print!("{}", render_ps_table(&rows, &totals)),
        TableFormat::Markdown => print!("{}", render_ps_markdown(&rows, &totals)),
    }
    Ok(())
}

// ── stop ────────────────────────────────────────────────────────────────────

/// `coral-router stop <model>` — unload a running model via `POST
/// /models/unload` and wait for it to disappear from `/v1/models`.
pub async fn stop(
    ctx: &CliContext,
    api_url: Option<&str>,
    config_path: Option<&Path>,
    model: &str,
    force: bool,
) -> CliResult {
    let base = router_base_url(api_url, config_path);
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    let config = load_config(config_path);
    let model_key = resolve_router_model_key(config.as_ref(), &ctx.gguf_dir, model)
        .ok_or_else(|| cli_err(format!("model '{model}' not found")))?;
    if force {
        ctx.log_debug("force=true — the router owns the server; unload is graceful");
    }
    println!("Unloading model '{model_key}' ...");

    let resp = client
        .post(format!("{base}/models/unload"))
        .json(&json!({ "model": model_key }))
        .send()
        .await
        .map_err(|e| cli_err(format!("Cannot connect to {base}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if status == 404 {
            println!("Model '{model_key}' has no managed server (nothing to unload)");
            return Ok(());
        }
        return Err(cli_err(format!(
            "/models/unload failed: HTTP {status}: {text}"
        )));
    }

    for attempt in 0..6 {
        let loaded = loaded_model_ids(&client, &base).await;
        if !loaded.contains(&model_key) {
            println!("Model '{model_key}' unloaded");
            return Ok(());
        }
        if attempt < 5 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Err(cli_err(format!(
        "Model '{model_key}' still listed as loaded after unload"
    )))
}

/// One-command frame rollback: close the running router's frame-resolution
/// runtime gate so new jobs drop before any index read or model call without
/// a restart. Loopback-only server side; idempotent.
pub async fn frame_rollback(
    ctx: &CliContext,
    api_url: Option<&str>,
    config_path: Option<&Path>,
) -> CliResult {
    let base = router_base_url(api_url, config_path);
    ctx.log_debug("frame-rollback: closing the frame-resolution runtime gate");
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    let resp = client
        .post(format!("{base}/admin/frame/rollback"))
        .send()
        .await
        .map_err(|e| cli_err(format!("Cannot connect to {base}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(cli_err(format!(
            "/admin/frame/rollback failed: HTTP {status}: {text}"
        )));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| cli_err(format!("rollback reply unreadable: {e}")))?;
    if body.get("status").and_then(Value::as_str) == Some("rolled_back") {
        println!("Frame resolution rolled back: new jobs drop before any model call");
    } else {
        println!("Rollback reply: {body}");
    }
    Ok(())
}

/// One-command coordinator rollback (M14c): close the running router's
/// coordinator runtime gate so dispatch returns to the pre-coordinator
/// pipeline without a restart. Loopback-only server side; idempotent.
pub async fn coordinator_rollback(
    ctx: &CliContext,
    api_url: Option<&str>,
    config_path: Option<&Path>,
) -> CliResult {
    let base = router_base_url(api_url, config_path);
    ctx.log_debug("coordinator-rollback: closing the coordinator runtime gate");
    let client = Client::builder().timeout(Duration::from_secs(10)).build()?;
    let resp = client
        .post(format!("{base}/admin/coordinator/rollback"))
        .send()
        .await
        .map_err(|e| cli_err(format!("Cannot connect to {base}: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(cli_err(format!(
            "/admin/coordinator/rollback failed: HTTP {status}: {text}"
        )));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| cli_err(format!("rollback reply unreadable: {e}")))?;
    if body.get("status").and_then(Value::as_str) == Some("rolled_back") {
        println!("Coordinator rolled back: dispatch is on the pre-coordinator pipeline");
    } else {
        println!("Rollback reply: {body}");
    }
    Ok(())
}

async fn loaded_model_ids(client: &Client, base: &str) -> Vec<String> {
    let Ok(models) = get_json(client, &format!("{base}/v1/models")).await else {
        return Vec::new();
    };
    // Residency, not mere presence: `/v1/models` also lists registered but
    // unloaded windows (lazy servers, config-synthesized rows), which never
    // drain — `stop` waits only for built contexts to go away.
    models
        .get("data")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|m| {
                    m.get("state")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s == "loaded" || s == "sleeping")
                })
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Map a CLI model argument to a router config model key: the exact key, a
/// `key:qualifier` base, or a GGUF-layout name resolved against configured
/// weights paths.
fn resolve_router_model_key(
    config: Option<&RouterConfig>,
    gguf_dir: &Path,
    arg: &str,
) -> Option<String> {
    let arg = arg.to_lowercase();
    let config = config?;
    if config.models.contains_key(&arg) {
        return Some(arg);
    }
    let (base, _) = arg.rsplit_once(':').unwrap_or((arg.as_str(), ""));
    if config.models.contains_key(base) {
        return Some(base.to_string());
    }
    let resolved = resolve_model(gguf_dir, &arg)?;
    config.models.iter().find_map(|(key, entry)| {
        entry
            .weights
            .as_deref()
            .is_some_and(|w| {
                crate::config::RouterConfig::join_config_dir(gguf_dir, w) == resolved.path
            })
            .then(|| key.clone())
    })
}

// ── speedtest ───────────────────────────────────────────────────────────────

/// `coral-router speedtest` — measure generation throughput: runs a chat
/// completion through the router, reports per-run tokens/s plus the server's
/// lifetime-average gauges from `/metrics`.
pub async fn speedtest(
    ctx: &CliContext,
    api_url: Option<&str>,
    config_path: Option<&Path>,
    args: &SpeedtestArgs,
) -> CliResult {
    let base = router_base_url(api_url, config_path);
    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;
    let config = load_config(config_path);

    let model_name = resolve_speedtest_model(config.as_ref(), &args.model);
    if model_name.is_empty() {
        return Err(cli_err(
            "no model to benchmark: pass --model, or configure a default_route with a model group",
        ));
    }
    let prompt = args
        .prompt
        .as_deref()
        .unwrap_or("Write a brief overview of machine learning, at least 4000 words.");
    let run_test = args.tokens > 0;

    let baseline = if run_test {
        Some(
            fetch_model_metrics(&client, &base, &model_name)
                .await
                .map_err(|e| cli_err(format!("Cannot read metrics from {base}: {e}")))?,
        )
    } else {
        None
    };

    let wall = if run_test {
        let payload = json!({
            "model": model_name,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": args.tokens,
            "temperature": args.temperature,
            "stream": false,
        });
        let wall_start = Instant::now();
        let resp = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| cli_err(format!("Generation request failed: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(cli_err(format!(
                "Generation request failed: HTTP {status}: {text}"
            )));
        }
        let data: Value = resp.json().await?;
        ctx.log_debug(&format!("completion: {data}"));
        let elapsed = wall_start.elapsed();
        let usage = data.get("usage").cloned().unwrap_or_else(|| json!({}));
        speedtest_report(&usage, elapsed);
        elapsed.as_secs_f64()
    } else {
        0.0
    };

    let final_metrics = fetch_model_metrics(&client, &base, &model_name)
        .await
        .map_err(|e| cli_err(format!("Cannot read metrics after generation: {e}")))?;

    print_speedtest_summary(
        &model_name,
        run_test,
        baseline.as_ref(),
        &final_metrics,
        wall,
    );
    Ok(())
}

/// Resolve the speedtest model: the arg if given, else the default route's
/// first model, else the first configured model key. Role members fan out to
/// their head candidate so a role-first route still yields a concrete model.
fn resolve_speedtest_model(config: Option<&RouterConfig>, arg: &str) -> String {
    if !arg.is_empty() {
        return arg.to_string();
    }
    if let Some(cfg) = config {
        if let Some(route) = cfg.routes_view().get(&cfg.default_route) {
            if let Some(role) = cfg.roles.get(&route.role) {
                let effective = role.effective_models();
                if let Some(first) = effective.first() {
                    if cfg.roles.contains_key(first) {
                        if let Some(head) =
                            crate::config::models_serving_role(&cfg.roles, &cfg.models, first)
                                .into_iter()
                                .next()
                        {
                            return head;
                        }
                    }
                    return first.clone();
                }
            }
        }
        let mut keys: Vec<&String> = cfg.models.keys().collect();
        keys.sort_unstable();
        if let Some(first) = keys.first() {
            return (*first).clone();
        }
    }
    String::new()
}

fn speedtest_report(usage: &Value, elapsed: Duration) {
    let prompt_tokens = usage
        .get("prompt_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = usage
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let secs = elapsed.as_secs_f64();
    eprintln!(
        "  completion: {completion_tokens} tokens in {secs:.2}s ({:.1} tok/s), prompt {prompt_tokens} tokens",
        completion_tokens as f64 / secs.max(1e-9)
    );
}

fn print_speedtest_summary(
    model_name: &str,
    run_test: bool,
    baseline: Option<&HashMap<String, f64>>,
    final_metrics: &HashMap<String, f64>,
    wall: f64,
) {
    let prompt_avg_tps = final_metrics
        .get("llamacpp:prompt_tokens_seconds")
        .copied()
        .unwrap_or(0.0);
    let gen_avg_tps = final_metrics
        .get("llamacpp:predicted_tokens_seconds")
        .copied()
        .unwrap_or(0.0);

    println!("Speedtest · {model_name}");
    println!(
        "{:<12} {:>10} {:>10} {:>14}",
        "Phase", "Tokens", "Time (s)", "Speed (tok/s)"
    );
    if run_test {
        if let Some(b) = baseline {
            let delta = |name: &str| {
                final_metrics.get(name).copied().unwrap_or(0.0)
                    - b.get(name).copied().unwrap_or(0.0)
            };
            let tok_prompt = delta("llamacpp:prompt_tokens_total");
            let time_prompt = delta("llamacpp:prompt_seconds_total");
            let tok_gen = delta("llamacpp:tokens_predicted_total");
            let time_gen = delta("llamacpp:tokens_predicted_seconds_total");
            let tok_total = tok_prompt + tok_gen;
            println!(
                "{:<12} {:>10.0} {:>10.2} {:>14.1}",
                "Prompt",
                tok_prompt,
                time_prompt,
                fmt_speed(tok_prompt, time_prompt)
            );
            println!(
                "{:<12} {:>10.0} {:>10.2} {:>14.1}",
                "Generation",
                tok_gen,
                time_gen,
                fmt_speed(tok_gen, time_gen)
            );
            println!(
                "{:<12} {:>10.0} {:>10.2} {:>14.1}",
                "Total",
                tok_total,
                wall,
                fmt_speed(tok_total, wall)
            );
        }
    }
    println!(
        "{:<12} {:>10} {:>10} {:>14.1}",
        "Prompt (avg)",
        "—",
        "—",
        fmt_speed(prompt_avg_tps, 1.0)
    );
    println!(
        "{:<12} {:>10} {:>10} {:>14.1}",
        "Generation (avg)",
        "—",
        "—",
        fmt_speed(gen_avg_tps, 1.0)
    );
}

fn fmt_speed(tokens: f64, seconds: f64) -> f64 {
    if seconds > 0.0 && tokens > 0.0 {
        tokens / seconds
    } else {
        0.0
    }
}

/// Fetch `/metrics?model=<model>` from the router and parse the Prometheus
/// text exposition into a metric-name → value map.
async fn fetch_model_metrics(
    client: &Client,
    base: &str,
    model: &str,
) -> Result<HashMap<String, f64>, CliError> {
    let url = format!("{base}/metrics?model={}", urlencoding(model));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| cli_err(format!("metrics request failed: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(cli_err(format!("metrics request failed: HTTP {status}")));
    }
    let text = resp.text().await.map_err(|e| cli_err(e.to_string()))?;
    Ok(parse_prometheus_metrics(&text))
}

fn urlencoding(s: &str) -> String {
    s.replace(':', "%3A").replace('/', "%2F")
}

/// Parse a Prometheus text exposition into `{metric_name: value}`.
pub(super) fn parse_prometheus_metrics(text: &str) -> HashMap<String, f64> {
    let mut metrics = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name_part, value_str)) = line.rsplit_once(' ') else {
            continue;
        };
        let name = name_part.split('{').next().unwrap_or(name_part).trim();
        if let Ok(value) = value_str.parse::<f64>() {
            metrics.insert(name.to_string(), value);
        }
    }
    metrics
}

// ── shared HTTP helpers ─────────────────────────────────────────────────────

/// ── shared HTTP helpers ─────────────────────────────────────────────────────
async fn get_json(client: &Client, url: &str) -> Result<Value, CliError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| cli_err(e.to_string()))?;
    if !resp.status().is_success() {
        let status = resp.status();
        return Err(cli_err(format!("GET {url} -> HTTP {status}")));
    }
    resp.json().await.map_err(|e| cli_err(e.to_string()))
}