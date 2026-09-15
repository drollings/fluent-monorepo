use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fluent_types::{FileType, GuidanceDoc};
use guidance_core::sync::json_store::load_guidance;
use ptree::{PrintConfig, TreeBuilder};

const HEADER: &str = r#"# AST-Guidance Project Structure

A fast, lightweight code navigation and orchestration framework friendly to
human and human-in-the-loop LLM agentic software engineering.  It is based
on enriched AST, and uses optional AI for documentation which is cached,
idempotent, and upcycled for lightweight searches and local agentic
intelligence.

## Quick Navigation (Coding Assistants)

| Purpose | File | Use When |
|---------|------|----------|
| **Find related code** | `make query QUERY="search terms"` | Searching for code |
| **Check Implementation** | `make explore QUERY="search terms"` | Before implementing anything |
| **Understand patterns** | `doc/capabilities/*.md` | Implementation examples + patterns |
| **Find existing code** | `mcp_grep` or `mcp_lsp_find_references` | Searching for implementations |

## **Attention**: Skills needed to understand files

Skills are referenced per-file in comments below.  The lookup path for the skills is: 
`{guidance_dir}/skills/{skill}/SKILL.md`

So if you find a file you're looking for named file.rs:
`file.rs      # [zig-current, gof-patterns] Summary of files' contents` , 
Then you you must read

```
{guidance_dir}/skills/zig-current/SKILL.md
{guidance_dir}/skills/gof-patterns/SKILL.md
```

---

## Directory Tree (Git-Tracked Files Only)

