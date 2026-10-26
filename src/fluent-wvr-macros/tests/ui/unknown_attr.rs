//! Compile-fail case: the field-attribute vocabulary stays closed.

use fluent_wvr::{Describable, FieldAccess};

#[derive(FieldAccess, Describable)]
struct Bad {
    #[field(not_a_real_arm = 1)]
    value: u32,
}

fn main() {}
