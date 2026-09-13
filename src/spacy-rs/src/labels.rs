//! The closed label vocabularies — fixed `repr` enums mirroring
//! `spacy/symbols.pxd` / `spacy/parts_of_speech.pxd`.
//!
//! These are the *canonical* label sets the walkthrough requires (§11.2): the
//! 17 UPOS tags (plus `NO_TAG`/`EOL`/`SPACE`), the reference dependency
//! relations, the NER entity types, and the IOB marker. Discriminants equal
//! the spaCy symbol ids so a `Doc::to_array`/`from_array` matrix round-trips
//! against real spaCy data.
//!
//! `Display` mirrors the spaCy `*_` accessors (`token.pos_` → `"noun"`,
//! `token.dep_` → `"nsubj"`, `token.ent_type_` → `"PERSON"`); `FromStr`
//! accepts any casing. Open vocabularies (orth, lemma, fine-grained tag, and
//! model-specific dep labels such as UD's `compound`/`case`) are *not* enums —
//! they are stored as `u64` hashes resolved through the vocabulary's
//! `StringStore`.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::SpacyError;

fluent_types::label_enum! {
    /// Universal part-of-speech tag. Discriminants are the `symbol_t` ids from
    /// `spacy/symbols.pxd` (which `univ_pos_t` aliases).
    ///
    /// Declared via [`fluent_types::label_enum`]: the table below is the single
    /// source of truth for the discriminants, the wire text (`as_str` /
    /// `Display`), and the case-folding parser (`FromStr`). Numeric (`id` /
    /// `from_id`), set (`UPOS`), and blob-key (`lemma_key`, which delegates to
    /// `as_str`) helpers stay hand-written beside it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    #[serde(rename_all = "lowercase")]
    pub enum Upos {
        /// Unset / unknown tag; `pos_` renders as the empty string.
        #[serde(rename = "no_tag")]
        NoTag = 0 => "",
        Adj = 84 => "adj",
        Adp = 85 => "adp",
        Adv = 86 => "adv",
        Aux = 87 => "aux",
        /// Deprecated alias of `CCONJ` (Universal Dependencies 2.0); kept for id
        /// parity but never produced by the validator.
        Conj = 88 => "conj",
        Cconj = 89 => "cconj",
        Det = 90 => "det",
        Intj = 91 => "intj",
        Noun = 92 => "noun",
        Num = 93 => "num",
        Part = 94 => "part",
        Pron = 95 => "pron",
        Propn = 96 => "propn",
        Punct = 97 => "punct",
        Sconj = 98 => "sconj",
        Sym = 99 => "sym",
        Verb = 100 => "verb",
        X = 101 => "x",
        /// Internal end-of-line tag; not part of the 17-tag contract.
        Eol = 102 => "eol",
        /// Internal whitespace-token tag; not part of the 17-tag contract.
        Space = 103 => "space",
    }
    err_ty SpacyError,
    err_ctor SpacyError::UnknownPos,
    normalize lowercase,
}

impl Upos {
    /// The 17 Universal Dependencies tags exposed to the LLM JSON contract.
    pub const UPOS: &'static [Self] = &[
        Self::Adj,
        Self::Adp,
        Self::Adv,
        Self::Aux,
        Self::Cconj,
        Self::Det,
        Self::Intj,
        Self::Noun,
        Self::Num,
        Self::Part,
        Self::Pron,
        Self::Propn,
        Self::Punct,
        Self::Sconj,
        Self::Sym,
        Self::Verb,
        Self::X,
    ];

    /// The label's `symbol_t` id.
    #[must_use]
    pub const fn id(self) -> u64 {
        self as u64
    }

    /// Reconstruct from a `symbol_t` id (the `to_array`/`from_array` value).
    pub fn from_id(value: u64) -> Result<Self, SpacyError> {
        match value {
            0 => Ok(Self::NoTag),
            84 => Ok(Self::Adj),
            85 => Ok(Self::Adp),
            86 => Ok(Self::Adv),
            87 => Ok(Self::Aux),
            88 => Ok(Self::Conj),
            89 => Ok(Self::Cconj),
            90 => Ok(Self::Det),
            91 => Ok(Self::Intj),
            92 => Ok(Self::Noun),
            93 => Ok(Self::Num),
            94 => Ok(Self::Part),
            95 => Ok(Self::Pron),
            96 => Ok(Self::Propn),
            97 => Ok(Self::Punct),
            98 => Ok(Self::Sconj),
            99 => Ok(Self::Sym),
            100 => Ok(Self::Verb),
            101 => Ok(Self::X),
            102 => Ok(Self::Eol),
            103 => Ok(Self::Space),
            other => Err(SpacyError::UnknownPos(other.to_string())),
        }
    }

    /// The lemma-blob table key for this tag: the same lowercase label
    /// [`Display`](fmt::Display) renders, as a `&'static str` with no
    /// allocation. Delegates to [`as_str`](Self::as_str) — the single source
    /// of truth — so the lemmatizer can never disagree with the renderer.
    #[must_use]
    pub const fn lemma_key(self) -> &'static str {
        self.as_str()
    }
}

