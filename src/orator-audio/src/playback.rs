//! Blocking local playout: one output stream, drained before return.
//!
//! The CLI's speech path synthesizes a whole turn, then plays it here
//! instead of owning a second cpal output implementation. The
//! callback reads a shared sample buffer by index — no allocation,
//! no lock per sample.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::error::AudioError;

/// Open the default output device at `rate` Hz and start `render`
/// draining: one mono stream, one error line. Both playout shapes
/// share this opener; only their callbacks differ.
fn open_default_output(
    rate: u32,
    render: impl FnMut(&mut [f32], &cpal::OutputCallbackInfo) + Send + 'static,
) -> Result<cpal::Stream, AudioError> {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        return Err(AudioError::NoDevice(
            "no output device for local playout".to_string(),
        ));
    };
    let config = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let stream = device
        .build_output_stream(
            &config,
            render,
            |error| eprintln!("orator: playout error: {error}"),
            None,
        )
        .map_err(|error| {
            AudioError::Stream(format!("output stream failed: {error}"))
        })?;
    stream
        .play()
        .map_err(|error| AudioError::Stream(format!("playout failed: {error}")))?;
    Ok(stream)
}

/// Play `samples` (mono, `rate` Hz) on the default output device and
/// return once drained. Empty input is a no-op that touches no
/// device. Reports a missing device or a failed stream actionably.
pub fn play_blocking(samples: &[f32], rate: u32) -> Result<(), AudioError> {
    if samples.is_empty() {
        return Ok(());
    }
    let shared: Arc<Vec<f32>> = Arc::new(samples.to_vec());
    let cursor = Arc::new(AtomicUsize::new(0));
    let _stream = {
        let shared = Arc::clone(&shared);
        let cursor = Arc::clone(&cursor);
        open_default_output(
            rate,
            move |data: &mut [f32], _| {
                let at = cursor.load(Ordering::Relaxed);
                for (offset, sample) in data.iter_mut().enumerate() {
                    *sample = shared.get(at + offset).copied().unwrap_or(0.0);
                }
                cursor.store(at + data.len(), Ordering::Relaxed);
            },
        )?
    };
    // Exact drain: poll the callback cursor until the buffer is
    // consumed, bounded by samples/rate + 1 s. A stall fails closed
    // with the undrained count instead of reporting a truncated turn
    // as success.
    let ceiling = std::time::Duration::from_secs_f64(
        samples.len() as f64 / f64::from(rate.max(1)) + 1.0,
    );
    let deadline = std::time::Instant::now() + ceiling;
    while cursor.load(Ordering::Relaxed) < samples.len() {
        if std::time::Instant::now() >= deadline {
            return Err(AudioError::Stream(format!(
                "playout stalled with {} sample(s) undrained",
                samples.len().saturating_sub(cursor.load(Ordering::Relaxed))
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

/// Streaming playout: chunks are pushed as they synthesize while the
/// device drains ahead. Same output path as [`play_blocking`] (one
/// mono stream, one callback); the callback briefly locks once per
/// refill to move queued samples, never per sample. `push` is
/// non-blocking; `finish` drains exactly like `play_blocking` and
/// stops the stream. Dropping without `finish` abandons queued audio
/// and stops the device — finish every player on the success path.
pub struct StreamPlayer {
    shared: Arc<StreamShared>,
    rate: u32,
    // The live output stream: held for the session, dropped (stopped)
    // by `finish` or by an early drop.
    _stream: cpal::Stream,
}

struct StreamShared {
    queue: std::sync::Mutex<VecDeque<f32>>,
    pushed: AtomicUsize,
    played: AtomicUsize,
    dropped: AtomicUsize,
}

impl StreamPlayer {
    /// Realtime queue cap in samples (10 s at 48 kHz). Over-cap pushes
    /// drop the oldest with a counter — realtime audio stays bounded
    /// and lossy, never OOMs on a stalled device.
    pub const MAX_QUEUE_SAMPLES: usize = 480_000;
    /// Open the default output device at `rate` Hz and start draining.
    /// Reports a missing device or a failed stream actionably, before
    /// any synthesis cost is paid.
    pub fn open(rate: u32) -> Result<Self, AudioError> {
        let shared = Arc::new(StreamShared {
            queue: std::sync::Mutex::new(VecDeque::new()),
            pushed: AtomicUsize::new(0),
            played: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        });
        let stream = {
            let shared = Arc::clone(&shared);
            open_default_output(
                rate,
                move |data: &mut [f32], _| {
                    let mut queue = common_core::sync::lock(&shared.queue);
                    for sample in data.iter_mut() {
                        *sample = queue.pop_front().unwrap_or(0.0);
                    }
                    shared.played.fetch_add(data.len(), Ordering::Relaxed);
                },
            )?
        };
        Ok(Self {
            shared,
            rate,
            _stream: stream,
        })
    }

    /// Queue `samples` for playout. Empty input is a no-op. Returns
    /// immediately; the device drains ahead on its own thread.
    /// Over-cap input drops the oldest queued samples first (realtime
    /// lossy) and counts them in [`dropped`](Self::dropped).
    pub fn push(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut queue = common_core::sync::lock(&self.shared.queue);
        queue.extend(samples.iter().copied());
        self.shared.pushed.fetch_add(samples.len(), Ordering::Relaxed);
        let excess = queue.len().saturating_sub(Self::MAX_QUEUE_SAMPLES);
        if excess > 0 {
            queue.drain(0..excess);
            self.shared.dropped.fetch_add(excess, Ordering::Relaxed);
        }
    }

    /// Samples dropped by over-cap pushes so far.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Samples queued so far.
    #[must_use]
    pub fn pushed(&self) -> usize {
        self.shared.pushed.load(Ordering::Relaxed)
    }

    /// Samples the callback has consumed so far.
    #[must_use]
    pub fn played(&self) -> usize {
        self.shared.played.load(Ordering::Relaxed)
    }

    /// Stop queueing, drain exactly like [`play_blocking`] (remaining
    /// samples at rate plus a 1 s ceiling), then stop the stream.
    /// A stalled device fails closed instead of reporting success.
    pub fn finish(self) -> Result<(), AudioError> {
        let remaining = self.pushed().saturating_sub(self.played()).saturating_sub(self.dropped());
        let ceiling = std::time::Duration::from_secs_f64(
            remaining as f64 / f64::from(self.rate.max(1)) + 1.0,
        );
        let deadline = std::time::Instant::now() + ceiling;
        while self.played() < self.pushed().saturating_sub(self.dropped()) {
            if std::time::Instant::now() >= deadline {
                return Err(AudioError::Stream(format!(
                    "playout stalled with {} sample(s) undrained",
                    self.pushed().saturating_sub(self.played())
                )));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/playback.rs"]
mod tests;
