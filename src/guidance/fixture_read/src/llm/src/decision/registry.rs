//! Decision registry: precision and control fires gate promotion.

/// fluent-llm decision registry.
pub struct Registry {
    precision: f64,
}

/// Zero control fires or the point stays measuring.
pub fn control_fires() -> usize {
    0
}
