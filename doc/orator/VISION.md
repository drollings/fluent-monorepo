# Orator — Vision

Orator is a fully local, Linux-first voice assistant. It lets you
speak to your computer the way you think: dictate into any window,
hold a real conversation with an assistant that listens while it
talks, get live captions of what others are saying, and walk out of
a meeting with a diarized transcript and a summary — all with every
byte of inference running on your own workstation.

## Who it serves

Anyone who works by voice on a Linux desktop: writers dictating
prose, developers driving tools hands-free, meeting participants who
need reliable notes, and accessibility users for whom the keyboard
is the wrong device. First-class homes are KDE Plasma and COSMIC
(Wayland), plain bash and other shells, and the terminal itself.

## What it delivers

- **Dictation that lands where you work.** Speak, and clean,
  punctuated text appears in the focused window — editor, browser,
  terminal — through the desktop's own input paths, with a
  compositor-keybinding fallback when portals are unavailable.
  Consecutive utterances join with natural spacing; nothing you said
  is ever silently dropped.
- **A conversational assistant that can be interrupted.** Ask
  questions, hear spoken answers, and cut in mid-reply — the
  assistant stops, listens, and resumes. Short acknowledgements
  ("uh-huh", "right") are ignored rather than treated as new
  requests.
- **Listen-in without side effects.** Hear a live transcript of
  system audio — a call, a video, someone at the door — rendered as
  text on screen or the terminal. Listen-in can never speak and can
  never trigger an action: it has no voice and no tools by
  construction, so background speech and media audio are always
  untrusted data, never commands.
- **Meetings that keep themselves.** Record with a lossless archive
  written before any inference runs, follow a live transcript while
  it happens, and get speaker-attributed transcripts ("Speaker 1",
  "Speaker 2", with suggestions), citable summaries where every
  claim resolves to a transcript segment, and file exports —
  resumable if anything crashes mid-way.
- **Tools the assistant can use, safely.** The assistant can run
  shell commands, fetch text, and manage notes through an explicit
  tool set with per-tool toggles. Anything sensitive asks first via
  an approval step (button, notification, or CLI confirmation) —
  and ambient speech from another person, a recording, or media can
  never approve or execute anything.
- **Operability without guesswork.** `orator doctor` reports device,
  model, endpoint, and desktop-integration health as green or
  actionable. `orator models` lists, fetches (explicitly, the only
  network activity), and hash-verifies model weights. `orator
  transcribe` turns audio files into timestamped text. `orator tui`
  gives a full terminal client with mic meter, transcript, and
  barge-in key. Tray icons, global hotkeys, desktop notifications, a
  KRunner runner, shell completions, and a D-Bus control API make it
  drivable from anywhere on the desktop.

## Guarantees the user can rely on

- **Private.** No telemetry, no accounts, no cloud. All inference is
  local; the network is touched only for an explicit model download
  or an explicitly enabled web tool.
- **Honest.** Every command either does the work or fails loudly
  with a named reason. There are no silent successes.
- **Lossless where it matters.** The recording is written upstream
  of all inference: if recognition falls behind or crashes, the
  audio is still on disk and can be processed later. Live,
  latency-sensitive paths degrade visibly (with counters) instead of
  growing without bound.
- **Frugal.** The idle daemon is small; models load lazily and
  unload when idle; the audio path holds ceilings, not histories.
- **Safe by structure.** Listening cannot act: sessions that hear
  other people carry no capability to touch effects, and every
  sandboxed tool runs jailed, audited, and killable as a group.

## How it is built (in brief)

Orator is written in Rust as a set of focused crates composed over
shared workspace foundations for supervised tasks, credit-gated
queues, cancellation, retries, retries-safe HTTP, SQLite mechanics,
and dependency graphs. Speech recognition, language understanding,
and speech synthesis come from established local-model families
served as supervised processes on the machine — the architecture
treats inference as a replaceable local endpoint, never a remote
service. Nothing in this document is a promise of a specific model;
the product is the assistant, and the models are its
interchangeable, verifiable fuel.
