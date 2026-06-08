#[derive(Debug, Clone)]
pub struct QuantizedEmbedding {
    pub values: Vec<i8>,
    pub scale: f32,
    pub dimensions: usize,
}

impl QuantizedEmbedding {
    pub fn from_f32(vec: &[f32]) -> Self {
        if vec.is_empty() {
            return Self {
                values: Vec::new(),
                scale: 1.0,
                dimensions: 0,
            };
        }

        let max_abs = vec
            .iter()
            .map(|v| v.abs())
            .fold(0.0_f32, f32::max);

        let scale = if max_abs > 0.0 {
            127.0 / max_abs
        } else {
            1.0
        };

        let values: Vec<i8> = vec
            .iter()
            .map(|v| (v * scale).round().clamp(-128.0, 127.0) as i8)
            .collect();

        Self {
            dimensions: vec.len(),
            values,
            scale,
        }
    }

    pub fn to_f32(&self) -> Vec<f32> {
        if self.scale == 0.0 {
            return vec![0.0; self.dimensions];
        }
        let inv_scale = 1.0 / self.scale;
        self.values
            .iter()
            .map(|&v| (v as f32) * inv_scale)
            .collect()
    }
}

pub fn cosine_similarity_q8(a: &QuantizedEmbedding, b: &QuantizedEmbedding) -> f32 {
    if a.dimensions != b.dimensions || a.dimensions == 0 {
        return 0.0;
    }

    let mut dot_product: i64 = 0;
    let mut norm_a: i64 = 0;
    let mut norm_b: i64 = 0;

    for (x, y) in a.values.iter().zip(b.values.iter()) {
        let xi = *x as i64;
        let yi = *y as i64;
        dot_product += xi * yi;
        norm_a += xi * xi;
        norm_b += yi * yi;
    }

    let magnitude = ((norm_a as f64) * (norm_b as f64)).sqrt();
    if magnitude == 0.0 {
        return 0.0;
    }

    (dot_product as f64 / magnitude) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantize_round_trip() {
        let original = vec![0.5, -0.3, 0.8, -0.1, 0.0, 1.0, -1.0];
        let q = QuantizedEmbedding::from_f32(&original);
        let restored = q.to_f32();

        assert_eq!(original.len(), restored.len());
        for (a, b) in original.iter().zip(restored.iter()) {
            let diff = (a - b).abs();
            assert!(diff < 0.02, "difference {diff} too large for {a} vs {b}");
        }
    }

    #[test]
    fn test_empty_quantize() {
        let q = QuantizedEmbedding::from_f32(&[]);
        assert!(q.values.is_empty());
        assert_eq!(q.dimensions, 0);
    }

    #[test]
    fn test_q8_cosine_similarity() {
        let a = QuantizedEmbedding::from_f32(&[1.0, 0.0, 0.0]);
        let b = QuantizedEmbedding::from_f32(&[1.0, 0.0, 0.0]);
        let sim = cosine_similarity_q8(&a, &b);
        assert!((sim - 1.0).abs() < 0.02);
    }

    #[test]
    fn test_q8_cosine_orthogonal() {
        let a = QuantizedEmbedding::from_f32(&[1.0, 0.0]);
        let b = QuantizedEmbedding::from_f32(&[0.0, 1.0]);
        let sim = cosine_similarity_q8(&a, &b);
        assert!((sim - 0.0).abs() < 0.02);
    }

    #[test]
    fn test_q8_cosine_empty() {
        let a = QuantizedEmbedding::from_f32(&[]);
        let b = QuantizedEmbedding::from_f32(&[]);
        assert_eq!(cosine_similarity_q8(&a, &b), 0.0);
    }

    #[test]
    fn test_scale_factor() {
        let v = vec![2.0, -2.0, 1.0, -1.0];
        let q = QuantizedEmbedding::from_f32(&v);
        assert!(q.scale > 0.0);
        assert_eq!(q.values.len(), 4);
    }
}
