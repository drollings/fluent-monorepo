use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct FrozenSnapshot {
    pub memory: String,
    pub skills: String,
    pub context_files: String,
}

impl FrozenSnapshot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load(paths: &[&Path]) -> Self {
        let context_files = read_and_join(paths);
        Self {
            memory: String::new(),
            skills: String::new(),
            context_files,
        }
    }

    pub fn load_sections(
        memory_paths: &[&Path],
        skill_paths: &[&Path],
        context_paths: &[&Path],
    ) -> Self {
        Self {
            memory: read_and_join(memory_paths),
            skills: read_and_join(skill_paths),
            context_files: read_and_join(context_paths),
        }
    }

    pub fn format_for_system_prompt(&self) -> String {
        let mut parts = Vec::new();
        if !self.memory.is_empty() {
            parts.push(format!("## Memory\n{}", self.memory));
        }
        if !self.skills.is_empty() {
            parts.push(format!("## Skills\n{}", self.skills));
        }
        if !self.context_files.is_empty() {
            parts.push(format!("## Context\n{}", self.context_files));
        }
        parts.join("\n\n")
    }
}

fn read_and_join(paths: &[&Path]) -> String {
    let contents: Vec<String> = paths
        .iter()
        .filter_map(|p| fs::read_to_string(p).ok())
        .collect();
    contents.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_wvr_testutil::tempdir;

    #[test]
    fn init_produces_empty_snapshot() {
        let snap = FrozenSnapshot::new();
        assert!(snap.memory.is_empty());
        assert!(snap.skills.is_empty());
        assert!(snap.context_files.is_empty());
    }

    #[test]
    fn format_for_system_prompt_empty() {
        let snap = FrozenSnapshot::new();
        assert!(snap.format_for_system_prompt().is_empty());
    }

    #[test]
    fn format_for_system_prompt_includes_headers() {
        let snap = FrozenSnapshot {
            memory: "foo".into(),
            skills: String::new(),
            context_files: String::new(),
        };
        let prompt = snap.format_for_system_prompt();
        assert!(prompt.contains("## Memory"));
        assert!(!prompt.contains("## Skills"));
    }

    #[test]
    fn load_reads_file_content() {
        let dir = tempdir();
        let p = dir.path().join("test.txt");
        fs::write(&p, "hello").unwrap();
        let snap = FrozenSnapshot::load(&[&p]);
        assert_eq!(snap.context_files, "hello");
    }

    #[test]
    fn load_skips_missing_files() {
        let snap = FrozenSnapshot::load(&[Path::new("/nonexistent/file.txt")]);
        assert!(snap.context_files.is_empty());
    }

    #[test]
    fn load_concatenates_multiple_files() {
        let dir = tempdir();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        let snap = FrozenSnapshot::load(&[&a, &b]);
        assert!(snap.context_files.contains("a"));
        assert!(snap.context_files.contains("b"));
    }
}
