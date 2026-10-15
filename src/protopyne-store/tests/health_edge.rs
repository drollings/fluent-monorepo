//! The stream-health transition table: every combination, plus the
//! fire-once proofs.
//!
//! The machine fires on exactly one transition — recovery — and the table
//! proves it fires there and nowhere else. The sequence rows then prove the
//! two properties a table of single steps cannot: that a boot never fires,
//! and that a second outage fires again exactly once rather than inheriting
//! the first outage's edge.

use super::*;

/// One sampled transition and what it must produce.
struct Row {
    /// What the row proves.
    name: &'static str,
    /// The state going in.
    previous: HealthEdge,
    /// The sample.
    connected: bool,
    /// The state coming out.
    next: HealthEdge,
    /// Whether this transition wakes a repair pass.
    fires: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "a healthy boot is not a recovery",
            previous: HealthEdge::Init,
            connected: true,
            next: HealthEdge::Up,
            fires: false,
        },
        Row {
            name: "an unhealthy boot is not a recovery either",
            previous: HealthEdge::Init,
            connected: false,
            next: HealthEdge::Down,
            fires: false,
        },
        Row {
            name: "a recovery resolves to steady, silently",
            previous: HealthEdge::Up,
            connected: true,
            next: HealthEdge::Steady,
            fires: false,
        },
        Row {
            name: "a disconnect after a recovery is not a repair signal",
            previous: HealthEdge::Up,
            connected: false,
            next: HealthEdge::Down,
            fires: false,
        },
        Row {
            name: "recovery fires exactly once",
            previous: HealthEdge::Down,
            connected: true,
            next: HealthEdge::Up,
            fires: true,
        },
        Row {
            name: "staying down is silent",
            previous: HealthEdge::Down,
            connected: false,
            next: HealthEdge::Down,
            fires: false,
        },
        Row {
            name: "staying up is silent",
            previous: HealthEdge::Steady,
            connected: true,
            next: HealthEdge::Steady,
            fires: false,
        },
        Row {
            name: "a disconnect from steady loses nothing by itself",
            previous: HealthEdge::Steady,
            connected: false,
            next: HealthEdge::Down,
            fires: false,
        },
    ]
}

#[test]
fn transition_table_fires_only_on_recovery() {
    let mut saw_fire = false;
    let mut saw_quiet = false;
    for row in rows() {
        let (next, fires) = health_edge(row.previous, row.connected);
        assert_eq!(next, row.next, "next state, row: {}", row.name);
        assert_eq!(fires, row.fires, "edge, row: {}", row.name);
        assert_eq!(
            next_health(row.previous, row.connected),
            row.next,
            "the state function agrees, row: {}",
            row.name
        );
        if fires {
            saw_fire = true;
        } else {
            saw_quiet = true;
        }
    }
    // One firing row out of eight: the machine is almost always silent, and
    // the table proves it rather than asserting it.
    let firing = rows().iter().filter(|row| row.fires).count();
    assert_eq!(firing, 1, "exactly one transition in the table fires");
    assert!(saw_fire && saw_quiet);
}

#[test]
fn a_healthy_boot_never_fires_no_matter_how_long_it_stays_up() {
    let mut state = HealthEdge::Init;
    let mut fires = 0;
    for _ in 0..5 {
        let (next, fires_now) = health_edge(state, true);
        state = next;
        fires += u32::from(fires_now);
    }
    assert_eq!(fires, 0, "booting healthy must not schedule a repair pass");
    assert_eq!(state, HealthEdge::Steady);
}

#[test]
fn an_unhealthy_boot_fires_on_its_first_recovery_and_only_there() {
    let mut state = HealthEdge::Init;
    let mut fires = 0;
    for connected in [false, false, true, true, true] {
        let (next, fires_now) = health_edge(state, connected);
        state = next;
        fires += u32::from(fires_now);
    }
    assert_eq!(fires, 1, "one outage produces one edge");
    assert_eq!(state, HealthEdge::Steady);
}

#[test]
fn two_outages_fire_twice_with_silence_between_them() {
    let mut state = HealthEdge::Init;
    let mut fires = 0;
    for connected in [true, false, false, true, true, false, true, true] {
        let (next, fires_now) = health_edge(state, connected);
        state = next;
        fires += u32::from(fires_now);
    }
    assert_eq!(fires, 2, "each recovery fires once; nothing else does");
    assert_eq!(state, HealthEdge::Steady);
}
