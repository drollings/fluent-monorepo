//! Deterministic workspace/crate/domain cards.
//!
//! Cards derive from `README.md` member lists, the root manifest's
//! `[workspace] members`, per-crate manifests, component gates, and
//! jump terms. No model: these are the orientation plane's Tier-0
//! facts, and the doc→code jump table for scoped pulls.

use std::path::{Path, PathBuf};

use crate::query::search_db::ProjectCard;

use crate::config::ProjectConfig;
use crate::jumps::JumpIndex;

/// Workspace member paths from the root manifest's `[workspace]`
/// members array (minimal scan, no new parser dependency). Anything
/// unparseable reads as no members — the map falls back to crate
/// directories, never fails.
#[must_use]
pub fn workspace_members(workspace_root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(workspace_root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut in_workspace = false;
    let mut in_array: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace = trimmed == "[workspace]";
            continue;
        }
        if !in_workspace {
            continue;
        }
        let scan = if let Some(buf) = in_array.as_mut() {
            buf.push('\n');
            buf.push_str(line);
            buf.clone()
        } else {
            let Some(eq) = trimmed.find('=') else {
                continue;
            };
            let (key, value) = trimmed.split_at(eq);
            if key.trim() != "members" {
                continue;
            }
            value[1..].to_string()
        };
        let Some(open) = scan.find('[') else {
            continue;
        };
        if in_array.is_none() {
            in_array = Some(scan.clone());
        }
        let body = &scan[open + 1..];
        let mut chars = body.chars().peekable();
        let mut current: Option<String> = None;
        let mut done = false;
        while let Some(c) = chars.next() {
            if c == ']' {
                done = true;
                break;
            }
            if c == '"' {
                if let Some(buf) = current.take() {
                    members.push(buf);
                } else {
                    current = Some(String::new());
                }
            } else if let Some(buf) = current.as_mut() {
                if c == '\\' {
                    if let Some(next) = chars.next() {
                        buf.push(next);
                    }
                } else {
                    buf.push(c);
                }
            }
        }
        if done {
            break;
        }
    }
    members.sort();
    members.dedup();
    members
}

/// Default source roots when the config omits `src_dirs`: workspace
/// members (or the conventional `src/` tree), never the whole workspace.
#[must_use]
pub fn default_source_roots(workspace: &Path) -> Vec<PathBuf> {
    let members: Vec<PathBuf> = workspace_members(workspace)
        .into_iter()
        .map(|member| workspace.join(member))
        .filter(|path| path.is_dir())
        .collect();
    if !members.is_empty() {
        return members;
    }
    let src = workspace.join("src");
    if src.is_dir() {
        vec![src]
    } else {
        vec![workspace.to_path_buf()]
    }
}

/// One discovered crate: manifest path, package name, and member dir.
struct CrateFacts {
    package: String,
    manifest: String,
    dir: String,
    description: String,
}

/// Crate manifests under the workspace with a `[package] name`.
fn discover_crates(workspace: &Path) -> Vec<CrateFacts> {
    let mut manifests = Vec::new();
    common_core::walk::walk_files(workspace, &["toml"], |path| {
        if path.file_name().and_then(|name| name.to_str()) == Some("Cargo.toml") {
            manifests.push(path.to_path_buf());
        }
    });
    manifests.sort();
    let mut crates = Vec::new();
    for manifest_path in manifests {
        let relative = manifest_path
            .strip_prefix(workspace)
            .unwrap_or(&manifest_path)
            .to_string_lossy()
            .replace('\\', "/");
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Some(package) = crate::read::doc_cards::manifest_package_name(&text) else {
            continue;
        };
        let dir = manifest_path
            .parent()
            .and_then(|parent| parent.strip_prefix(workspace).ok())
            .map(|parent| parent.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let description = crate::read::doc_cards::manifest_description(&text)
            .unwrap_or_else(|| "(no description)".to_string());
        crates.push(CrateFacts {
            package,
            manifest: relative,
            dir,
            description,
        });
    }
    crates
}

/// First `# ` heading and first section anchor of a doc, for card
/// titles and links. Both read through the shared chunker so card
/// anchors resolve to the same sections the import verifies.
fn doc_title_and_anchor(text: &str, source_path: &str) -> (Option<String>, String) {
    let mut title = None;
    for line in text.lines() {
        if let Some(heading) = line.strip_prefix("# ") {
            let heading = heading.trim();
            if !heading.is_empty() {
                title = Some(heading.to_string());
                break;
            }
        }
    }
    let (chunks, _) = crate::read::doc_cards::chunk_markdown_sections(source_path, text);
    let anchor = chunks
        .iter()
        .find(|chunk| chunk.anchor != "preamble")
        .map(|chunk| chunk.anchor.clone())
        .unwrap_or_default();
    (title, anchor)
}

/// Verbatim doc-section cards: one project card per non-empty markdown
/// section, with the live hash and the package paths it points at.
#[must_use]
pub fn build_doc_cards(
    workspace: &Path,
    cfg: &ProjectConfig,
    _jumps: Option<&JumpIndex>,
) -> Vec<ProjectCard> {
    let src_dirs = if cfg.src_dirs.is_empty() {
        default_source_roots(workspace)
    } else {
        cfg.src_dirs.iter().map(|dir| workspace.join(dir)).collect()
    };
    let capabilities = cfg
        .capabilities_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("doc/capabilities"));
    let docs =
        crate::jumps::doc_tree_files_with(workspace, &cfg.docs_dir, cfg.root_docs, &src_dirs);
    let mut cards = Vec::new();
    for path in docs {
        let relative = path
            .strip_prefix(workspace)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if crate::source_scope::is_fixture_path(&relative)
            || crate::eval::is_eval_path(&relative)
            || relative.starts_with(&format!("{}/", capabilities.display()))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (chunks, _gaps) = crate::read::doc_cards::chunk_markdown_sections(&relative, &text);
        for chunk in chunks {
            let code_paths = crate::component_map::paths_for_package(&chunk.crate_name);
            cards.push(ProjectCard {
                key: format!("doc:{}", chunk.id),
                kind: "doc".to_string(),
                title: chunk.title.clone(),
                body: chunk.text.clone(),
                source_path: chunk.source_path.clone(),
                anchor: chunk.anchor.clone(),
                aliases_json: "[]".to_string(),
                code_paths_json: serde_json::to_string(&code_paths)
                    .unwrap_or_else(|_| "[]".to_string()),
                symbols_json: "[]".to_string(),
                content_hash: chunk.content_hash.clone(),
                generated_at: crate::summary::now_ms(),
            });
        }
    }
    cards
}

