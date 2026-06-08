use std::process::Command;

pub fn run_command(argv: &[&str]) -> bool {
    if argv.is_empty() {
        return false;
    }
    Command::new(argv[0])
        .args(&argv[1..])
        .status()
        .is_ok_and(|s| s.success())
}

pub fn add_unique_path(
    list: &mut Vec<String>,
    path: &str,
    project_root: Option<&str>,
) -> bool {
    if list.iter().any(|p| p == path) {
        return false;
    }
    if let Some(root) = project_root {
        if !root.is_empty() {
            let full_path = if root.ends_with('/') {
                format!("{root}{path}")
            } else {
                format!("{root}/{path}")
            };
            if !std::path::Path::new(&full_path).exists() {
                return false;
            }
        }
    }
    list.push(path.to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_true() {
        assert!(run_command(&["true"]));
    }

    #[test]
    fn run_command_false() {
        assert!(!run_command(&["false"]));
    }

    #[test]
    fn add_unique_path_deduplicates() {
        let mut list = Vec::new();
        assert!(add_unique_path(&mut list, "path1", None));
        assert!(!add_unique_path(&mut list, "path1", None));
        assert!(add_unique_path(&mut list, "path2", None));
        assert_eq!(list.len(), 2);
    }
}