```
"#;

enum Node {
    Dir(BTreeMap<String, Node>),
    File { rel_path: String },
}

fn insert_path(root: &mut BTreeMap<String, Node>, components: &[&str], rel_path: &str) {
    if components.is_empty() {
        return;
    }
    let first = components[0].to_string();
    if components.len() == 1 {
        root.entry(first).or_insert(Node::File {
            rel_path: rel_path.to_string(),
        });
    } else {
        let child = root
            .entry(first)
            .or_insert_with(|| Node::Dir(BTreeMap::new()));
        if let Node::Dir(children) = child {
            insert_path(children, &components[1..], rel_path);
        }
    }
}

fn build_ptree(node: &Node, builder: &mut TreeBuilder, json_dir: &Path, skills: &[String]) {
    match node {
        Node::Dir(children) => {
            for (name, child) in children {
                match child {
                    Node::Dir(_) => {
                        builder.begin_child(format!("{name}/"));
                        build_ptree(child, builder, json_dir, skills);
                        builder.end_child();
                    }
                    Node::File { rel_path } => {
                        let annotation = annotate_file(rel_path, name, json_dir, skills);
                        builder.add_empty_child(annotation);
                    }
                }
            }
        }
        Node::File { rel_path } => {
            let annotation = annotate_file(rel_path, "", json_dir, skills);
            builder.add_empty_child(annotation);
        }
    }
}

fn is_annotatable(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let dotted = format!(".{ext}");
    matches!(
        FileType::from_extension(&dotted),
        FileType::Source | FileType::Markdown
    )
}

fn annotate_file(rel_path: &str, name: &str, json_dir: &Path, skills: &[String]) -> String {
    let display_name = if name.is_empty() {
        Path::new(rel_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| rel_path.to_string())
    } else {
        name.to_string()
    };

    let path = PathBuf::from(rel_path);

    let mut desc = String::new();
    let mut file_skills: Vec<String> = Vec::new();

    let json_path = json_dir.join("src").join(rel_path).with_extension("json");
    if let Some(doc) = load_guidance(&json_path).ok().flatten() {
        let summary = source_summary(&doc);
        if !summary.is_empty() {
            desc = first_line(summary);
        }
        file_skills = extract_skills(&doc, skills);
    } else if is_annotatable(&path) && path.exists() {
        if let Ok(content) = common_core::io::read_to_string_err(&path) {
            let tiny = tiny_summary(&content);
            if !tiny.is_empty() {
                desc = first_line(&tiny);
            } else {
                desc = first_line(rel_path);
            }
        }
    }

    desc = truncate(&desc, 40);

    if !file_skills.is_empty() || !desc.is_empty() {
        let tag_str = if file_skills.is_empty() {
            String::new()
        } else {
            format!("[{}] ", file_skills.join(", "))
        };
        format!("{display_name}  # {tag_str}{desc}")
    } else {
        display_name
    }
}

fn extract_skills(doc: &GuidanceDoc, available: &[String]) -> Vec<String> {
    let mut matched: Vec<String> = Vec::new();
    for skill in &doc.skills {
        let skill_name = Path::new(skill.ref_path.as_str())
            .parent()
            .and_then(|p| p.file_name())
            .or_else(|| Path::new(skill.ref_path.as_str()).file_stem())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| skill.ref_path.to_string());
        if available.iter().any(|a| a == &skill_name) && !matched.contains(&skill_name) {
            matched.push(skill_name);
        }
    }
    matched
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
}

fn tiny_summary(text: &str) -> String {
    common_core::string::truncate_at_sentence(text, 40)
}

fn source_summary(doc: &GuidanceDoc) -> &str {
    doc.comment.as_deref().unwrap_or("")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        let idx = s.floor_char_boundary(max - 3);
        format!("{}...", &s[..idx])
    } else {
        s[..s.floor_char_boundary(max)].to_string()
    }
}

pub fn generate(guidance_dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let json_dir = guidance_dir;

    let files = get_git_tracked_files()?;
    let skills = get_available_skills(guidance_dir);

    let mut root: BTreeMap<String, Node> = BTreeMap::new();
    for file_path in &files {
        // Skip hidden files/dirs (names starting with '.')
        if file_path.split('/').any(|c| c.starts_with('.')) {
            continue;
        }
        let components: Vec<&str> = file_path.split('/').collect();
        insert_path(&mut root, &components, file_path);
    }

    let mut builder = TreeBuilder::new(".".to_string());
    for (name, node) in &root {
        match node {
            Node::Dir(_) => {
                builder.begin_child(format!("{name}/"));
                build_ptree(node, &mut builder, json_dir, &skills);
                builder.end_child();
            }
            Node::File { rel_path } => {
                let annotation = annotate_file(rel_path, name, json_dir, &skills);
                builder.add_empty_child(annotation);
            }
        }
    }
    let tree = builder.build();

    let mut buf: Vec<u8> = Vec::new();
    let config = PrintConfig {
        indent: 4,
        ..PrintConfig::default()
    };
    ptree::write_tree_with(&tree, &mut buf, &config)?;
    let tree_str = String::from_utf8(buf)?;

    let output = format!("{HEADER}{tree_str}```\n");
    Ok(output)
}

fn get_git_tracked_files() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = common_core::shell::run_capture(&["git", "ls-files"])?;
    if !output.success {
        return Err("git ls-files failed".into());
    }
    let mut files: Vec<String> = output
        .stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();
    files.sort();
    Ok(files)
}

