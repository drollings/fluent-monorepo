# WER corpus contract

Each case is a pair: `name.wav` (16 kHz mono PCM) plus `name.txt`
(the exact reference transcript, one line). The live runner
(`tests/live.rs::live_wer_per_tier`, `#[ignore]` + `live-ai`) decodes
every pair with every pinned whisper tier present on disk and reports
WER per tier. Missing models or an empty corpus skip cleanly — never
fail.

Recording guidance: quiet room, native speaker, read the sentence
once, trim leading/trailing silence to under 200 ms. Reference text
is lowercase, no punctuation (matches whisper normalization).