/// Build every card: one workspace card, one per discovered crate,
/// one per owning component. Sorted by key; content hashes cover the
/// body so edits reconverge incrementally.
#[must_use]
pub fn build_project_cards(
    workspace: &Path,
    project: &str,
    jumps: Option<&JumpIndex>,
) -> Vec<ProjectCard> {
    let generated_at = crate::summary::now_ms();
    let members = workspace_members(workspace);
    let crates = discover_crates(workspace);
    let readme = workspace.join("README.md");
    let readme_text = std::fs::read_to_string(&readme).ok();
    let mut cards = Vec::new();
    let workspace_body = {
        let mut lines = vec![format!("# {project}"), String::new()];
        if let Some(text) = readme_text.as_deref() {
            let (title, _) = doc_title_and_anchor(text, "README.md");
            if let Some(heading) = title {
                lines.push(format!("Root doc: README.md ({heading})."));
            }
        }
        if members.is_empty() {
            lines.push("Workspace members: (none declared).".to_string());
        } else {
            lines.push(format!("Workspace members: {}.", members.join(", ")));
        }
        if crates.is_empty() {
            lines.push("Crates: (none discovered).".to_string());
        } else {
            let names: Vec<&str> = crates.iter().map(|facts| facts.package.as_str()).collect();
            lines.push(format!("Crates: {}.", names.join(", ")));
        }
        lines.join("\n")
    };
    let (workspace_source, workspace_anchor) = if readme.is_file() {
        ("README.md".to_string(), String::new())
    } else {
        ("Cargo.toml".to_string(), "manifest".to_string())
    };
    cards.push(ProjectCard {
        key: format!("workspace:{project}"),
        kind: "workspace".to_string(),
        title: project.to_string(),
        body: workspace_body.clone(),
        source_path: workspace_source,
        anchor: workspace_anchor,
        aliases_json: "[]".to_string(),
        code_paths_json: serde_json::to_string(&members).unwrap_or_else(|_| "[]".to_string()),
        symbols_json: "[]".to_string(),
        content_hash: common_core::hash::blake3_hex(workspace_body.as_bytes()),
        generated_at,
    });
    let mut by_component: std::collections::BTreeMap<String, Vec<&CrateFacts>> =
        std::collections::BTreeMap::new();
    for facts in &crates {
        let display = if facts.dir.is_empty() {
            "."
        } else {
            facts.dir.as_str()
        };
        let (member_readme, member_anchor) =
            if workspace.join(&facts.dir).join("README.md").is_file() {
                let anchor = std::fs::read_to_string(workspace.join(&facts.dir).join("README.md"))
                    .ok()
                    .map(|text| {
                        doc_title_and_anchor(
                            &text,
                            &format!("{}/README.md", facts.dir.trim_end_matches('/')),
                        )
                        .1
                    })
                    .unwrap_or_default();
                (
                    format!("{}/README.md", facts.dir.trim_end_matches('/')),
                    anchor,
                )
            } else {
                (facts.manifest.clone(), "manifest".to_string())
            };
        let gate = crate::component_map::component_gate_for_path(display);
        let component = gate
            .component
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        by_component
            .entry(component.clone())
            .or_default()
            .push(facts);
        let mut aliases = vec![facts.package.clone()];
        if let Some(index) = jumps {
            aliases.extend(index.terms_for_crate(&facts.package).iter().cloned());
        }
        aliases.sort();
        aliases.dedup();
        let code_paths = if facts.dir.is_empty() {
            vec![".".to_string()]
        } else {
            vec![facts.dir.clone()]
        };
        let body = format!(
            "# {}\n\nCrate `{}` (component `{component}`) at `{display}`.\nDescription: {}\nManifest: {}.\n",
            facts.package, facts.package, facts.description, facts.manifest,
        );
        cards.push(ProjectCard {
            key: format!("crate:{}", facts.package),
            kind: "crate".to_string(),
            title: facts.package.clone(),
            body: body.clone(),
            source_path: member_readme,
            anchor: member_anchor,
            aliases_json: serde_json::to_string(&aliases).unwrap_or_else(|_| "[]".to_string()),
            code_paths_json: serde_json::to_string(&code_paths)
                .unwrap_or_else(|_| "[]".to_string()),
            symbols_json: "[]".to_string(),
            content_hash: common_core::hash::blake3_hex(body.as_bytes()),
            generated_at,
        });
    }
    for (component, group) in &by_component {
        let names: Vec<&str> = group.iter().map(|facts| facts.package.as_str()).collect();
        let mut code_paths: Vec<String> = group
            .iter()
            .map(|facts| {
                if facts.dir.is_empty() {
                    ".".to_string()
                } else {
                    facts.dir.clone()
                }
            })
            .collect();
        code_paths.sort();
        code_paths.dedup();
        let body = format!(
            "# {component}\n\nComponent `{component}` owns: {}.\nCode: {}.\n",
            names.join(", "),
            code_paths.join(", "),
        );
        cards.push(ProjectCard {
            key: format!("domain:{component}"),
            kind: "domain".to_string(),
            title: component.clone(),
            body: body.clone(),
            source_path: "README.md".to_string(),
            anchor: String::new(),
            aliases_json: serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string()),
            code_paths_json: serde_json::to_string(&code_paths)
                .unwrap_or_else(|_| "[]".to_string()),
            symbols_json: "[]".to_string(),
            content_hash: common_core::hash::blake3_hex(body.as_bytes()),
            generated_at,
        });
    }
    cards.sort_by(|a, b| a.key.cmp(&b.key));
    cards
}

