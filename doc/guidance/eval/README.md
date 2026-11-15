# Guidance evaluation corpus

This directory holds natural-language evaluation cases for the `guidance` read
plane. It is an input to `guidance benchmark` and the scorecard, never a
retrieval source. `guidance sync`, the doc scanner, and capability imports
must exclude `doc/guidance/eval/**`.

## Format

One JSON object per line:

```json
{"id":"owner-calibration","intent":"ownership","query":"Which crate owns calibration?","golden_files":["src/common-core/src/calibration.rs"],"golden_facts":["common-core","CalibrationRegistry"],"required_sections":["From the project map"],"split":"tune"}
```

- `id` — stable, unique.
- `intent` — orientation, ownership, graph, mechanism, provenance, ranking, or general.
- `query` — the exact question.
- `golden_files` — files an orchestrator must end up reading (basenames are compared).
- `golden_facts` — facts a correct answer must contain.
- `required_sections` — sections a correct answer must render.
- `split` — `tune` or `heldout`.
- `control` — an out-of-corpus case that must stay silent (no answer, no
  confident citation).

## Rules

- Tune and held-out are separate. Do not iterate on held-out cases.
- A case that cannot be answered from the repository must be a control, not a
  guessed golden.
- Goldens are human-authored against the source tree; model output is never a
  label.
- The benchmark must not index this directory. The corpus is read directly.
- Keep `golden_facts` short and falsifiable. File-level recall alone is not a
  correctness signal.
