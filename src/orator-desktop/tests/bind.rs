//! Binding-walk tests: every surface resolves on explicit session
//! facts — headless falls through to working fallbacks with zero
//! errors, live sessions surface their candidates. Hermetic: pure
//! `DesktopEnv` values, no compositor, bus, or daemon.

use crate::bind::{
    probe_dbus, probe_hotkeys, probe_krunner, probe_notify, probe_tray, DbusRoute, DesktopEnv,
    HotkeyRoute, KrunnerRoute, NotifyRoute, TrayRoute,
};

fn headless() -> DesktopEnv {
    DesktopEnv::default()
}

fn kde_live() -> DesktopEnv {
    DesktopEnv {
        current_desktop: Some("KDE".to_string()),
        session_bus: true,
        wayland: true,
        daemon_socket: true,
    }
}

#[test]
fn headless_resolves_every_fallback_without_error() {
    let env = headless();
    assert_eq!(probe_hotkeys(&env), HotkeyRoute::Fallback);
    assert_eq!(probe_tray(&env), TrayRoute::Headless);
    assert_eq!(probe_notify(&env), NotifyRoute::CliFallback);
    assert_eq!(probe_dbus(&env), DbusRoute::Absent);
    assert_eq!(probe_krunner(&env), KrunnerRoute::ParserOnly);
}

#[test]
fn live_kde_session_surfaces_every_candidate() {
    let env = kde_live();
    assert_eq!(probe_hotkeys(&env), HotkeyRoute::PortalCandidate);
    assert_eq!(probe_tray(&env), TrayRoute::HostCandidate);
    assert_eq!(probe_notify(&env), NotifyRoute::DaemonEmit);
    assert_eq!(probe_dbus(&env), DbusRoute::ServiceCandidate);
    assert_eq!(probe_krunner(&env), KrunnerRoute::PluginCandidate);
}

#[test]
fn compositor_rung_names_the_family_or_covers_unknown() {
    let gnome = DesktopEnv {
        current_desktop: Some("GNOME".to_string()),
        ..DesktopEnv::default()
    };
    let route = probe_hotkeys(&gnome);
    assert!(
        matches!(&route, HotkeyRoute::Compositor(text) if text.contains("GNOME")),
        "{route:?}"
    );
    let cosmic = DesktopEnv {
        current_desktop: Some("COSMIC".to_string()),
        ..DesktopEnv::default()
    };
    let route = probe_hotkeys(&cosmic);
    assert!(
        matches!(&route, HotkeyRoute::Compositor(text) if text.contains("COSMIC")),
        "{route:?}"
    );
    let tiling = DesktopEnv {
        current_desktop: Some("sway".to_string()),
        ..DesktopEnv::default()
    };
    let route = probe_hotkeys(&tiling);
    assert!(
        matches!(&route, HotkeyRoute::Compositor(text) if text.contains("sway")),
        "unknown compositors still get instructions: {route:?}"
    );
}

#[test]
fn bus_without_desktop_skips_portal_but_serves_dbus() {
    let env = DesktopEnv {
        session_bus: true,
        ..DesktopEnv::default()
    };
    assert_eq!(probe_hotkeys(&env), HotkeyRoute::Fallback);
    assert_eq!(probe_dbus(&env), DbusRoute::ServiceCandidate);
    assert_eq!(probe_tray(&env), TrayRoute::Headless);
}

#[test]
fn krunner_plugin_needs_kde_parser_serves_everywhere() {
    let gnome = DesktopEnv {
        current_desktop: Some("GNOME".to_string()),
        session_bus: true,
        ..DesktopEnv::default()
    };
    assert_eq!(probe_krunner(&gnome), KrunnerRoute::ParserOnly);
    assert_eq!(probe_hotkeys(&gnome), HotkeyRoute::PortalCandidate);
}

#[test]
fn notify_prefers_emit_only_with_a_socket() {
    let nosocket = DesktopEnv {
        current_desktop: Some("KDE".to_string()),
        session_bus: true,
        ..DesktopEnv::default()
    };
    assert_eq!(probe_notify(&nosocket), NotifyRoute::CliFallback);
}
