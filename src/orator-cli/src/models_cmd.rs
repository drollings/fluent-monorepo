//! Models dispatch: list, pull, rm, verify over the model catalog.
//!
//! Pulls fetch for real through the shared artifact plane (size plus
//! hash verified before the atomic rename); unpinned entries refuse
//! before any network.

use orator_cli::{ExitCode, ModelsAction};

/// Model management: list, pull, rm, verify. Pulls of unpinned entries
/// refuse before any network; verify re-hashes with SHA-256.
pub(crate) fn models(action: ModelsAction) -> ExitCode {
    match action {
        ModelsAction::List => {
            for row in orator_models::list_models() {
                let state = if row.present {
                    match row.verified {
                        Some(true) => "present verified",
                        Some(false) => "present MISMATCH",
                        None => "present",
                    }
                } else {
                    "missing"
                };
                println!("{} ({}) {state}", row.name, row.kind);
            }
            ExitCode::Success
        }
        ModelsAction::Verify { name } => {
            let rows = orator_models::list_models();
            let report = orator_cli::models::verify_report(&rows, &name, |row_name| {
                let Some(entry) = find_model_entry(row_name) else {
                    return Err(format!("no catalog entry for {row_name}"));
                };
                orator_models::verify_entry(&entry).map_err(|error| error.to_string())
            });
            if report.unknown {
                eprintln!("orator: unknown model: {name}");
                return ExitCode::Usage;
            }
            for line in &report.lines {
                println!("{line}");
            }
            if report.failed { ExitCode::Error } else { ExitCode::Success }
        }
        ModelsAction::Rm { name } => {
            let Some(entry) = find_model_entry(&name) else {
                eprintln!("orator: unknown model: {name}");
                return ExitCode::Usage;
            };
            match orator_models::remove_entry(&entry) {
                Ok(true) => {
                    println!("removed {name}");
                    ExitCode::Success
                }
                Ok(false) => {
                    println!("{name}: not present");
                    ExitCode::Success
                }
                Err(error) => {
                    eprintln!("orator: {error}");
                    ExitCode::Error
                }
            }
        }
        ModelsAction::Pull { name } => {
            let Some(entry) = find_model_entry(&name) else {
                eprintln!("orator: unknown model: {name}");
                return ExitCode::Usage;
            };
            match orator_models::pull_plan(&entry) {
                Ok((repo, file, dest)) => {
                    if dest.exists() && orator_models::verify_entry(&entry) == Ok(true) {
                        println!("{name}: already present and verified");
                        return ExitCode::Success;
                    }
                    // Real fetch through the shared artifact plane (size
                    // plus hash verified before the atomic rename), never
                    // a stub error. Production dials Hugging Face.
                    let fetcher = fluent_llm::ReqwestArtifactFetcher::new();
                    match common_core::runtime::block_on(
                        orator_cli::models::pull_single(&entry, &dest, &fetcher),
                    ) {
                        Ok(landed) => {
                            println!("{name}: pulled to {}", landed.display());
                            ExitCode::Success
                        }
                        Err(error) => {
                            eprintln!("orator: pull {repo}/{file} failed: {error}");
                            ExitCode::Error
                        }
                    }
                }
                Err(error) => {
                    eprintln!("orator: {error}");
                    ExitCode::Error
                }
            }
        }
    }
}

/// Look up a catalog entry by tag across families.
fn find_model_entry(name: &str) -> Option<orator_models::ModelEntry> {
    use orator_models::ModelKind;
    for kind in [ModelKind::Whisper, ModelKind::Llm, ModelKind::Tts, ModelKind::Audio, ModelKind::Vad] {
        if let Some(entry) = orator_models::find_entry(kind, name) {
            return Some(entry);
        }
    }
    None
}

