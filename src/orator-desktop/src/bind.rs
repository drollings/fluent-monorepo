//! Desktop binding walks: probe each surface in rung order, fall back
//! honestly.
//!
//! No walk binds anything by itself. Portal/host/emit/service rungs
//! report *candidacy* (a live backend can bind at runtime when it
//! lands); fallback rungs report what works today (compositor
//! instructions, quoted CLI verbs, headless renders, the query
//! parser). Every walk composes the shared
//! `first_some_in_order_sync` walker — never a second fallback
//! loop — and every result lands in `doctor` as green-or-actionable:
//! the won rung names the working path or the exact pending bind.

use fluent_wvr::ladder::first_some_in_order_sync;

/// Observed session facts. Read once at the edge
/// (`read_desktop_env`); every walk below is a pure function of it,
/// so the headless matrix pins without a compositor, bus, or daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopEnv {
    /// `XDG_CURRENT_DESKTOP` (e.g. `KDE`, `COSMIC`, `GNOME`).
    pub current_desktop: Option<String>,
    /// Session bus addressable.
    pub session_bus: bool,
    /// Wayland session.
    pub wayland: bool,
    /// The daemon socket file exists.
    pub daemon_socket: bool,
}

/// Read the live session facts. Impure edge (env plus two stats);
/// calls nothing, binds nothing.
#[must_use]
pub fn read_desktop_env() -> DesktopEnv {
    let current_desktop = std::env::var("XDG_CURRENT_DESKTOP")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let session_bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        || user_bus_socket_exists();
    let daemon_socket = orator_core::socket_path().is_some_and(|path| path.exists());
    DesktopEnv {
        current_desktop,
        session_bus,
        wayland,
        daemon_socket,
    }
}

/// Whether the user-bus socket exists (`/run/user/<uid>/bus`).
fn user_bus_socket_exists() -> bool {
    let uid = orator_core::paths::current_uid();
    std::path::Path::new(&format!("/run/user/{uid}/bus")).exists()
}

/// Where global hotkeys bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyRoute {
    /// Portal GlobalShortcuts candidate: bus plus a desktop session
    /// (binds when the portal backend lands).
    PortalCandidate,
    /// Compositor custom shortcuts: static instructions for `desktop`.
    Compositor(String),
    /// Quoted CLI verbs, always available (`fallback_bindings`).
    Fallback,
}

/// Probe hotkeys: portal, then compositor instructions, then the
/// always-available CLI fallback.
#[must_use]
pub fn probe_hotkeys(env: &DesktopEnv) -> HotkeyRoute {
    #[derive(Clone, Copy)]
    enum Rung {
        Portal,
        Compositor,
        Fallback,
    }
    first_some_in_order_sync(
        [Rung::Portal, Rung::Compositor, Rung::Fallback],
        |rung| match rung {
            Rung::Portal => {
                (env.session_bus && env.current_desktop.is_some())
                    .then_some(HotkeyRoute::PortalCandidate)
            }
            Rung::Compositor => env
                .current_desktop
                .as_deref()
                .map(|desktop| HotkeyRoute::Compositor(compositor_instructions(desktop))),
            Rung::Fallback => Some(HotkeyRoute::Fallback),
        },
    )
    .unwrap_or(HotkeyRoute::Fallback)
}

/// Static custom-shortcut instructions per compositor family. Text the
/// operator follows once; always available, never a handshake.
fn compositor_instructions(desktop: &str) -> String {
    let family = desktop.to_uppercase();
    if family.contains("KDE") {
        "KDE: System Settings → Shortcuts → Custom Shortcuts, bind the fallback commands".to_string()
    } else if family.contains("COSMIC") {
        "COSMIC: Settings → Keyboard → Custom Shortcuts, bind the fallback commands".to_string()
    } else if family.contains("GNOME") {
        "GNOME: Settings → Keyboard → Custom Shortcuts, bind the fallback commands".to_string()
    } else {
        format!("{desktop}: bind the fallback commands as custom shortcuts in the compositor settings")
    }
}

/// Where the tray indicator lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayRoute {
    /// Tray host candidate: a desktop session is present (binds when
    /// the host backend lands; the headless render stays available).
    HostCandidate,
    /// Headless state render, always available.
    Headless,
}

/// Probe the tray: host candidate on a desktop session, else the
/// headless render (which is also what the candidate falls back to).
#[must_use]
pub fn probe_tray(env: &DesktopEnv) -> TrayRoute {
    #[derive(Clone, Copy)]
    enum Rung {
        Host,
        Headless,
    }
    first_some_in_order_sync([Rung::Host, Rung::Headless], |rung| match rung {
        Rung::Host => env.current_desktop.is_some().then_some(TrayRoute::HostCandidate),
        Rung::Headless => Some(TrayRoute::Headless),
    })
    .unwrap_or(TrayRoute::Headless)
}

/// Where approval/recording notifications go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyRoute {
    /// Daemon emit candidate: the socket exists (emits when the emit
    /// path lands; failures fall through to the CLI verbs).
    DaemonEmit,
    /// CLI verbs (`orator approve <id>`), always available.
    CliFallback,
}

/// Probe notifications: daemon emit on a present socket, else CLI verbs.
#[must_use]
pub fn probe_notify(env: &DesktopEnv) -> NotifyRoute {
    #[derive(Clone, Copy)]
    enum Rung {
        Emit,
        Cli,
    }
    first_some_in_order_sync([Rung::Emit, Rung::Cli], |rung| match rung {
        Rung::Emit => env.daemon_socket.then_some(NotifyRoute::DaemonEmit),
        Rung::Cli => Some(NotifyRoute::CliFallback),
    })
    .unwrap_or(NotifyRoute::CliFallback)
}

/// Where the control API serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbusRoute {
    /// Service candidate: the session bus is addressable (serves when
    /// the service backend lands; ambient callers still never approve).
    ServiceCandidate,
    /// Absent: the socket and CLI serve control today.
    Absent,
}

/// Probe D-Bus: service candidate on a bus, else absent.
#[must_use]
pub fn probe_dbus(env: &DesktopEnv) -> DbusRoute {
    #[derive(Clone, Copy)]
    enum Rung {
        Service,
        Absent,
    }
    first_some_in_order_sync([Rung::Service, Rung::Absent], |rung| match rung {
        Rung::Service => env.session_bus.then_some(DbusRoute::ServiceCandidate),
        Rung::Absent => Some(DbusRoute::Absent),
    })
    .unwrap_or(DbusRoute::Absent)
}

/// Where the launcher integration lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KrunnerRoute {
    /// Plugin candidate: KDE session (registers when the plugin
    /// backend lands; the parser answers today).
    PluginCandidate,
    /// Query parser only.
    ParserOnly,
}

/// Probe KRunner: plugin candidate on KDE, else the parser.
#[must_use]
pub fn probe_krunner(env: &DesktopEnv) -> KrunnerRoute {
    #[derive(Clone, Copy)]
    enum Rung {
        Plugin,
        Parser,
    }
    first_some_in_order_sync([Rung::Plugin, Rung::Parser], |rung| match rung {
        Rung::Plugin => env
            .current_desktop
            .as_deref()
            .is_some_and(|desktop| desktop.to_uppercase().contains("KDE"))
            .then_some(KrunnerRoute::PluginCandidate),
        Rung::Parser => Some(KrunnerRoute::ParserOnly),
    })
    .unwrap_or(KrunnerRoute::ParserOnly)
}

#[cfg(test)]
#[path = "../tests/bind.rs"]
mod tests;