fluent_types::label_enum! {
    /// Named-entity IOB marker, matching spaCy's `IOB_STRINGS = ("", "I", "O", "B")`
    /// (`spacy/attrs.pyx:4`). The transition parser works in BILUO internally, but
    /// the stored `ent_iob` is classic IOB.
    ///
    /// Declared via [`fluent_types::label_enum`] with `normalize exact`: parsing
    /// is a verbatim match (no case folding — `"i"` is rejected, unlike the
    /// case-folding Upos/NerType/DepRel vocabularies).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u8)]
    pub enum EntIoB {
        /// No entity annotation.
        Missing = 0 => "",
        Inside = 1 => "I",
        Outside = 2 => "O",
        Begin = 3 => "B",
    }
    err_ty SpacyError,
    err_ctor SpacyError::InvalidEntIobText,
    normalize exact,
}

impl EntIoB {
    /// The id as stored in `TokenC.ent_iob` and exported by `to_array`.
    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// Construct from the stored integer; rejects values ≥ 4 (the
    /// `from_array` bound, `doc.pyx:1130-1142`).
    pub const fn from_id(value: u8) -> Result<Self, SpacyError> {
        match value {
            0 => Ok(Self::Missing),
            1 => Ok(Self::Inside),
            2 => Ok(Self::Outside),
            3 => Ok(Self::Begin),
            other => Err(SpacyError::InvalidEntIob(other as u64)),
        }
    }
}

fluent_types::label_enum! {
    /// Named-entity type. Discriminants are the `symbol_t` ids for `PERSON` …
    /// `CARDINAL` (`spacy/symbols.pxd`).
    ///
    /// Declared via [`fluent_types::label_enum`] with `normalize uppercase`:
    /// lowercase input folds up before matching. The `symbol_t` id helper
    /// stays hand-written beside it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u16)]
    #[serde(rename_all = "UPPERCASE")]
    pub enum NerType {
        Person = 380 => "PERSON",
        Norp = 381 => "NORP",
        Facility = 382 => "FACILITY",
        Org = 383 => "ORG",
        Gpe = 384 => "GPE",
        Loc = 385 => "LOC",
        Product = 386 => "PRODUCT",
        Event = 387 => "EVENT",
        WorkOfArt = 388 => "WORK_OF_ART",
        Language = 389 => "LANGUAGE",
        Law = 390 => "LAW",
        Date = 391 => "DATE",
        Time = 392 => "TIME",
        Percent = 393 => "PERCENT",
        Money = 394 => "MONEY",
        Quantity = 395 => "QUANTITY",
        Ordinal = 396 => "ORDINAL",
        Cardinal = 397 => "CARDINAL",
    }
    err_ty SpacyError,
    err_ctor SpacyError::UnknownNerType,
    normalize uppercase,
}

impl NerType {
    /// The `symbol_t` id.
    #[must_use]
    pub const fn id(self) -> u64 {
        self as u64
    }
}

fluent_types::label_enum! {
    /// Canonical dependency relation. Discriminants are the `symbol_t` ids for
    /// `acomp` … `acl` (`spacy/symbols.pxd`), plus the modern Universal-Dependency
    /// labels that a current model actually emits (`compound`, `case`, `flat`, …)
    /// which have **no** spaCy symbol id — those get ids in the reserved
    /// 2000+ range (the stored `dep` field is always the content hash, so the
    /// numeric id is only for `to_array`/`from_array` interop on the symbol set).
    /// The validator accepts open labels via [`crate::labels::DepLabelSet`].
    ///
    /// Declared via [`fluent_types::label_enum`] with `normalize lowercase`.
    /// The `symbol_t` id helper stays hand-written beside it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[repr(u16)]
    #[serde(rename_all = "lowercase")]
    pub enum DepRel {
        Acomp = 398 => "acomp",
        Advcl = 399 => "advcl",
        Advmod = 400 => "advmod",
        Agent = 401 => "agent",
        Amod = 402 => "amod",
        Appos = 403 => "appos",
        Attr = 404 => "attr",
        Aux = 405 => "aux",
        Auxpass = 406 => "auxpass",
        Cc = 407 => "cc",
        Ccomp = 408 => "ccomp",
        Complm = 409 => "complm",
        Conj = 410 => "conj",
        Cop = 411 => "cop",
        Csubj = 412 => "csubj",
        Csubjpass = 413 => "csubjpass",
        Dep = 414 => "dep",
        Det = 415 => "det",
        Dobj = 416 => "dobj",
        Expl = 417 => "expl",
        Hmod = 418 => "hmod",
        Hyph = 419 => "hyph",
        Infmod = 420 => "infmod",
        Intj = 421 => "intj",
        Iobj = 422 => "iobj",
        Mark = 423 => "mark",
        Meta = 424 => "meta",
        Neg = 425 => "neg",
        Nmod = 426 => "nmod",
        Nn = 427 => "nn",
        Npadvmod = 428 => "npadvmod",
        Nsubj = 429 => "nsubj",
        Nsubjpass = 430 => "nsubjpass",
        Num = 431 => "num",
        Number = 432 => "number",
        Oprd = 433 => "oprd",
        Obj = 434 => "obj",
        Obl = 435 => "obl",
        Parataxis = 436 => "parataxis",
        Partmod = 437 => "partmod",
        Pcomp = 438 => "pcomp",
        Pobj = 439 => "pobj",
        Poss = 440 => "poss",
        Possessive = 441 => "possessive",
        Preconj = 442 => "preconj",
        Prep = 443 => "prep",
        Prt = 444 => "prt",
        Punct = 445 => "punct",
        Quantmod = 446 => "quantmod",
        Relcl = 447 => "relcl",
        Rcmod = 448 => "rcmod",
        Root = 449 => "root",
        Xcomp = 450 => "xcomp",
        Acl = 451 => "acl",
        // Modern UD labels (no spaCy symbol id; reserved 2000+ range).
        Compound = 2000 => "compound",
        Case = 2001 => "case",
        Fixed = 2002 => "fixed",
        Flat = 2003 => "flat",
        Discourse = 2004 => "discourse",
        Dislocated = 2005 => "dislocated",
        Goeswith = 2006 => "goeswith",
        List = 2007 => "list",
        Mixed = 2008 => "mixed",
        Nummod = 2009 => "nummod",
        Orphan = 2010 => "orphan",
        Reparandum = 2011 => "reparandum",
        Vocative = 2012 => "vocative",
    }
    err_ty SpacyError,
    err_ctor SpacyError::UnknownDepLabel,
    normalize lowercase,
}

