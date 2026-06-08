use internment::ArcIntern;
use std::sync::Arc;

pub const LOD_COUNT: usize = 6;

#[derive(Debug, Clone)]
pub struct ContentNode {
    pub source: ArcIntern<String>,
    pub lod: [Option<ArcIntern<String>>; LOD_COUNT],
}

impl ContentNode {
    pub fn new(full_text: impl Into<String>) -> Self {
        let source = ArcIntern::new(full_text.into());
        let mut lod: [Option<ArcIntern<String>>; LOD_COUNT] = Default::default();
        lod[0] = Some(source.clone());
        Self { source, lod }
    }

    pub fn get_lod(&self, level: usize) -> Option<&str> {
        self.lod.get(level)?.as_deref().map(|s| s.as_str())
    }

    pub fn set_lod(&mut self, level: usize, value: impl Into<String>) {
        if level == 0 {
            return;
        }
        if let Some(slot) = self.lod.get_mut(level) {
            *slot = Some(ArcIntern::new(value.into()));
        }
    }

    pub fn set_source(&mut self, text: impl Into<String>) {
        let new_source = ArcIntern::new(text.into());
        self.lod[0] = Some(new_source.clone());
        self.source = new_source;
    }
}

use std::sync::atomic::{AtomicUsize, Ordering};

pub struct RefCounted<T> {
    inner: Arc<RefCountInner<T>>,
}

struct RefCountInner<T> {
    count: AtomicUsize,
    value: std::sync::Mutex<T>,
}

impl<T> RefCounted<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(RefCountInner {
                count: AtomicUsize::new(1),
                value: std::sync::Mutex::new(value),
            }),
        }
    }

    pub fn clone_ref(&self) -> Self {
        self.inner.count.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn ref_count(&self) -> usize {
        self.inner.count.load(Ordering::Relaxed)
    }

    pub fn value(&self) -> std::sync::MutexGuard<'_, T> {
        self.inner.value.lock().unwrap()
    }
}

impl<T> Clone for RefCounted<T> {
    fn clone(&self) -> Self {
        self.clone_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_and_free() {
        let node = ContentNode::new("full text content");
        assert_eq!(node.get_lod(0), Some("full text content"));
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
        assert_eq!(node.get_lod(2), Some("summary"));
    }

    #[test]
    fn set_source() {
        let mut node = ContentNode::new("old");
        node.set_source("new");
        assert_eq!(node.source.as_str(), "new");
    }

    #[test]
    fn ref_counted_basic() {
        let r = RefCounted::new(42);
        assert_eq!(*r.value(), 42);
    }

    #[test]
    fn ref_counted_clone_increments() {
        let r = RefCounted::new(1);
        let _r2 = r.clone_ref();
        assert_eq!(r.ref_count(), 2);
    }

    #[test]
    fn ref_counted_value_mutation() {
        let r = RefCounted::new(10);
        *r.value() = 20;
        assert_eq!(*r.value(), 20);
    }
}
