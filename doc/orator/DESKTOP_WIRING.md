# Desktop wiring plan (Murmur parity, Q11)

Each binding lands only with a headless-pinned pure model first and a
live-compositor note; no unverified handshake ships as done. Status of
each surface today:

| Surface | Pure model (headless-pinned) | Live wiring | Note |
| --- | --- | --- | --- |
| Global shortcuts | `hotkey::parse_chord`/`pretty` codec + `fallback_bindings` (`orator chat --ptt`, `orator toggle`) | Portal GlobalShortcuts unbound; compositor custom-shortcut path is the reliable route | Fallback always available, never a handshake |
| Tray | `TrayState`/`TrayStatus` state model | No `ksni` tray host wired | `Recording` never hides; states render without a host |
| Notifications | `Notification::approval`/`recording` shapes | No daemon emit path wired | Approve/deny/stop actions ride the existing approval hub when bound |
| KRunner | `parse_query` (`orator` prefix, `Ask`/`StartMeeting`/`Dictate` offers) | No runner registration | Parser rejects non-`orator` input (`oratorian`) |
| Shell init | `orator-cli::shell` scripts (`dictate --once` fill) | Ships today via `shell-init` | Completions via `clap_complete` |
| D-Bus | `dbus::serves` == `METHOD_NAMES` parity, `Caller::may_approve` (never D-Bus) | No service connection | Ambient callers can never approve |
| Clipboard | `paste_with_restore`/`capture_selection` (10 ms polls, 180/200 ms deadlines, restore-or-clear) | `ExternalClipboard` shells to `wl-copy`/`wl-paste` when present | Headless falls through the ladder to output-only |
| Injection | Ladder `ORDER` + `LadderMemory` + continuation spacing | RemoteDesktop/VirtualKeyboard/Uinput report `Unavailable` (skip, never fake) | Only `ClipboardPaste` and `OutputOnly` can succeed today |

Rules carried forward from the roadmap: ambient speech never approves
or executes; clipboard restore-or-clear keeps read-aloud text out of
later dictation; continuation joins same-window turns within 10 s.
