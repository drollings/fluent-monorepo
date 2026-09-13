//! Focused unit tests for `common_core::blob_spec::BlobCursor`, independent
//! of any blob format: synthetic buffers pin the cursor mechanics (advance,
//! absolute reads, overrun `None`s, magic checks) no caller depends on.

use common_core::blob_spec::{BlobCursor, BlobError};

fn buf() -> Vec<u8> {
    // u16 0x0201 | u32 0x04030201... laid out explicitly:
    // [01 02] [03 04 05 06] [02 'h' 'i'] [ff]
    vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x02, b'h', b'i', 0xFF]
}

#[test]
fn advancing_reads_move_the_cursor() {
    let data = buf();
    let mut c = BlobCursor::new(&data);
    assert_eq!(c.offset(), 0);
    assert_eq!(c.u16_le(), Some(0x0201));
    assert_eq!(c.offset(), 2);
    assert_eq!(c.u32_le(), Some(0x0605_0403));
    assert_eq!(c.offset(), 6);
    assert_eq!(c.sized_str(), Some("hi"));
    assert_eq!(c.offset(), 9);
    assert_eq!(c.remaining(), 1);
}

#[test]
fn reads_past_the_end_return_none_without_moving() {
    let data = buf();
    let mut c = BlobCursor::with_offset(&data, data.len());
    assert_eq!(c.u16_le(), None);
    assert_eq!(c.u32_le(), None);
    assert_eq!(c.usize_le(), None);
    assert_eq!(c.sized_str(), None);
    assert_eq!(c.take(1), None);
    assert_eq!(c.offset(), data.len(), "failed reads never advance");
    // A partial tail: u16 needs 2 bytes, only 1 remains.
    let mut t = BlobCursor::with_offset(&data, data.len() - 1);
    assert_eq!(t.u16_le(), None);
    assert_eq!(t.take(2), None);
    assert_eq!(t.take(1), Some(&[0xFF][..]));
}

#[test]
fn usize_le_is_u32_wide() {
    let data = [0x2C, 0x00, 0x00, 0x00, 0xFF];
    let mut c = BlobCursor::new(&data);
    assert_eq!(c.usize_le(), Some(44));
    assert_eq!(c.offset(), 4);
}

#[test]
fn sized_str_rejects_overrun_and_bad_utf8() {
    // Length byte claims 4, only 2 follow.
    let data = [0x04, b'a', b'b'];
    let mut c = BlobCursor::new(&data);
    assert_eq!(c.sized_str(), None);
    // Empty string is representable (length 0).
    let data = [0x00, b'x'];
    let mut c = BlobCursor::new(&data);
    assert_eq!(c.sized_str(), Some(""));
    assert_eq!(c.offset(), 1);
    // Invalid UTF-8 body.
    let data = [0x01, 0xFF];
    let mut c = BlobCursor::new(&data);
    assert_eq!(c.sized_str(), None);
}

#[test]
fn absolute_reads_do_not_move_the_cursor() {
    let data = buf();
    let c = BlobCursor::with_offset(&data, 4);
    assert_eq!(c.peek_u16(0), Some(0x0201));
    assert_eq!(c.peek_u32(2), Some(0x0605_0403));
    assert_eq!(c.peek_u16(999), None);
    assert_eq!(c.slice(7, 2), Some(&b"hi"[..]));
    assert_eq!(c.slice(9, 2), None, "overrun");
    assert_eq!(c.slice(usize::MAX, 1), None, "overflow-safe");
    assert_eq!(c.offset(), 4, "peeks never advance");
}

#[test]
fn expect_magic_checks_and_advances() {
    let data = [0x31, 0x52, 0x4F, 0x53, 0x01, 0x00]; // "SOR1" + version 1
    let mut c = BlobCursor::new(&data);
    assert!(c.expect_magic(0x534F_5231).is_ok());
    assert_eq!(c.offset(), 4);
    let mut c = BlobCursor::new(&data);
    assert!(matches!(
        c.expect_magic(0xDEAD_BEEF),
        Err(BlobError::BadMagic(_))
    ));
    assert!(BlobCursor::new(&data[..2]).expect_magic(0x534F_5231).is_err());
}
