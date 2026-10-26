//! Pass case: every composition shape the derive supports expands and runs.

use fluent_wvr::{Describable, FieldAccess};

#[derive(FieldAccess, Describable, Default, Clone)]
struct Leaf {
    amount: u32,
    label: String,
}

#[derive(FieldAccess, Describable, Default, Clone)]
struct Shapes {
    #[field(nested)]
    inner: Leaf,
    #[field(items = 2)]
    open: Vec<Leaf>,
    fixed: [Leaf; 2],
}

#[derive(FieldAccess, Describable, Clone)]
enum Union {
    Breakout { period: u32 },
    Off,
}

fn main() {
    let mut shapes = Shapes::default();
    shapes.set_field("inner.amount", "1").expect("nested");
    shapes.set_field("open.count", "1").expect("count");
    shapes.set_field("fixed.0.label", "x").expect("fixed index");
    let mut union = Union::Breakout { period: 3 };
    union.set_field("kind", "off").expect("arm switch");
    let _ = shapes.describe();
    let _ = union.describe();
}
