//! Operator tool: encode a YaGO taxonomy TTL to a YSM1 binary.
//!
//! Usage: `ysm1_encode <input.ttl> <output.ysm1>`
//!
//! The TTL remains the source of truth; this is the only trusted YSM1
//! producer. Hooked into the `make yago-load` flow as a post-step so the
//! downloaded taxonomy is encoded once, at fetch time, and every boot after
//! that decodes the binary instead of re-parsing the TTL.

/// Operator tool: encode a YaGO taxonomy TTL to a YSM1 binary.
/// 
/// Usage: `ysm1_encode <input.ttl> <output.ysm1>`
/// 
/// The TTL remains the source of truth; this is the only trusted YSM1
/// producer. Hooked into the `make yago-load` flow as a post-step so the
/// downloaded taxonomy is encoded once, at fetch time, and every boot after
/// that decodes the binary instead of re-parsing the TTL.
fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| {
        eprintln!("usage: ysm1_encode <input.ttl> <output.ysm1>");
        std::process::exit(2);
    });
    let output = args.next().unwrap_or_else(|| {
        eprintln!("usage: ysm1_encode <input.ttl> <output.ysm1>");
        std::process::exit(2);
    });
    let ttl = std::fs::read_to_string(&input).unwrap_or_else(|e| {
        eprintln!("ysm1_encode: read {input}: {e}");
        std::process::exit(1);
    });
    let bytes = guidance_ontology::ysm1::encode_ttl(&ttl);
    std::fs::write(&output, &bytes).unwrap_or_else(|e| {
        eprintln!("ysm1_encode: write {output}: {e}");
        std::process::exit(1);
    });
    eprintln!(
        "ysm1_encode: {} TTL bytes -> {} YSM1 bytes",
        ttl.len(),
        bytes.len()
    );
}