/// List available skill directories. Uses a flat (non-recursive) `read_dir`
/// intentionally: this enumerates subdirectory names, not files, which is
/// outside the scope of `common_core::walk::walk_files`.
fn get_available_skills(guidance_dir: &Path) -> Vec<String> {
    let skills_dir = guidance_dir.join("skills");
    let mut skills = Vec::new();
    if skills_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        skills.push(name.to_string());
                    }
                }
            }
        }
    }
    skills.sort();
    skills
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_types::{GuidanceDoc, Skill};
    use std::sync::Mutex;

    // `generate` reads git-tracked files from the CURRENT working directory,
    // so the shell-out test below temporarily changes it. The lock serializes
    // that mutation against any other test that might depend on the cwd.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 40), "hello");
    }

    #[test]
    fn truncate_long_string_adds_ellipsis() {
        assert_eq!(truncate(&"a".repeat(100), 10), "aaaaaaa...");
    }

    #[test]
    fn truncate_tiny_max_never_panics() {
        assert_eq!(truncate("hello", 1), "h");
        assert_eq!(truncate("hello", 2), "he");
        assert_eq!(truncate("hello", 3), "hel");
    }

    #[test]
    fn first_line_returns_only_first_line() {
        assert_eq!(first_line("one\ntwo"), "one");
        assert_eq!(first_line("single"), "single");
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn extract_skills_matches_only_available_skills() {
        let doc = GuidanceDoc {
            skills: vec![
                Skill {
                    ref_path: "skills/zig-current/SKILL.md".into(),
                    context: None,
                },
                Skill {
                    ref_path: "skills/gof-patterns/SKILL.md".into(),
                    context: None,
                },
                Skill {
                    ref_path: "skills/unknown/SKILL.md".into(),
                    context: None,
                },
            ],
            ..GuidanceDoc::default()
        };
        let available = vec!["zig-current".to_string(), "gof-patterns".to_string()];
        assert_eq!(
            extract_skills(&doc, &available),
            vec!["zig-current", "gof-patterns"]
        );
    }

    #[test]
    fn get_available_skills_lists_skill_dirs_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let skills = dir.path().join("skills");
        std::fs::create_dir_all(skills.join("zig-current")).expect("mkdir");
        std::fs::create_dir_all(skills.join("gof")).expect("mkdir");
        // A plain file under skills must be ignored (dirs only).
        std::fs::write(skills.join("NOT_A_DIR"), "x").expect("write");
        assert_eq!(get_available_skills(dir.path()), vec!["gof", "zig-current"]);
        // No skills dir -> empty.
        let empty = tempfile::tempdir().expect("tempdir");
        assert!(get_available_skills(empty.path()).is_empty());
    }

    #[test]
    fn insert_path_builds_nested_tree() {
        let mut root: BTreeMap<String, Node> = BTreeMap::new();
        insert_path(&mut root, &["src", "main.zig"], "src/main.zig");
        insert_path(&mut root, &["src", "lib", "util.zig"], "src/lib/util.zig");
        insert_path(&mut root, &["README.md"], "README.md");
        assert!(root.contains_key("src"));
        assert!(root.contains_key("README.md"));
        match &root["src"] {
            Node::Dir(children) => {
                assert!(children.contains_key("main.zig"));
                assert!(children.contains_key("lib"));
                assert!(matches!(children["lib"], Node::Dir(_)));
            }
            _ => panic!("src should be a dir node"),
        }
        assert!(matches!(root["README.md"], Node::File { .. }));
    }

    fn write_doc(json_dir: &Path, rel_path: &str, comment: Option<&str>, skills: &[&str]) {
        let json_path = json_dir
            .join("src")
            .join(rel_path)
            .with_extension("json");
        std::fs::create_dir_all(json_path.parent().expect("parent")).expect("mkdir");
        let skills_json: Vec<serde_json::Value> = skills
            .iter()
            .map(|s| serde_json::json!({"ref": s}))
            .collect();
        let body = serde_json::json!({
            "meta": {"module": "app", "source": rel_path},
            "comment": comment,
            "skills": skills_json,
        });
        std::fs::write(&json_path, serde_json::to_string(&body).expect("serialize"))
            .expect("write doc");
    }

    #[test]
    fn annotate_file_source_comment() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_doc(dir.path(), "app.rs", Some("Does things well"), &[]);
        assert_eq!(
            annotate_file("app.rs", "app.rs", dir.path(), &[]),
            "app.rs  # Does things well"
        );
    }

    #[test]
    fn annotate_file_source_no_comment() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_doc(dir.path(), "app.rs", None, &[]);
        assert_eq!(annotate_file("app.rs", "app.rs", dir.path(), &[]), "app.rs");
    }

    #[test]
    fn annotate_file_text_sentence_boundary_before_limit() {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        let old = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(tmp.path()).expect("set cwd to tempdir");
        let result = {
            std::fs::write(
                "notes.md",
                "1234567890123456789012345. Rest of text goes on and on and on.",
            )
            .expect("write");
            annotate_file("notes.md", "notes.md", tmp.path(), &[])
        };
        std::env::set_current_dir(&old).expect("restore cwd");
        assert_eq!(result, "notes.md  # 1234567890123456789012345.");
    }

    #[test]
    fn annotate_file_text_no_sentence_boundary_before_limit() {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        let old = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(tmp.path()).expect("set cwd to tempdir");
        let result = {
            std::fs::write(
                "plain.md",
                "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ rest of text",
            )
            .expect("write");
            annotate_file("plain.md", "plain.md", tmp.path(), &[])
        };
        std::env::set_current_dir(&old).expect("restore cwd");
        assert_eq!(
            result,
            "plain.md  # abcdefghijklmnopqrstuvwxyz0123456789ABCD"
        );
    }

    #[test]
    fn annotate_file_empty_falls_back_to_path() {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        let old = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(tmp.path()).expect("set cwd to tempdir");
        let result = {
            std::fs::write("empty.md", "").expect("write");
            annotate_file("empty.md", "empty.md", tmp.path(), &[])
        };
        std::env::set_current_dir(&old).expect("restore cwd");
        assert_eq!(result, "empty.md  # empty.md");
    }

    #[test]
    fn annotate_file_skills_block_with_description() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_doc(
            dir.path(),
            "app.rs",
            Some("Does things well"),
            &["skills/zig-current/SKILL.md"],
        );
        assert_eq!(
            annotate_file("app.rs", "app.rs", dir.path(), &["zig-current".to_string()]),
            "app.rs  # [zig-current] Does things well"
        );
    }

    #[test]
    fn annotate_file_long_description_uses_final_truncation() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_doc(
            dir.path(),
            "app.rs",
            Some("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ rest of text"),
            &[],
        );
        assert_eq!(
            annotate_file("app.rs", "app.rs", dir.path(), &[]),
            "app.rs  # abcdefghijklmnopqrstuvwxyz0123456789A..."
        );
    }

    #[test]
    fn tiny_summary_short_text_unchanged() {
        assert_eq!(tiny_summary("Hello"), "Hello");
    }

    #[test]
    fn tiny_summary_long_text_truncated_to_limit() {
        let long = "A. ".repeat(500);
        let text = &long[..long.len() - 2];
        assert!(tiny_summary(text).len() <= 40);
    }

    #[test]
    fn tiny_summary_prefers_sentence_boundary() {
        assert_eq!(
            tiny_summary("1234567890123456789012345. Rest of text goes on."),
            "1234567890123456789012345."
        );
    }

    #[test]
    fn generate_renders_git_tracked_files() {
        // Guard: the test shells out to `git`; skip cleanly when absent.
        if common_core::shell::run_capture(&["git", "--version"]).is_err() {
            eprintln!("git not available; skipping structure generate test");
            return;
        }
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().expect("tempdir");
        let old = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(tmp.path()).expect("set cwd to tempdir");

        let result = (|| -> Result<String, Box<dyn std::error::Error>> {
            common_core::shell::run_capture(&["git", "init", "-q"])?;
            common_core::shell::run_capture(&["git", "config", "user.email", "t@example.com"])?;
            common_core::shell::run_capture(&["git", "config", "user.name", "t"])?;
            std::fs::create_dir_all(tmp.path().join("src"))?;
            std::fs::write(tmp.path().join("src/main.zig"), "pub fn main() void {}\n")?;
            common_core::shell::run_capture(&["git", "add", "-A"])?;
            let _ = common_core::shell::run_capture(&["git", "commit", "-q", "-m", "init"]);
            generate(tmp.path())
        })();

        std::env::set_current_dir(&old).expect("restore cwd");
        let output = result.expect("generate");
        assert!(
            output.contains("main.zig"),
            "tree must include the tracked file:\n{output}"
        );
    }
}
