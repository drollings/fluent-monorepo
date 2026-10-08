use super::*;

#[test]
fn drawing_builder_and_serde_round_trip() {
    let drawing = Drawing::new()
        .kind(DrawingKind::Label)
        .bar_index(3)
        .price(100.5)
        .text("breakout".to_owned())
        .build();
    assert_eq!(drawing.kind, DrawingKind::Label);
    let json = serde_json::to_string(&drawing).expect("serialize");
    let back: Drawing = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, drawing);
}
