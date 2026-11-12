//! Agglomerative clustering on cosine distance, plus smoothing.
//!
//! Thresholds arrive as configuration on every run (never cached per
//! meeting, never baked into a checkpoint): the same audio with a new
//! expected speaker count re-runs from the diarize step.

/// Diarization tunables. Shipped values stay inside the calibrated
/// band; the calibration corpus pins them, not this struct.
#[derive(Debug, Clone, PartialEq)]
/// Agglomerative clustering on cosine distance, plus smoothing.
/// 
/// Thresholds arrive as configuration on every run (never cached per
/// meeting, never baked into a checkpoint): the same audio with a new
/// expected speaker count re-runs from the diarize step.
/// Diarization tunables. Shipped values stay inside the calibrated
/// band; the calibration corpus pins them, not this struct.
pub struct DiarConfig {
    /// Cosine-distance merge threshold in `[0.0, 2.0]`.
    pub cluster_threshold: f32,
    /// Expected speakers when known (`None` means decide by threshold).
    pub expected_speakers: Option<usize>,
    /// Smoothing window in segments (majority vote each side).
    pub smooth_width: usize,
}

impl Default for DiarConfig {
    fn default() -> Self {
        Self {
            cluster_threshold: 0.35,
            expected_speakers: None,
            smooth_width: 1,
        }
    }
}

/// Cluster `vectors` agglomeratively: repeatedly merge the closest
/// pair under the threshold until none remains. Returns one label per
/// vector, compacted to `0..n`.
#[must_use]
/// Cluster `vectors` agglomeratively: repeatedly merge the closest
/// pair under the threshold until none remains. Returns one label per
/// vector, compacted to `0..n`.
pub fn cluster(vectors: &[Vec<f32>], config: &DiarConfig) -> Vec<usize> {
    let count = vectors.len();
    if count == 0 {
        return Vec::new();
    }
    let mut labels: Vec<usize> = (0..count).collect();
    loop {
        let mut best: Option<(f32, usize, usize)> = None;
        for first in 0..count {
            for second in (first + 1)..count {
                if labels[first] == labels[second] {
                    continue;
                }
                let distance = 1.0 - vector_math::cosine_similarity_f32(&vectors[first], &vectors[second]);
                if distance < config.cluster_threshold
                    && best.is_none_or(|(known, _, _)| distance < known)
                {
                    best = Some((distance, first, second));
                }
            }
        }
        let Some((_, first, second)) = best else {
            break;
        };
        let (keep, drop) = (labels[first].min(labels[second]), labels[first].max(labels[second]));
        for label in &mut labels {
            if *label == drop {
                *label = keep;
            }
        }
        if let Some(expected) = config.expected_speakers {
            let mut distinct = labels.clone();
            distinct.sort_unstable();
            distinct.dedup();
            if distinct.len() <= expected {
                break;
            }
        }
    }
    // Compact labels to 0..n.
    let mut distinct = labels.clone();
    distinct.sort_unstable();
    distinct.dedup();
    labels
        .iter()
        .map(|label| distinct.iter().position(|known| known == label).expect("known"))
        .collect()
}

/// Smooth `labels` by majority vote over `width` neighbors each side.
/// Breaks single-segment flicker without moving real turns.
#[must_use]
/// Smooth `labels` by majority vote over `width` neighbors each side.
/// Breaks single-segment flicker without moving real turns.
pub fn smooth(labels: &[usize], width: usize) -> Vec<usize> {
    if width == 0 || labels.is_empty() {
        return labels.to_vec();
    }
    labels
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let from = index.saturating_sub(width);
            let to = (index + width + 1).min(labels.len());
            let window = &labels[from..to];
            // Ties keep the original label: smoothing removes flicker,
            // never moves a real turn on a coin flip.
            let mut best = labels[index];
            let mut best_count = window.iter().filter(|label| **label == best).count();
            for candidate in window {
                let count = window.iter().filter(|label| *label == candidate).count();
                if count > best_count {
                    best_count = count;
                    best = *candidate;
                }
            }
            best
        })
        .collect()
}

/// Misattributed-time fraction on a labeled held-out set: the DER
/// numerator over segment counts (uniform windows, so counts stand in
/// for time). Single-speaker control must score near zero.
#[must_use]
/// Misattributed-time fraction on a labeled held-out set: the DER
/// numerator over segment counts (uniform windows, so counts stand in
/// for time). Single-speaker control must score near zero.
pub fn diarization_error(got: &[usize], want: &[usize]) -> f32 {
    if want.is_empty() {
        return 0.0;
    }
    // Labels are arbitrary ids: remap by majority overlap first.
    let mut mapping = std::collections::HashMap::new();
    let mut distinct: Vec<usize> = got.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    for candidate in distinct {
        let mut counts = std::collections::HashMap::new();
        for (index, label) in got.iter().enumerate() {
            if *label == candidate && index < want.len() {
                *counts.entry(want[index]).or_insert(0usize) += 1;
            }
        }
        if let Some((truth, _)) = counts.into_iter().max_by_key(|(_, count)| *count) {
            mapping.insert(candidate, truth);
        }
    }
    let errors = got
        .iter()
        .enumerate()
        .filter(|(index, label)| {
            index >= &want.len() || mapping.get(label) != want.get(*index)
        })
        .count();
    errors as f32 / want.len() as f32
}

#[cfg(test)]
#[path = "../../tests/diary_cluster.rs"]
mod tests;