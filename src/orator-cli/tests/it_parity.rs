//! Daemon/CLI parity: the clap surface equals `COMMANDS`, and every wire
//! method is buildable through the same validator the daemon serves.

use std::collections::HashSet;

use orator_cli::{Cli, OutboundRequest, COMMANDS};
use orator_core::protocol::METHOD_NAMES;
use orator_daemon::DISPATCH_METHODS;

#[test]
fn clap_surface_matches_canonical_commands() {
    let surface: HashSet<String> = Cli::surface().into_iter().collect();
    let canonical: HashSet<String> = COMMANDS.into_iter().map(str::to_string).collect();
    assert_eq!(surface, canonical);
}

#[test]
fn every_daemon_method_is_addressable_from_the_cli() {
    for method in DISPATCH_METHODS {
        assert!(
            METHOD_NAMES.contains(&method),
            "daemon serves outside the shared contract: {method}"
        );
        OutboundRequest::new(method).expect("shared contract builds");
    }
}

#[test]
fn cli_method_set_equals_wire_contract() {
    let cli: HashSet<&str> = Cli::methods().into_iter().collect();
    let wired: HashSet<&str> = METHOD_NAMES.into_iter().collect();
    assert_eq!(cli, wired);
    assert_eq!(METHOD_NAMES.len(), 14);
    assert_eq!(DISPATCH_METHODS.len(), 14);
    for method in ["StopDaemon", "Reload", "ExportMeeting"] {
        OutboundRequest::new(method).expect("lifecycle methods build");
    }
}