impl DepRel {
    /// The `symbol_t` id.
    #[must_use]
    pub const fn id(self) -> u64 {
        self as u64
    }
}

/// A configurable set of dependency labels accepted by the annotation
/// validator (§10.2 check 2). The default starts from the canonical
/// [`DepRel`] reference set and adds the Universal-Dependencies labels a
/// modern `en_core_web_sm` actually emits (`compound`, `case`, `flat`, …),
/// so a finetuned model is not rejected for using current UD labels while
/// still failing closed on garbage.
///
/// The set is serde-able (serialized as the label list) and round-trips
/// through [`FromStr`]/[`Display`] (comma-joined), so a model's `label_data`
/// can override the default accepted set (§10.6, roadmap §5).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DepLabelSet {
    labels: std::collections::HashSet<String>,
}

impl DepLabelSet {
    /// The canonical reference set plus the UD labels from `en_core_web_sm`
    /// v3.8's tag map (observed label set).
    #[must_use]
    pub fn ud_default() -> Self {
        let mut set = Self::default();
        for rel in [
            "acomp",
            "advcl",
            "advmod",
            "agent",
            "amod",
            "appos",
            "attr",
            "aux",
            "auxpass",
            "case",
            "cc",
            "ccomp",
            "complm",
            "compound",
            "conj",
            "cop",
            "csubj",
            "csubjpass",
            "dep",
            "det",
            "discourse",
            "dislocated",
            "dobj",
            "expl",
            "fixed",
            "flat",
            "goeswith",
            "hmod",
            "hyph",
            "infmod",
            "intj",
            "iobj",
            "list",
            "mark",
            "meta",
            "mixed",
            "neg",
            "nmod",
            "nn",
            "npadvmod",
            "nsubj",
            "nsubjpass",
            "num",
            "number",
            "nummod",
            "obj",
            "obl",
            "oprd",
            "orphan",
            "parataxis",
            "partmod",
            "pcomp",
            "pobj",
            "poss",
            "possessive",
            "preconj",
            "prep",
            "prt",
            "punct",
            "quantmod",
            "rcmod",
            "reparandum",
            "relcl",
            "root",
            "vocative",
            "xcomp",
        ] {
            set.insert(rel.to_string());
        }
        set
    }

    /// Add a label to the accepted set.
    pub fn insert(&mut self, label: impl Into<String>) {
        self.labels.insert(label.into());
    }

    /// Whether `label` is accepted (case-insensitive).
    #[must_use]
    pub fn contains(&self, label: &str) -> bool {
        self.labels.contains(&label.to_ascii_lowercase())
    }

    /// Number of accepted labels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// The accepted labels, sorted for determinism.
    #[must_use]
    pub fn to_sorted_vec(&self) -> Vec<String> {
        let mut labels: Vec<String> = self.labels.iter().cloned().collect();
        labels.sort();
        labels
    }
}

impl fmt::Display for DepLabelSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_sorted_vec().join(","))
    }
}

impl FromStr for DepLabelSet {
    type Err = SpacyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut set = Self::default();
        for label in s.split(',').map(str::trim).filter(|l| !l.is_empty()) {
            // Reject garbage eagerly: every accepted label must resolve to the
            // canonical reference set or a known UD label.
            let parsed: DepRel = label.parse()?;
            set.insert(parsed.to_string());
        }
        Ok(set)
    }
}

#[cfg(test)]
#[path = "../tests/labels.rs"]
mod tests;