/// Totals for one project-card build (stored data, never thrown).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectMapReport {
    /// Cards stored this run (all layers).
    pub cards: usize,
    /// Doc cards stored this run (verbatim sections).
    pub doc_cards: usize,
    /// Cards skipped on matching stored hashes.
    pub skipped_unchanged: usize,
}

/// Rebuild the deterministic project cards and store the changed
/// ones. Model-free; unchanged cards skip by content hash.
pub fn import_project_cards(
    workspace: &Path,
    project: &str,
    db: &crate::query::search_db::GuidanceSearchDb,
    jumps: Option<&JumpIndex>,
) -> Result<ProjectMapReport, String> {
    import_project_cards_with(workspace, project, db, jumps, &ProjectConfig::default())
}

/// Import project and doc cards under one config; removed doc cards are swept.
pub fn import_project_cards_with(
    workspace: &Path,
    project: &str,
    db: &crate::query::search_db::GuidanceSearchDb,
    jumps: Option<&JumpIndex>,
    cfg: &ProjectConfig,
) -> Result<ProjectMapReport, String> {
    let mut cards = build_project_cards(workspace, project, jumps);
    cards.extend(build_doc_cards(workspace, cfg, jumps));
    let stored = db
        .project_cards_all()
        .map_err(|error| format!("project card read: {error}"))?;
    let known: std::collections::HashMap<&str, &ProjectCard> = stored
        .iter()
        .map(|card| (card.key.as_str(), card))
        .collect();
    let mut report = ProjectMapReport::default();
    let mut fresh = Vec::new();
    for card in &cards {
        let unchanged = known
            .get(card.key.as_str())
            .is_some_and(|existing| existing.content_hash == card.content_hash);
        if unchanged {
            report.skipped_unchanged += 1;
            continue;
        }
        fresh.push(card.clone());
    }
    if !fresh.is_empty() {
        db.replace_project_cards(&fresh)
            .map_err(|error| format!("project card store: {error}"))?;
        report.cards = fresh.len();
        report.doc_cards = fresh.iter().filter(|card| card.kind == "doc").count();
    }
    let doc_keys: Vec<String> = cards
        .iter()
        .filter(|card| card.kind == "doc")
        .map(|card| card.key.clone())
        .collect();
    db.delete_project_cards_not_in("doc", &doc_keys)
        .map_err(|error| format!("project card sweep: {error}"))?;
    Ok(report)
}

#[cfg(test)]
#[path = "../../tests/project_map.rs"]
mod project_map_tests;
