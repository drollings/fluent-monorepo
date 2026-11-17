use common_core::blob_spec::*;

const TEST_MAGIC: u32 = 0x534C_4D32;

#[test]
fn header_roundtrip() {
    let mut buf = vec![0u8; BlobHeader::LEN];
    buf[0..4].copy_from_slice(&TEST_MAGIC.to_le_bytes());
    buf[4..6].copy_from_slice(&HEADER_VERSION.to_le_bytes());
    buf[6..8].copy_from_slice(&1u16.to_le_bytes());
    buf[8..16].copy_from_slice(&0xdeadbeef_cafebabe_u64.to_le_bytes());
    buf[16..20].copy_from_slice(&5u32.to_le_bytes());
    buf[20..24].copy_from_slice(&44u32.to_le_bytes());
    buf[24..28].copy_from_slice(&0u32.to_le_bytes());
    let h = parse_header(&buf, &[TEST_MAGIC]).expect("header");
    assert_eq!(h.magic, TEST_MAGIC);
    assert_eq!(h.count, 5);
}

#[test]
fn bad_magic_rejected() {
    let mut buf = vec![0u8; BlobHeader::LEN];
    buf[0..4].copy_from_slice(&0xdeadbeef_u32.to_le_bytes());
    assert!(matches!(
        parse_header(&buf, &[TEST_MAGIC]),
        Err(BlobError::BadMagic(_))
    ));
}

#[test]
fn crc_validate() {
    let payload = b"hello";
    let mut h = crc32fast::Hasher::new();
    h.update(payload);
    let c = h.finalize();
    validate_crc(payload, c).expect("crc ok");
    assert!(validate_crc(payload, c.wrapping_add(1)).is_err());
}
