pub const LOD_COUNT: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetId(pub i64);

impl NodeId {
    pub fn from_int(i: i64) -> Self {
        Self(i)
    }
    pub fn as_int(self) -> i64 {
        self.0
    }
}

impl SessionId {
    pub fn from_int(i: i64) -> Self {
        Self(i)
    }
    pub fn as_int(self) -> i64 {
        self.0
    }
}

impl TargetId {
    pub fn from_int(i: i64) -> Self {
        Self(i)
    }
    pub fn as_int(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_roundtrip() {
        let id = NodeId::from_int(42);
        assert_eq!(id.as_int(), 42);
    }

    #[test]
    fn session_id_roundtrip() {
        let id = SessionId::from_int(-1);
        assert_eq!(id.as_int(), -1);
    }

    #[test]
    fn target_id_roundtrip() {
        let id = TargetId::from_int(0);
        assert_eq!(id.as_int(), 0);
    }

    #[test]
    fn types_are_distinct() {
        fn takes_node(_: NodeId) {}
        fn takes_session(_: SessionId) {}
        fn takes_target(_: TargetId) {}
        takes_node(NodeId::from_int(1));
        takes_session(SessionId::from_int(1));
        takes_target(TargetId::from_int(1));
    }
}
