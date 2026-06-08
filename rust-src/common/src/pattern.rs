use crate::string::{contains_any, contains_any_word, contains_ignore_case, contains_word};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternType {
    Domain,
    GoF,
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub name: &'static str,
    pub pattern_type: PatternType,
    pub r#ref: Option<&'static str>,
}

pub fn detect_ring_buffer(source: &str) -> bool {
    contains_any(source, &["ring", "ringbuffer", "circular", "fifo", "deque"])
}

pub fn detect_state_persistence(source: &str) -> bool {
    contains_ignore_case(source, "self.state")
        || contains_ignore_case(source, ".state =")
        || contains_ignore_case(source, "state: State")
        || contains_ignore_case(source, "state: enum")
}

pub fn detect_factory(source: &str) -> bool {
    contains_ignore_case(source, "factory")
        || contains_ignore_case(source, "fn create")
        || contains_ignore_case(source, "fn make")
        || contains_ignore_case(source, "pub fn create")
        || contains_ignore_case(source, "pub fn make")
}

pub fn detect_singleton(source: &str) -> bool {
    contains_ignore_case(source, "_instance")
        || contains_ignore_case(source, "getInstance")
        || contains_ignore_case(source, "get_instance")
        || (contains_word(source, "instance")
            && contains_ignore_case(source, "fn instance("))
}

pub fn detect_builder(source: &str) -> bool {
    (contains_ignore_case(source, "builder") && contains_ignore_case(source, "fn build("))
        || (count_occurrences(source, "return self;") >= 2
            && contains_ignore_case(source, "fn build("))
}

pub fn detect_adapter(source: &str) -> bool {
    contains_ignore_case(source, "fn adapt")
        || contains_ignore_case(source, "fn convert")
        || contains_ignore_case(source, "fn transform")
        || contains_ignore_case(source, "fn to_")
        || contains_ignore_case(source, "fn as_")
}

pub fn detect_observer(source: &str) -> bool {
    contains_any_word(
        source,
        &["observer", "subscriber", "listener", "publisher", "event_bus", "eventbus"],
    )
}

fn count_occurrences(s: &str, needle: &str) -> usize {
    s.matches(needle).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_ring_buffer_positive() {
        assert!(detect_ring_buffer("using a ring buffer for the queue"));
        assert!(detect_ring_buffer("circular buffer implementation"));
    }

    #[test]
    fn detect_ring_buffer_negative() {
        assert!(!detect_ring_buffer("just a regular vec"));
    }

    #[test]
    fn detect_factory_positive() {
        assert!(detect_factory("pub fn create_widget()"));
    }

    #[test]
    fn detect_factory_negative() {
        assert!(!detect_factory("fn process()"));
    }

    #[test]
    fn detect_observer_positive() {
        assert!(detect_observer("observer pattern via events"));
    }

    #[test]
    fn detect_observer_negative() {
        assert!(!detect_observer("just processing data"));
    }
}
