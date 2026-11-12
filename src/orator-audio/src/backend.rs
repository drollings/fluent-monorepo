//! Capture and playout backends.
//!
//! The real-time callback touches only the lock-free ring: it converts the
//! device sample format to f32 and pushes. Everything else — timestamps,
//! resampling, archiving — happens on the draining task. The callback never
//! allocates, locks, or awaits.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::error::AudioError;

/// A named input or output device.
#[derive(Debug, Clone, PartialEq, Eq)]
/// A named input or output device.
pub struct DeviceInfo {
    /// Backend-reported device name.
    pub name: String,
    /// Whether this device can capture.
    pub can_capture: bool,
}

/// List input devices. Never fails: headless machines simply yield an
/// empty list, and capture falls back to synthetic sources.
#[must_use]
/// List input devices. Never fails: headless machines simply yield an
/// empty list, and capture falls back to synthetic sources.
pub fn list_input_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let mut devices = Vec::new();
    if let Some(default) = host.default_input_device() {
        let name = default.name().unwrap_or_else(|_| "default".to_string());
        devices.push(DeviceInfo {
            name,
            can_capture: true,
        });
    }
    if let Ok(all) = host.input_devices() {
        for device in all {
            if let Ok(name) = device.name() {
                if !devices.iter().any(|info| info.name == name) {
                    devices.push(DeviceInfo {
                        name,
                        can_capture: true,
                    });
                }
            }
        }
    }
    devices
}

/// A live cpal input stream feeding a lock-free sample ring.
///
/// The owning task drains `consumer`, stamps media time, and forwards
/// frames. Dropping the backend stops the stream.
pub struct CpalBackend {
    /// Kept alive for the stream's lifetime.
    _stream: cpal::Stream,
    /// Drain end of the sample ring.
    pub consumer: Consumer<f32>,
    /// Device sample rate in Hz.
    pub sample_rate: u32,
    /// Ring capacity in samples.
    pub capacity: usize,
    /// Ring-overflow drops since open (RT callback increments).
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl CpalBackend {
    /// Ring capacity: two seconds of 48 kHz mono. Bounded, never grows.
    pub const RING_SAMPLES: usize = 96_000;

    /// Open the default input device and start capturing into a fresh ring.
    /// Fails ordinarily (not fatally) when no device exists.
    pub fn open_default() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioError::NoDevice("no default input device".to_string()))?;
        Self::open_device(&device)
    }

    /// Open `device` by name. Fails when the name matches nothing.
    pub fn open_named(name: &str) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let devices = host
            .input_devices()
            .map_err(|error| AudioError::NoDevice(format!("cannot enumerate devices: {error}")))?;
        for device in devices {
            if device.name().as_deref() == Ok(name) {
                return Self::open_device(&device);
            }
        }
        Err(AudioError::NoDevice(format!("unknown device: {name}")))
    }

    fn open_device(device: &cpal::Device) -> Result<Self, AudioError> {
        let config = device
            .default_input_config()
            .map_err(|error| AudioError::Stream(format!("no supported input config: {error}")))?;
        let sample_rate = config.sample_rate().0;
        let (producer, consumer) = RingBuffer::new(Self::RING_SAMPLES);
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let dropped_counter = || std::sync::Arc::clone(&dropped);
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => Self::build_stream(
                device,
                &config.into(),
                producer,
                dropped_counter(),
                |sample| sample,
            ),
            cpal::SampleFormat::I16 => Self::build_stream(
                device,
                &config.into(),
                producer,
                dropped_counter(),
                |sample: i16| {
                    common_core::pcm::i16_to_f32_sample(
                        sample,
                        common_core::pcm::Scale16::Symmetric32767,
                    )
                },
            ),
            cpal::SampleFormat::U16 => Self::build_stream(
                device,
                &config.into(),
                producer,
                dropped_counter(),
                |sample: u16| (f32::from(sample) / f32::from(u16::MAX)).mul_add(2.0, -1.0),
            ),
            format => {
                return Err(AudioError::Stream(format!(
                    "unsupported sample format: {format:?}"
                )));
            }
        }?;
        stream
            .play()
            .map_err(|error| AudioError::Stream(format!("cannot start stream: {error}")))?;
        Ok(Self {
            _stream: stream,
            consumer,
            sample_rate,
            capacity: Self::RING_SAMPLES,
            dropped,
        })
    }

    /// Ring-overflow drops since open. The RT callback drops instead of
    /// blocking; this counter surfaces the loss to the draining task.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Build the stream around `convert`. The callback pushes converted
    /// samples and drops overflows: the RT thread never blocks, and every
    /// drop is observable as a ring gap downstream.
    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        mut producer: Producer<f32>,
        dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
        convert: fn(T) -> f32,
    ) -> Result<cpal::Stream, AudioError>
    where
        T: cpal::SizedSample + Send + 'static,
    {
        let channels = config.channels as usize;
        let mut mono = Vec::with_capacity(1024);
        device
            .build_input_stream(
                config,
                move |samples: &[T], _| {
                    mono.clear();
                    let remainder = Self::collapse_to_mono(samples, convert, channels, &mut mono);
                    let mut lost = remainder as u64;
                    for sample in mono.drain(..) {
                        if producer.push(sample).is_err() {
                            lost += 1;
                        }
                    }
                    if lost > 0 {
                        dropped.fetch_add(lost, std::sync::atomic::Ordering::SeqCst);
                    }
                },
                |error| {
                    tracing::warn!(target: "orator.audio", %error, "capture stream error");
                },
                None,
            )
            .map_err(|error| AudioError::Stream(format!("build failed: {error}")))
    }

    /// Collapse one interleaved device callback to mono, appending
    /// frames into `out`. Multi-channel frames average (a stereo mic
    /// reads as mono, never double-speed); single-channel input
    /// passes through untouched. Returns the trailing partial-frame
    /// sample count, which the caller counts as dropped — a partial
    /// frame is not audio, just a torn read. The averaging composes
    /// `common_core::pcm::extend_mono_from_interleaved`, so the
    /// real-time callback keeps its reused `mono` vector and never
    /// locks or awaits.
    fn collapse_to_mono<T: Copy>(
        samples: &[T],
        convert: fn(T) -> f32,
        channels: usize,
        out: &mut Vec<f32>,
    ) -> usize {
        common_core::pcm::extend_mono_from_interleaved(
            samples.iter().copied(),
            channels,
            convert,
            out,
        )
    }
}

#[cfg(test)]
#[path = "../tests/backend.rs"]
mod tests;