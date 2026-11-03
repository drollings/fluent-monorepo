//! Ask dispatch: one supervised question over the LLM endpoint.
//!
//! A live endpoint is used as-is; otherwise this command owns a
//! supervised server for exactly one question, then stops it.

use orator_cli::{
    config::{gpu_llm_args, resolve_llm_base},
    socket::Transport,
    ExitCode,
};
use orator_core::{LlmEndpointConfig, LlmSession, OratorConfig};
use orator_llm::{probe_models, HttpSession, Supervisor, SupervisorConfig};

use crate::say::say_via_daemon;

/// One question through the supervised endpoint: probe the configured
/// address, boot a supervised server when nothing answers, ask, print.
/// One-shot CLI use keeps one-question ownership only when it booted the
/// server itself; the per-process model-load cost is retained by design
/// (no cross-command server lease in v1).
pub(crate) fn ask(question: &str, speak: bool, config: &OratorConfig, transport: Transport) -> ExitCode {
    let llm = config.llm.clone();
    let explicit = std::env::var("LLM_BASE_URL").unwrap_or_default();
    let (base, model) = resolve_llm_base(&llm);
    common_core::runtime::block_on_shared(async {
        // A live endpoint is used as-is; otherwise this command owns a
        // supervised server for exactly one question.
        let supervised: Option<std::sync::Arc<Supervisor>> = if probe_models(
            &base,
            common_core::http::shared_http_client(),
        )
        .await
        .is_err()
            && explicit.is_empty()
        {
            match boot_supervised(&llm).await {
                Ok(server) => Some(std::sync::Arc::new(server)),
                Err(orator_cli::ask::AskFail::Usage(message)) => {
                    eprintln!("{message}");
                    return ExitCode::Usage;
                }
                Err(orator_cli::ask::AskFail::Error(message)) => {
                    eprintln!("{message}");
                    return ExitCode::Error;
                }
            }
        } else {
            None
        };
        // A shutdown mid-question stops the owned server; the step
        // below then fails fast instead of hanging on a dead child.
        let watch = supervised.clone().map(|server| {
            orator_cli::SignalWatch::arm(move || server.stop())
        });
        let live_base = supervised
            .as_ref()
            .map(|server| server.base_url().to_string())
            .unwrap_or(base);
        let mut session = HttpSession::open(
            &live_base,
            &model,
            usize::from(llm.ctx),
            usize::from(llm.n_batch),
        );
        session.push_text(question);
        if watch.as_ref().is_some_and(orator_cli::SignalWatch::signalled) {
            return ExitCode::Interrupted;
        }
        let code = match session.step(&mut orator_core::Sampler::default()) {
            Ok(out) => {
                let reply = out.delta.trim_end().to_string();
                println!("{reply}");
                if speak {
                    // Full-duplex rule: speak concisely, print fully.
                    // Long replies voice a two-sentence summary plus
                    // an announcement; the complete text is already
                    // printed above, so nothing is lost.
                    let spoken = orator_cli::ask::concise_for_speech(&reply);
                    let code = say_via_daemon(&spoken, transport);
                    if code != ExitCode::Success {
                        eprintln!("{}", orator_cli::ask::speak_failure_note(code));
                    }
                    code
                } else {
                    ExitCode::Success
                }
            }
            Err(error) => {
                eprintln!("orator: {error}");
                ExitCode::Error
            }
        };
        if watch.as_ref().is_some_and(orator_cli::SignalWatch::signalled) {
            return ExitCode::Interrupted;
        }
        if let Some(server) = supervised {
            server.stop();
        }
        code
    })
}

/// Boot the configured endpoint under supervision: weights resolve from
/// the model home when present, otherwise the boot refuses fast naming
/// the expected path (no 60 s health burn on known-missing files). A
/// GPU opt-in without a visible device refuses before boot; with one
/// it requests full layer offload. Garbage ports are usage errors and
/// never silently auto-reserve elsewhere.
async fn boot_supervised(endpoint: &LlmEndpointConfig) -> Result<Supervisor, orator_cli::ask::AskFail> {    use orator_cli::ask::AskFail;
    if !endpoint.auto_boot {
        return Err(AskFail::Error(format!(
            "orator: no LLM server answers at {} (start a llama-server there, or set llm.auto_boot to boot one automatically)",
            endpoint.endpoint,
        )));
    }
    if endpoint.gpu {
        orator_core::gpu::require_gpu("llama-server").map_err(AskFail::Error)?;
    }
    let weights =
        orator_cli::ask::check_ask_weights(endpoint).map_err(AskFail::Error)?;
    let port = orator_cli::ask::parse_ask_port(&endpoint.endpoint).map_err(AskFail::Usage)?;
    let mut config = SupervisorConfig {
        model: endpoint.model.clone(),
        weights: Some(weights),
        ctx: endpoint.ctx,
        n_batch: endpoint.n_batch,
        port,
        ..SupervisorConfig::default()
    };
    config.extra_args.extend(gpu_llm_args(endpoint.gpu));
    config.health_timeout = std::time::Duration::from_secs(60);
    Supervisor::boot(config)
        .await
        .map_err(|error| AskFail::Error(error.to_string()))
}

