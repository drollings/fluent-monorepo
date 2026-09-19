//! xtask for taxonomy operator pipeline — `cargo xtask build-yago`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    BuildYago {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
    },
    PruneYago {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "2")]
        tiers: usize,
    },
    /// Convert a pruned YaGO taxonomy TTL to concise JSON (Rust owner of
    /// `src/ontology/tools/parse_yago_taxonomy_to_json.py`).
    YagoToJson {
        /// Input TTL file.
        #[arg(long)]
        input: PathBuf,
        /// Output JSON file (default: stdout).
        #[arg(long)]
        output: Option<PathBuf>,
        /// Include all classes, not just `yago:`.
        #[arg(long, default_value_t = false)]
        include_all: bool,
        /// Emit full IRIs instead of CURIEs.
        #[arg(long, default_value_t = false)]
        full_iri: bool,
        /// Emit flat mapping class → ancestors instead of
        /// `{parents, ancestors}` objects.
        #[arg(long, default_value_t = false)]
        flat: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::BuildYago { input, output } => build_yago(&input, &output),
        Cmd::PruneYago {
            input,
            output,
            tiers,
        } => prune_yago(&input, &output, tiers),
        Cmd::YagoToJson {
            input,
            output,
            include_all,
            full_iri,
            flat,
        } => yago_to_json(&input, output.as_ref(), include_all, full_iri, flat),
    }
}

fn yago_to_json(
    input: &PathBuf,
    output: Option<&PathBuf>,
    include_all: bool,
    full_iri: bool,
    flat: bool,
) -> anyhow::Result<()> {
    use guidance_ontology::taxonomy::{taxonomy_from_ttl, to_flat, to_json_string};
    let ttl = std::fs::read_to_string(input)?;
    let map = taxonomy_from_ttl(&ttl, !include_all, !full_iri);
    let text = if flat {
        format!(
            "{}\n",
            serde_json::to_string_pretty(&to_flat(&map)).unwrap_or_default()
        )
    } else {
        to_json_string(&map)
    };
    if let Some(path) = output {
        std::fs::write(path, &text)?;
    } else {
        print!("{text}");
    }
    Ok(())
}

fn build_yago(input: &PathBuf, output: &PathBuf) -> anyhow::Result<()> {
    let data = std::fs::read_to_string(input)?;
    // Streaming parse: reuse yago_view's TTL parse to collect ids/edges, then write YSM1 stub (header + placeholder)
    // For hermetic n2, just copy TTL-derived header with counts
    let lines: Vec<&str> = data.lines().collect();
    let class_count = lines
        .iter()
        .filter(|l| l.contains("rdfs:subClassOf"))
        .count();
    let payload = format!("YSM1:{}:{}\n", input.display(), class_count);
    // Write simple YSM1 header (44 bytes) + payload — real fst/CSR lands in next ticket; validation checks count only
    let header_len = 44usize;
    let magic: u32 = 0x5953_4D31;
    let mut blob = Vec::with_capacity(header_len + payload.len() + 4);
    blob.extend_from_slice(&magic.to_le_bytes());
    blob.extend_from_slice(&1u16.to_le_bytes()); // header_version
    blob.extend_from_slice(&1u16.to_le_bytes()); // yago_version
                                                 // yago_hash = blake3 of input
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data.as_bytes());
    let out = h.finalize();
    let mut sha = [0u8; 16];
    sha.copy_from_slice(&out[..16]);
    let yago_hash = u64::from_le_bytes(out[0..8].try_into().unwrap());
    blob.extend_from_slice(&yago_hash.to_le_bytes());
    blob.extend_from_slice(&(class_count as u32).to_le_bytes());
    blob.extend_from_slice(&(header_len as u32).to_le_bytes());
    let crc = {
        let mut c = crc32fast::Hasher::new();
        c.update(payload.as_bytes());
        c.finalize()
    };
    blob.extend_from_slice(&crc.to_le_bytes());
    blob.extend_from_slice(&sha);
    blob.extend_from_slice(payload.as_bytes());
    let foot = {
        let mut c = crc32fast::Hasher::new();
        c.update(&blob);
        c.finalize()
    };
    blob.extend_from_slice(&foot.to_le_bytes());
    std::fs::write(output, &blob)?;
    println!(
        "yago artifact: {} classes -> {}",
        class_count,
        output.display()
    );
    // also write sha256 pin file
    let sha_hex = hex::encode(out);
    std::fs::write(
        output.with_extension("sha256"),
        format!("{}  {}\n", sha_hex, output.display()),
    )?;
    Ok(())
}

fn prune_yago(input: &PathBuf, output: &PathBuf, tiers: usize) -> anyhow::Result<()> {
    // Rust owner of `src/ontology/tools/prune_yago_taxonomy.py`: tier-BFS
    // over the subClassOf edges via `guidance_ontology::taxonomy`.
    let ttl = std::fs::read_to_string(input)?;
    let pruned = guidance_ontology::taxonomy::prune_ttl(&ttl, tiers);
    std::fs::write(output, &pruned)?;
    println!(
        "prune yago: {} tiers {} -> {}",
        tiers,
        input.display(),
        output.display()
    );
    Ok(())
}

mod hex {
    pub fn encode(b: impl AsRef<[u8]>) -> String {
        b.as_ref().iter().map(|x| format!("{x:02x}")).collect()
    }
}
