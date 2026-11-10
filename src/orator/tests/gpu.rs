//! Device-probe tests: consistent answers, named refusal without one.

use crate::gpu::{device_present, nvidia_present, require_gpu};

#[test]
fn probe_answers_consistently() {
    assert_eq!(device_present(), device_present());
    assert_eq!(nvidia_present(), nvidia_present());
}

#[test]
fn nvidia_probe_matches_its_device_nodes() {
    let nodes = std::path::Path::new("/dev/nvidia0").exists()
        || std::path::Path::new("/dev/nvidiactl").exists();
    assert_eq!(nvidia_present(), nodes);
    if device_present() && !nodes {
        eprintln!("NOTE nvidia-absent: GPU present but no NVIDIA nodes (e.g. AMD ROCm)");
    }
}

#[test]
fn refusal_names_the_backend_without_a_device() {
    if device_present() {
        eprintln!("SKIP refusal-names-the-backend: GPU device present, nothing to refuse");
        return;
    }
    let error = require_gpu("whisper").expect_err("no device must refuse");
    assert!(error.contains("whisper"), "names the backend: {error}");
    assert!(error.contains("gpu"), "names the remedy: {error}");
}

#[test]
fn presence_permits_every_backend() {
    if !device_present() {
        eprintln!("SKIP presence-permits: no GPU device on this machine");
        return;
    }
    for kind in ["whisper", "kokoro", "llama-server"] {
        require_gpu(kind).expect("present device must permit");
    }
}
