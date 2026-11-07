//! Moonshine backend tests: refusal shapes, API contract — all
//! hermetic (missing files, no device; no weights, no model).

use crate::moonshine::MoonshineBackend;

#[test]
fn missing_file_refuses_with_the_pull_remedy() {
    let backend = MoonshineBackend::new("/nonexistent/moonshine.gguf");
    assert!(!backend.is_loaded(), "nothing resident before load");
    let error = backend
        .decode_blocking(
            &[0.0; 1600],
            &fluent_concurrency::stream::StreamAbort::new(),
        )
        .expect_err("missing weights refuse");
    assert!(error.to_string().contains("missing"), "{error:?}");
}

#[test]
fn gpu_without_a_device_refuses_actionably() {
    use fluent_concurrency::stream::StreamAbort;
    if orator_core::gpu::device_present() {
        eprintln!("SKIP gpu-without-device: GPU device present, nothing to refuse");
        return;
    }
    let backend = MoonshineBackend::new("/nonexistent/moonshine.gguf").with_gpu();
    let error = backend
        .decode_blocking(&[0.0; 1600], &StreamAbort::new())
        .expect_err("GPU without a device must fail");
    assert!(
        error.to_string().contains("GPU"),
        "the flag reaches the backend: {error:?}"
    );
}

#[test]
fn gpu_without_rocm_build_names_the_rebuild() {
    use fluent_concurrency::stream::StreamAbort;
    if !orator_core::gpu::device_present() {
        eprintln!("SKIP rocm-build-check: no GPU device on this machine");
        return;
    }
    let backend = MoonshineBackend::new("/nonexistent/moonshine.gguf").with_gpu();
    let error = backend
        .decode_blocking(&[0.0; 1600], &StreamAbort::new())
        .expect_err("gpu opt-in must resolve before any weights load");
    // Default (CPU-only) builds name the rocm rebuild; a rocm build on
    // a ROCm box sails past this check to the missing-file refusal.
    #[cfg(feature = "rocm")]
    assert!(error.to_string().contains("missing"), "{error:?}");
    #[cfg(not(feature = "rocm"))]
    assert!(error.to_string().contains("rocm"), "{error:?}");
}

#[test]
fn backend_reports_its_load_state() {
    let backend = MoonshineBackend::new("/nonexistent/moonshine.gguf");
    assert!(!backend.is_loaded(), "lazy until first decode");
}

/// M15-S1: the moonshine field surface. `describe()` is the schema
/// golden (gpu deliberately outside `required`); the table pins
/// accept and reject.
#[test]
fn moonshine_field_surface_pins_schema_and_table() {
    use fluent_wvr::{Describable, FieldAccess};
    let mut backend = MoonshineBackend::new("/nonexistent/moonshine.gguf");
    assert_eq!(
        backend.describe(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "model_path": { "type": "string", "description": "Moonshine model file" },
                "gpu": { "type": "boolean", "description": "GPU offload (explicit opt-in)" }
            },
            "required": ["model_path"]
        })
    );
    assert_eq!(backend.field_names(), &["model_path", "gpu"]);
    backend
        .set_field("model_path", "/m.gguf")
        .expect("path sets");
    assert_eq!(
        backend.get_field("model_path").expect("path reads"),
        "/m.gguf"
    );
    backend.set_field("gpu", "true").expect("bool sets");
    backend
        .set_field("gpu", "maybe")
        .expect_err("garbage bool rejects");
    backend.set_field("nope", "1").expect_err("unknown rejects");
    backend.get_field("nope").expect_err("unknown reads reject");
}
