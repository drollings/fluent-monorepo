pub use guidance_common::registry::Target;
pub use guidance_common::types::{ExecutorKind, TargetType};

#[cfg(test)]
mod tests {
    use super::*;
    use bitvec::prelude::*;

    #[test]
    fn test_target_creation_and_clone() {
        let t = Target::new()
            .id(1)
            .name("build".into())
            .target_type(TargetType::File)
            .executor(ExecutorKind::Native)
            .depends(bitvec![0, 1])
            .provides(bitvec![1, 0])
            .command("zig build".into())
            .essential(true)
            .build();

        let t2 = t.clone();
        assert_eq!(t.id, t2.id);
        assert_eq!(t.name, t2.name);
        assert_eq!(t.target_type, t2.target_type);
        assert_eq!(t.essential, t2.essential);
    }
}
