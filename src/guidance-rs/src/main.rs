use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

// ============================================================================
// CLI Definition
// ============================================================================

#[derive(Parser)]
#[command(name = "guidance-rs", about = "Rust AST → JSON guidance files")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sync guidance JSON for Rust source files
    Sync {
        #[arg(long)]
        scan: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        output: String,
        #[arg(long)]
        infill: bool,
        #[arg(long)]
        regen: bool,
        #[arg(long)]
        debug: bool,
    },
    /// Blank synthetic/mangled comments in guidance JSON files
    Scrub {
        #[arg(long)]
        scan: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        debug: bool,
    },
}

// ============================================================================
// Schema Models
// ============================================================================

#[derive(Serialize, Deserialize, Default)]
struct Meta {
    module: String,
    source: String,
    language: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Param {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Member {
    name: String,
    r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(default)]
    params: Vec<Param>,
    #[serde(skip_serializing_if = "Option::is_none")]
    returns: Option<String>,
    #[serde(default)]
    patterns: Vec<serde_json::Value>,
    #[serde(default)]
    tags: Vec<String>,
    is_pub: bool,
    #[serde(default)]
    members: Vec<Member>,
    #[serde(default)]
    skills: Vec<serde_json::Value>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    equivalents: Vec<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct GuidanceDoc {
    meta: Meta,
    #[serde(skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    skills: Vec<serde_json::Value>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    hashtags: Vec<String>,
    #[serde(default)]
    used_by: Vec<String>,
    members: Vec<Member>,
}

// ============================================================================
// Hashing & Parsing Logic
// ============================================================================

fn normalize_type(ty: &str) -> String {
    ty.replace(" ", "")
}

fn compute_hash(name: &str, params: &[Param], returns: Option<&str>) -> String {
    let mut sig = String::new();
    sig.push_str(name);
    sig.push('(');

    let param_strs: Vec<String> = params.iter().map(|p| {
        let t = p.r#type.as_deref().unwrap_or("Any");
        format!("{}:{}", p.name, normalize_type(t))
    }).collect();

    sig.push_str(&param_strs.join(","));
    sig.push_str(")->");
    sig.push_str(&normalize_type(returns.unwrap_or("void")));

    let mut hasher = Sha256::new();
    hasher.update(sig.as_bytes());
    hex::encode(hasher.finalize())
}

fn process_file(filepath: &Path, output_dir: &Path) {
    let source_code = fs::read_to_string(filepath).expect("Failed to read file");
    let syntax_tree = syn::parse_file(&source_code).expect("Failed to parse Rust code");

    let mut doc = GuidanceDoc::default();
    doc.meta.language = "rust".to_string();
    doc.meta.source = filepath.to_string_lossy().to_string();
    doc.meta.module = filepath.file_stem().unwrap().to_string_lossy().to_string();

    for item in syntax_tree.items {
        match item {
            syn::Item::Fn(func) => {
                let name = func.sig.ident.to_string();
                let is_pub = matches!(func.vis, syn::Visibility::Public(_));
                let member_type = if is_pub { "fn_decl" } else { "fn_private" };

                let mut params = Vec::new();
                for input in func.sig.inputs {
                    if let syn::FnArg::Typed(pat_type) = input {
                        if let syn::Pat::Ident(pat_ident) = &*pat_type.pat {
                            let type_str = quote::quote!(#pat_type).to_string();
                            params.push(Param {
                                name: pat_ident.ident.to_string(),
                                r#type: Some(type_str.replace(" ", "")),
                                default: None,
                            });
                        }
                    }
                }

                let returns = match &func.sig.output {
                    syn::ReturnType::Default => None,
                    syn::ReturnType::Type(_, ty) => Some(quote::quote!(#ty).to_string()),
                };

                let hash = compute_hash(&name, &params, returns.as_deref());
                let signature = format!("fn {}(...) -> {}", name, returns.as_deref().unwrap_or("void"));

                doc.members.push(Member {
                    name,
                    r#type: member_type.to_string(),
                    signature: Some(signature),
                    line: Some(func.sig.ident.span().start().line),
                    match_hash: Some(hash),
                    params,
                    returns,
                    is_pub,
                    ..Default::default()
                });
            }
            syn::Item::Struct(strct) => {
                let name = strct.ident.to_string();
                let is_pub = matches!(strct.vis, syn::Visibility::Public(_));

                let hash = {
                    let mut hasher = Sha256::new();
                    hasher.update(format!("{}({})", name, "").as_bytes());
                    hex::encode(hasher.finalize())
                };

                doc.members.push(Member {
                    name: name.clone(),
                    r#type: "struct".to_string(),
                    signature: Some(format!("struct {}()", name)),
                    line: Some(strct.ident.span().start().line),
                    match_hash: Some(hash),
                    is_pub,
                    ..Default::default()
                });
            }
            // Add other AST mappings (Enum, Impl blocks for Methods) here...
            _ => {}
        }
    }

    let rel_path = filepath.strip_prefix(".").unwrap_or(filepath);
    let mut out_path = PathBuf::from(output_dir);
    out_path.push("src");
    out_path.push(rel_path);
    out_path.set_extension("rs.json");

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }

    let json = serde_json::to_string_pretty(&doc).unwrap();
    fs::write(&out_path, json).unwrap();

    // Incremental compile safe: update modification time [cite: 141, 142]
    filetime::set_file_mtime(&out_path, filetime::FileTime::from_system_time(SystemTime::now())).unwrap();
}

fn file_needs_processing(filepath: &Path, json_path: &Path) -> bool {
    if !json_path.exists() { return true; }
    let src_meta = fs::metadata(filepath).unwrap();
    let json_meta = fs::metadata(json_path).unwrap();
    src_meta.modified().unwrap() > json_meta.modified().unwrap()
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Sync { scan, file, output, debug, .. } => {
            let output_dir = Path::new(output);

            let mut files_to_process = Vec::new();
            if let Some(f) = file {
                files_to_process.push(PathBuf::from(f));
            } else if let Some(s) = scan {
                for entry in WalkDir::new(s).into_iter().filter_map(|e| e.ok()) {
                    if entry.path().extension().map_or(false, |ext| ext == "rs") {
                        files_to_process.push(entry.path().to_path_buf());
                    }
                }
            }

            for filepath in files_to_process {
                let mut json_path = PathBuf::from(output_dir);
                json_path.push("src");
                json_path.push(&filepath);
                json_path.set_extension("rs.json");

                if file_needs_processing(&filepath, &json_path) {
                    if *debug { println!("Processing: {:?}", filepath); }
                    process_file(&filepath, output_dir);
                }
            }
        }
        Commands::Scrub { .. } => {
            println!("Scrub command not fully implemented in this minimal version.");
        }
    }
}
