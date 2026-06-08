use internment::ArcIntern;
use std::sync::Arc;

pub const LOD_COUNT: usize = crate::types::LOD_COUNT;

#[derive(Debug, Clone)]
pub struct ContentNode {
    pub source: ArcIntern<str>,
    pub lod: [Option<ArcIntern<str>>; LOD_COUNT],
}

impl ContentNode {
    pub fn new(full_text: impl AsRef<str>) -> Self {
        let source: ArcIntern<str> = ArcIntern::from(full_text.as_ref());
        let mut lod: [Option<ArcIntern<str>>; LOD_COUNT] = Default::default();
        lod[0] = Some(source.clone());
        Self { source, lod }
    }

    pub fn get_lod(&self, level: usize) -> &str {
        self.lod
            .get(level)
            .and_then(|opt| opt.as_deref())
            .unwrap_or("")
    }

    pub fn set_lod(&mut self, level: usize, value: impl AsRef<str>) {
        if level == 0 {
            return;
        }
        if let Some(slot) = self.lod.get_mut(level) {
            *slot = Some(ArcIntern::from(value.as_ref()));
        }
    }

    pub fn set_source(&mut self, text: impl AsRef<str>) {
        let new_source: ArcIntern<str> = ArcIntern::from(text.as_ref());
        self.lod[0] = Some(new_source.clone());
        self.source = new_source;
    }
}

/// Generate LOD (Level of Detail) slices for the LOD pyramid.
/// LOD levels: 0=full, 1=summary(800), 2=brief(240), 3=tiny(80), 4=name, 5=reserved.
pub fn generate_lod_slices(full_text: &str) -> Vec<String> {
    let targets = [usize::MAX, 800, 240, 80, 40, usize::MAX];
    let mut slices = Vec::with_capacity(targets.len());
    for &max_chars in &targets {
        if max_chars == usize::MAX || full_text.len() <= max_chars {
            slices.push(full_text.to_string());
        } else {
            let truncated = &full_text[..max_chars];
            if let Some(last_period) = truncated.rfind('.') {
                if last_period > max_chars / 2 {
                    slices.push(full_text[..=last_period].to_string());
                    continue;
                }
            }
            if let Some(last_space) = truncated.rfind(' ') {
                slices.push(full_text[..last_space].to_string());
            } else {
                slices.push(truncated.to_string());
            }
        }
    }
    slices
}

pub struct RefCounted<T> {
    inner: Arc<std::sync::Mutex<T>>,
}

impl<T> RefCounted<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(value)),
        }
    }

    pub fn value(&self) -> std::sync::MutexGuard<'_, T> {
        self.inner.lock().unwrap()
    }

    pub fn ref_count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }
}

impl<T> Clone for RefCounted<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_and_free() {
        let node = ContentNode::new("full text content");
        assert_eq!(node.get_lod(0), "full text content");
    }

    #[test]
    fn get_lod_returns_empty_for_unset() {
        let node = ContentNode::new("full text");
        assert_eq!(node.get_lod(1), "");
        assert_eq!(node.get_lod(99), "");
    }

    #[test]
    fn clone_shares_source() {
        let a = ContentNode::new("shared text");
        let b = a.clone();
        assert_eq!(a.source, b.source);
    }

    #[test]
    fn set_lod_and_get_lod() {
        let mut node = ContentNode::new("original");
        node.set_lod(2, "summary");
        assert_eq!(node.get_lod(2), "summary");
    }

    #[test]
    fn set_lod_0_is_noop() {
        let mut node = ContentNode::new("original");
        node.set_lod(0, "should not change");
        assert_eq!(node.get_lod(0), "original");
    }

    #[test]
    fn set_source() {
        let mut node = ContentNode::new("old");
        node.set_source("new");
        assert_eq!(&*node.source, "new");
    }

    #[test]
    fn ref_counted_basic() {
        let r = RefCounted::new(42);
        assert_eq!(*r.value(), 42);
    }

    #[test]
    fn ref_counted_clone_increments() {
        let r = RefCounted::new(1);
        let r2 = r.clone();
        assert_eq!(r.ref_count(), 2);
        drop(r2);
        assert_eq!(r.ref_count(), 1);
    }

    #[test]
    fn ref_counted_value_mutation() {
        let r = RefCounted::new(10);
        *r.value() = 20;
        assert_eq!(*r.value(), 20);
    }

    #[test]
    fn ref_counted_shared_value() {
        let r = RefCounted::new(10);
        let r2 = r.clone();
        *r2.value() = 99;
        assert_eq!(*r.value(), 99);
    }

    #[test]
    fn generate_lod_slices_short_text() {
        let slices = generate_lod_slices("Hello");
        assert_eq!(slices.len(), 6);
        for s in &slices {
            assert_eq!(s, "Hello");
        }
    }

    #[test]
    fn generate_lod_slices_truncates_summary() {
        let long = "A. ".repeat(500);
        let text = &long[..long.len() - 2];
        let slices = generate_lod_slices(text);
        assert!(slices[0].len() > 800);
        assert!(slices[1].len() <= 800);
        assert!(slices[2].len() <= 240);
        assert!(slices[3].len() <= 80);
        assert!(slices[4].len() <= 40);
    }

    #[test]
    fn generate_lod_slices_sentence_boundary() {
        let text = "First sentence. Second sentence that is longer. Third sentence.";
        let slices = generate_lod_slices(text);
        assert!(slices[1].ends_with('.'));
    }
}
