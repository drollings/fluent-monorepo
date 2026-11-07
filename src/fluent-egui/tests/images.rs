use super::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn png_bytes(payload_len: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend(std::iter::repeat_n(0xABu8, payload_len));
    bytes
}

fn fetch_fn(
    fetch: impl Fn(String) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ImageError>> + Send>>
        + Send
        + Sync
        + 'static,
) -> ImageFetch {
    let fetch: ImageFetch = Arc::new(fetch);
    fetch
}

fn test_loader(fetch: ImageFetch, attempts: u32, max_bytes: usize) -> ImageLoader {
    ImageLoader::new(
        2,
        16,
        fetch,
        TransportRetry::new(attempts, 1, 0).expect("valid policy"),
        max_bytes,
    )
    .expect("valid loader")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failure_corpus_always_takes_the_error_path() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&fetches);
    let loader = test_loader(
        fetch_fn(move |url: String| {
            let probe = Arc::clone(&probe);
            Box::pin(async move {
                probe.fetch_add(1, Ordering::SeqCst);
                match url.as_str() {
                    "https://cdn.test/ok.png" => Ok(png_bytes(64)),
                    "https://cdn.test/huge.png" => Ok(png_bytes(1024 * 1024)),
                    _ => Ok(b"not an image at all".to_vec()),
                }
            })
        }),
        3,
        512,
    );
    let bad = loader.load("notaurl").await.expect_err("bad URLs refuse");
    assert!(
        matches!(bad, ImageError::Permanent(_)),
        "a bad URL never retries: {bad:?}"
    );
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        0,
        "a bad URL never reaches the transport"
    );
    let text = loader
        .load("https://cdn.test/text")
        .await
        .expect_err("non-image bytes refuse");
    assert!(
        matches!(text, ImageError::Permanent(_)),
        "non-image bytes refuse: {text:?}"
    );
    let huge = loader
        .load("https://cdn.test/huge.png")
        .await
        .expect_err("oversize payloads refuse");
    assert!(
        matches!(huge, ImageError::Permanent(_)),
        "oversize payloads refuse: {huge:?}"
    );
    let ok = loader
        .load("https://cdn.test/ok.png")
        .await
        .expect("valid bytes deliver");
    assert_eq!(ok, png_bytes(64), "valid bytes pass through intact");
}

#[test]
fn validate_image_bytes_names_each_refusal() {
    assert!(validate_image_bytes(&png_bytes(8), 512).is_ok());
    assert!(validate_image_bytes(&[], 512).is_err(), "empty refuses");
    assert!(
        validate_image_bytes(&png_bytes(513), 512).is_err(),
        "oversize refuses"
    );
    assert!(
        validate_image_bytes(b"hello", 512).is_err(),
        "unknown magic refuses"
    );
    assert!(
        validate_image_bytes(&[0xFF, 0xD8, 0xFF, 0x00], 512).is_ok(),
        "jpeg magic passes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn burst_loads_stay_within_the_pool_bound() {
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let probe_current = Arc::clone(&current);
    let probe_peak = Arc::clone(&peak);
    let loader = test_loader(
        fetch_fn(move |_: String| {
            let probe_current = Arc::clone(&probe_current);
            let probe_peak = Arc::clone(&probe_peak);
            Box::pin(async move {
                let held = probe_current.fetch_add(1, Ordering::SeqCst) + 1;
                probe_peak.fetch_max(held, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(30)).await;
                probe_current.fetch_sub(1, Ordering::SeqCst);
                Ok(png_bytes(16))
            })
        }),
        1,
        512,
    );
    let loader = Arc::new(loader);
    let mut tasks = Vec::new();
    for index in 0..10u32 {
        let loader = Arc::clone(&loader);
        tasks.push(tokio::spawn(async move {
            loader.load(&format!("https://cdn.test/{index}.png")).await
        }));
    }
    for task in tasks {
        task.await.expect("join succeeds").expect("loads deliver");
    }
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "ten burst loads never exceed the pool bound"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decode_happens_at_most_once_per_url() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&fetches);
    let loader = test_loader(
        fetch_fn(move |_: String| {
            let probe = Arc::clone(&probe);
            Box::pin(async move {
                probe.fetch_add(1, Ordering::SeqCst);
                Ok(png_bytes(32))
            })
        }),
        1,
        512,
    );
    for _ in 0..3 {
        let bytes = loader
            .load("https://cdn.test/logo.png")
            .await
            .expect("loads deliver");
        assert_eq!(bytes, png_bytes(32));
    }
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        1,
        "sequential loads decode once"
    );
    let loader = Arc::new(loader);
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let loader = Arc::clone(&loader);
        tasks.push(tokio::spawn(async move {
            loader.load("https://cdn.test/burst.png").await
        }));
    }
    for task in tasks {
        task.await.expect("join succeeds").expect("loads deliver");
    }
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        2,
        "concurrent loads decode once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_calibration_transients_retry_permanents_do_not() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&attempts);
    let flaky = test_loader(
        fetch_fn(move |_: String| {
            let probe = Arc::clone(&probe);
            Box::pin(async move {
                let n = probe.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 3 {
                    Err(ImageError::Transient("blip".to_owned()))
                } else {
                    Ok(png_bytes(8))
                }
            })
        }),
        3,
        512,
    );
    flaky
        .load("https://cdn.test/flaky.png")
        .await
        .expect("transients recover");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "transients retry to success"
    );
    let denials = Arc::new(AtomicUsize::new(0));
    let probe = Arc::clone(&denials);
    let hard = test_loader(
        fetch_fn(move |_: String| {
            let probe = Arc::clone(&probe);
            Box::pin(async move {
                probe.fetch_add(1, Ordering::SeqCst);
                Err::<Vec<u8>, ImageError>(ImageError::Permanent("gone".to_owned()))
            })
        }),
        3,
        512,
    );
    assert!(
        hard.load("https://cdn.test/gone.png").await.is_err(),
        "permanents surface"
    );
    assert_eq!(denials.load(Ordering::SeqCst), 1, "permanents never retry");
}

#[test]
fn placeholder_geometry_is_exact() {
    let rect = placeholder_rect((10.0, 20.0), 100.0, 50.0);
    assert_eq!(
        rect,
        Rect::new(Pos2::new(10.0, 20.0), Pos2::new(110.0, 70.0)),
        "the placeholder spans exactly the image box"
    );
    let degenerate = placeholder_rect((10.0, 20.0), f32::NAN, -5.0);
    assert_eq!(
        degenerate,
        Rect::new(Pos2::new(10.0, 20.0), Pos2::new(10.0, 20.0)),
        "degenerate sizes collapse to the origin"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn net_fetch_refuses_bad_urls_without_touching_the_network() {
    use fluent_concurrency::capability::default_capability_set;
    use fluent_concurrency::io::net::NetCapability;
    let net = NetCapability::new();
    let error = net_fetch(default_capability_set(), &net, "notaurl".to_owned())
        .await
        .expect_err("bad URLs refuse");
    assert!(
        matches!(error, ImageError::Permanent(_)),
        "a bad URL never retries: {error:?}"
    );
}
