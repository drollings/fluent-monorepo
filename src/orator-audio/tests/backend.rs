//! Backend tests: enumeration never fails; the ring contract holds.

use rtrb::RingBuffer;

use crate::backend::list_input_devices;

#[test]
fn device_listing_never_fails() {
    // Headless CI yields an empty list; hardware yields names. Either way,
    // listing itself must not raise.
    let _devices = list_input_devices();
}

#[test]
fn ring_push_pop_is_lossless_in_order() {
    let (mut producer, mut consumer) = RingBuffer::new(8);
    for sample in 0..8 {
        producer.push(sample).expect("ring has room");
    }
    assert!(producer.push(99).is_err());
    for sample in 0..8 {
        assert_eq!(consumer.pop().expect("ring has data"), sample);
    }
    assert!(consumer.pop().is_err());
}

#[test]
fn rt_overflow_drops_instead_of_blocking() {
    let (mut producer, mut consumer) = RingBuffer::new(2);
    producer.push(1.0f32).expect("room");
    producer.push(2.0).expect("room");
    // Full ring: the RT callback drops, counted downstream as a gap.
    assert!(producer.push(3.0).is_err());
    assert!((consumer.pop().expect("data") - 1.0).abs() < f32::EPSILON);
    producer.push(3.0).expect("room again");
}
