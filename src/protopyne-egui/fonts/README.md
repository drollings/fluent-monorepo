# Terminal fonts

`NotoSans-Regular.ttf` (proportional primary) and `NotoSansMono-Regular.ttf`
(monospace primary plus proportional fallback for arrows and geometric
shapes) are the Noto Sans files Google ships under the SIL Open Font
License 1.1.

They are embedded at compile time through `include_bytes!` in the
`demo`/`terminal` examples only — no `src/` module embeds a binary blob —
and installed ahead of egui's built-in fallbacks by
`protopyne_egui :: install_noto_sans` over the `fluent_egui :: fonts`
fallback-chain policy.
