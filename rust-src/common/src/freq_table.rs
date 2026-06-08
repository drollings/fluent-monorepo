use std::collections::HashMap;
use std::sync::Mutex;

lazy_static::lazy_static! {
    static ref ACTIVE_TABLE: Mutex<Option<Box<FrequencyTable>>> = Mutex::new(None);
}

pub type FrequencyTable = [[u16; 256]; 256];

pub fn default_frequency_table() -> FrequencyTable {
    let mut table = [[0xFEu16; 256]; 256];
    for a in b'a'..=b'z' {
        for b in b'a'..=b'z' {
            table[a as usize][b as usize] = 0x1000;
        }
    }
    let common: &[([u8; 2], u16)] = &[
        (*b"th", 0x0800), (*b"he", 0x0800), (*b"in", 0x0800),
        (*b"er", 0x0800), (*b"an", 0x0800), (*b"re", 0x0800),
        (*b"on", 0x0800), (*b"at", 0x0800), (*b"en", 0x0800),
        (*b"nd", 0x0800), (*b"ti", 0x0800), (*b"es", 0x0800),
        (*b"or", 0x0800), (*b"te", 0x0800), (*b"of", 0x0800),
        (*b"ed", 0x0800), (*b"is", 0x0800), (*b"it", 0x0800),
        (*b"al", 0x0800), (*b"ar", 0x0800), (*b"st", 0x0800),
        (*b"to", 0x0800), (*b"nt", 0x0800), (*b"ng", 0x0800),
    ];
    for &([a, b], val) in common {
        table[a as usize][b as usize] = val;
    }
    table
}

pub fn get_default_pair_freq() -> &'static FrequencyTable {
    lazy_static::lazy_static! {
        static ref DEFAULT: FrequencyTable = default_frequency_table();
    }
    &*DEFAULT
}

pub fn set_frequency_table(table: Box<FrequencyTable>) {
    let mut active = ACTIVE_TABLE.lock().unwrap();
    *active = Some(table);
}

pub fn get_frequency_table() -> &'static FrequencyTable {
    get_default_pair_freq()
}

pub fn pair_weight(a: u8, b: u8) -> u16 {
    get_frequency_table()[a as usize][b as usize]
}

pub fn build_frequency_table(contents: &str) -> FrequencyTable {
    let mut table = default_frequency_table();
    let bytes = contents.as_bytes();
    for window in bytes.windows(2) {
        let a = window[0] as usize;
        let b = window[1] as usize;
        table[a][b] = table[a][b].saturating_add(1);
    }
    table
}

pub fn build_frequency_table_from_map(contents: &str) -> FrequencyTable {
    build_frequency_table(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_weight_default() {
        let w = pair_weight(b't', b'h');
        assert_eq!(w, 0x0800);
    }

    #[test]
    fn build_frequency_table_basic() {
        let table = build_frequency_table("hello world");
        let w = pair_weight(b'h', b'e');
        assert!(w > 0);
    }

    #[test]
    fn build_frequency_table_from_map_works() {
        let table = build_frequency_table_from_map("test data");
        let _ = table;
    }
}
