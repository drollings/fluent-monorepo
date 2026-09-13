# Taxonomy Blob Specification — Decoupled Artifacts

This document is the authoritative spec for the two versioned artifacts that
replace the former split `LemmaBlob SLM1` and the ad-hoc YaGO JSON/turtle loaders.

- **LemmaArtifact** — `SLM2` (`0x534C_4D32`), compile-time embedded via `include_bytes!`.
- **YagoArtifact** — `YSM1` (`0x5953_4D31`), runtime file via safe `std::fs::read`.

No `mmap`; no `memmap2`; `#![forbid(unsafe_code)]`.

## 1. Header (common shape, per-artifact)

Every artifact begins with a fixed-size header, little-endian, 8-byte aligned:

```text
[0..4)  u32 magic         // SLM2 = 0x534C_4D32 ("SLM2"), YSM1 = 0x5953_4D31 ("YSM1")
[4..6)  u16 header_version // format of this header (currently 1)
[6..8)  u16 section_version // semantic version of the section payload
[8..16) u64 section_hash   // content hash of the canonical source (lemma JSON or TTL)
[16..20) u32 count         // class/pos count
[20..24) u32 section_off   // offset of payload from file start
[24..28) u32 crc32         // IEEE crc32 of payload bytes
[28..44) u8[16] sha256     // first 16 bytes of sha256(payload)
[44..48) u32 foot_crc32    // trailing crc32 of entire file (including header)
```

No combined `build_hash`: consumers key invalidation off `lemma_hash` (`section_hash` of
`SLM2`) or `yago_hash` (`section_hash` of `YSM1`) individually. A lemma-only edit never
re-inserts YaGO rows.

`section_hash` is `blake3(canonical_bytes)[0..8]` interpreted little-endian (or `hash_utf8`
over the source text's hash), stable across builds.

## 2. LemmaArtifact (`SLM2`)

```
LemmaArtifact:
  header { magic SLM2, lemma_version, lemma_hash, pos_count, lemma_off, crc32, sha256 }
  lemma: { per-POS { rules Vec<(old,new)>, index fst::Set, exc phf::Map } }
```

- `pos_count` — number of POS sections.
- `lemma_off` — offset of lemma payload.
- Each POS entry: `u8 key_len, key, u32 rules_off, rules_len, index_off, index_len, exc_off, exc_len` (relative to file start).
- `rules`: `u32 n_rules, then n_rules × { u8 old_len, old, u8 new_len, new }`
- `index`: `fst::Set` bytes — sorted NUL-joined word list serialized as `fst` set (deterministic, `fst::SetBuilder`).
- `exc`: `phf::Map` bytes — surface → `(offset,len)` into NUL-joined lemma list, built via `phf_codegen`.
- All slices are `&'static [u8]` zero-copy when embedded via `include_bytes!`.
- Little-endian, validation via `rd_u32`/`slice` helpers (`lemma_blob.rs:218` style).
- Budget: `<1 MB` (n2); verifier checks `crc/sha`.

SLM1 reader retained one release for rollback; `SLM2` dispatch falls back to `SLM1` on `version != 2`.

## 3. YagoArtifact (`YSM1`)

```
YagoArtifact:
  header { magic YSM1, yago_version, yago_hash, class_count, edges_count, yago_off, crc32, sha256 }
  yago: { classes fst::Map curie→InterlinguaId(YagoClass), parents CSR{indptr,indices} }
  foot crc32
```

- `class_count` / `edges_count` — cardinalities.
- `yago_off` — offset of yago payload.
- `classes`: `fst::Map` `curie → u64 InterlinguaId` (`YagoClass` local_id). Keys are normalized CURIEs (`yago:Foo`, `schema:Bar`).
- `parents`: CSR (`indptr: Vec<u32>`, `indices: Vec<u32>`) encoding `child → parents`. No `ancestors_memo` flattening; `ancestors_of` memoizes first encounter via `OnceLock<HashMap<Id, Vec<Id>>>` lazy, amortized `O(depth)`.
- No strings section — share `vocab.strings()` first-wins intern via `StringStore`.
- Runtime loaded via `std::fs::read` + `postcard`/`fst` decode, safe, no `mmap`. Header validated `crc/sha`; sorted; `fst` round-trip; CSR topo acyclic.
- Budgets: `n2 <2.5 MB`, `n10 <12 MB`.

## 4. Traits (ISP/DIP)

```rust
trait LemmaView { fn index_contains(&self, key: &str, word: &str) -> bool; fn exc_for(&self, key: &str, surface: &str) -> Option<&[u8]>; fn rules(&self, key: &str) -> &[(String,String)]; }
trait YagoView { fn resolve_curie(&self, curie: &str) -> Option<InterlinguaId>; fn ancestors_of(&self, id: InterlinguaId) -> Vec<InterlinguaId>; fn is_subclass_of(&self, child: InterlinguaId, parent: InterlinguaId) -> bool; }
```

OCP: `fst`/`phf`/`CSR` behind trait, not exposed. `InMemory` vs `Sqlite` LSP.

## 5. Validation

- Sorted, no dup surface, `exc n*4 == offsets`, `fst` round-trip, CSR topo acyclic, `crc/sha` ok.
- Per-artifact hash independent.
- Spec crate: `src/common-core/src/blob_spec.rs` (sole spec).

## 6. Operational

- Hermetic `n2` fixture in `env/` or `src/ontology/fixtures/`, never network-fetched in tests.
- Operator `n10`: `cargo xtask build-yago --input yago-ttl --output yago.bin`, `tools/download_yago_taxonomy.sh` pins `sha256`.
- No `memmap2`; compile-time lemma via `include_bytes!(env!("LEMMA_BLOB"))` + `LazyLock<Arc<LemmaView>>`.
