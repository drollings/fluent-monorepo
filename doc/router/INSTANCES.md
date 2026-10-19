# Coral Router: /instances API - shared-weight context instances

Coral Router is a local-first control plane that serves one OpenAI-compatible
endpoint. It spawns and supervises one `llama-server` process per model weights
file, owns all VRAM policy and orchestration, and exposes the `/instances`
management API at its own address as an addition to that OpenAI endpoint. This
document specifies that API and the semantics of the multiple context windows
("instances") and KV-cache snapshots Coral Router manages.

The upstream llama.cpp router mode is NOT used. Coral Router is the only
orchestrator. It never reaches llama.cpp's router; it talks directly to the
llama-server processes it owns.

## 1. Relationship to llama-server

- One `llama-server` process per `(base, variant)` weights file, spawned by
  Coral Router on a free localhost port.
- Each llama-server loads its model's weights once and allocates several named
  contexts (instances) from them. Every instance is an independent context
  window of exactly its own `ctx_size`; `parallel` slots SHARE that one window
  and never multiply or divide it. Weights are loaded once; instances never
  divide a context window among each other. The exact mapping is in
  `LLAMA_INSTANCES.md` section 4.6: to run N full-size contexts, create N
  instances (`count: N`), not `parallel: N` slots.
- The llama-server exposes the same `/instances` management contract as a
  low-level primitive (see `LLAMA_INSTANCES.md`). Coral Router is its only
  caller.
- Coral Router's public `/instances` mirrors that contract: same envelope,
  same id grammar, same operations. Existing sidecar tooling carries over
  unchanged.

### 1.1 Model id translation

Coral Router config keys are the public model ids; llama.cpp model names are
internal. For the reference config (`env/coral-router.json`):

| Coral Router model id | llama-server model name |
| --- | --- |
| `lfm2.5-1.2b` | `unsloth/lfm2.5-1.2b-instruct` |
| `swarm` | `abiray/lfm2.5-2.6b-heretic-abliterated` |
| `qwen3.6-27b` | `vinpix/ternary-bonsai-27b-stock-mtp` |
| `gemma4-12b` | `mradermacher/limn-alpha-12b-i1` |
| `embed` | `google/embeddinggemma-300m` |

Clients always use the model id. Coral Router translates to the owning server
when proxying a request or a management call. Instance ids are
`<model_id>:<instance_name>`.

## 2. The envelope (GET /instances)

`GET /instances` lists every running instance across every managed model,
plus the pool's on-disk KV snapshots. The shared weights of each model are
counted once in `total`.

```json
{ "instances": [
    { "id": "swarm:ledger",  "aliases": ["swarm", "swarm:ledger", "swarm:latest"],
      "group": "ledger", "n_ctx": 65536, "parallel": 1, "pinned": true,
      "is_default": true, "state": "loaded",
      "model_bytes": 2428416000, "context_bytes": 2148925440,
      "compute_bytes": 2220361792, "total_bytes": 6797703232,
      "vram_bytes": 4369287232, "last_used": 1699999999 },
    { "id": "swarm:scratch", "aliases": ["swarm:scratch"],
      "group": "scratch", "n_ctx": 131072, "parallel": 1, "pinned": false,
      "is_default": false, "state": "loaded",
      "model_bytes": 0, "context_bytes": 2148925440,
      "compute_bytes": 2220361792, "total_bytes": 4369287232,
      "vram_bytes": 4369287232, "last_used": -1 }
  ],
  "snapshots": [ { "model": "swarm", "name": "readfiles", "size": 4194304,
                   "mtime": 1699999999, "n_ctx_seq": 131072 } ],
  "total": { "model": 2428416000, "context": 4297850880,
             "compute": 4440723584, "total": 11166990464 } }
```

Per-instance fields:

| Field | Meaning |
| --- | --- |
| `id` | `<model_id>:<instance_name>` |
| `aliases` | the bare model id, `latest` forms, group name, and the exact id |
| `group` | group name; `model:group` routes to the first available member |
| `n_ctx` | context size of this instance |
| `parallel` | slots within this instance |
| `pinned` | advisory eviction-exemption flag (see 5.3) |
| `resume` | router-side preserve-on-evict flag (see 3.6): the context's KV is
  snapshotted before it is dropped so a later `snapshot=<name>-resume`
  request restores it |
| `is_default` | target of a bare `<model_id>` request |
| `state` | always `"loaded"`; instances exist only while allocated |
| `model_bytes` | shared weights, reported on the first loaded instance only (0 elsewhere) |
| `context_bytes` | this instance's KV buffer |
| `compute_bytes` | this instance's compute buffers |
| `total_bytes` | `model + context + compute` |
| `vram_bytes` | alias of `context_bytes + compute_bytes` (excludes shared weights) |
| `last_used` | most recent slot use (max `t_last_used` over slots), `-1` if unused |

`total` is summed with 64-bit arithmetic. `model` counts each model's weights
once, regardless of how many of its instances are loaded.

`state` is always `"loaded"` because unloading is imperative: an instance is
either allocated (and immediately ready) or deleted. There is no sleeping
state. A deleted instance vanishes from the list.

`GET /instances` is the single memory surface. There is no separate
`/memory`; a `/memory` compatibility route, when present, reshapes the same
envelope.

Query parameter `?model=<model_id>` scopes the response to one model.

## 3. Operations

All management endpoints require the API key when `api_key_env` is set. All
changes run as server tasks on the owning instance's scheduler thread and
never race generation.

Routing: every operation names a model. A call that names no model routes to
the single running server (400 "cannot route instance management request:
specify 'model'" when there is more than one or none). Per-instance operations
accept either `?model=<model_id>` or an `id` in the form `<model_id>:<name>`.

| Endpoint | Purpose |
| --- | --- |
| `GET /instances` | list instances + snapshots + summed memory (2) |
| `POST /instances` | allocate a NEW context from a model's shared weights |
| `POST /instances/:name/pin` | set advisory `pinned = true` |
| `POST /instances/:name/unpin` | set advisory `pinned = false` |
| `POST /instances/:name/resize` | destroy and re-create the context at a new size |
| `DELETE /instances/:name` | free the instance's KV + compute; if it was the last, the shared weights unload |
| `POST /instances/:name/snapshot` | save the instance's slot 0 KV to disk; the slot binds to it |
| `GET /instances/:name/snapshots` | list the model's snapshots |
| `DELETE /instances/:name/snapshot/:snapshot` | delete a snapshot file and unbind |

### 3.1 POST /instances

Body:

```json
{ "model": "swarm", "name": "work", "group": "swarm",
  "ctx_size": 32768, "parallel": 1, "pinned": false, "default": false,
  "resume": false }
```

- `model` selects the pool. If the model's server is not running, Coral Router
  auto-loads it (spawn + weights load) before creating.
- `name` and `group` are restricted to `[A-Za-z0-9._-]`; a name or group
  outside that set is rejected with `400` (it would break the id grammar).
- `ctx_size`: the instance's context window, allocated exactly; `0` inherits
  the model's configured default. Never multiplied or divided by `parallel`.
- `parallel`: slots SHARING that one window; default `1`. The global
  `--parallel` value is never assumed. With `parallel > 1` the window stays
  `ctx_size` and each slot is capped at `ctx_size/parallel` of KV; to get N
  full-size windows create N instances (`count: N`), not `parallel: N` (see
  `LLAMA_INSTANCES.md` 4.6).
- `pinned`: advisory flag, stored and reported only (5.3).
- `resume`: router-side flag, stored and reported only (3.6). The fork never
  sees it; Coral Router uses it to decide whether to snapshot the context
  before eviction.
- `default`: marks the target of a bare `<model_id>` request.

Returns `201` + the new instance object. `409` on duplicate name or a
name/group collision. `507` on insufficient device memory (weights stay
loaded, nothing aborts). `400` on an invalid name.

### 3.2 DELETE /instances/:name

`DELETE /instances/:name?model=swarm`. Returns `200 {"success": true}`.

The instance's KV + compute are freed. If it was the last instance of the
model, the shared weights unload: `model_bytes` and `total.model` fall to 0,
`instances` becomes empty, and `snapshots` stays populated so a later request
can re-warm and restore. `pinned` is not enforced (see 5.3); `?force=true`
is accepted for compatibility and ignored.

After the last instance is deleted Coral Router MAY retire the server process
(it is the process owner) to reclaim the process overhead, or keep it for a
cheap warm recreate. The weights are freed in either case.

### 3.3 POST /instances/:name/resize

Body `{ "ctx_size": 49152 }`. Drains in-flight tasks, then destroys and
re-creates the context at the new size. The current KV and any snapshot
binding are discarded. Returns `200` + the resized instance object.

### 3.4 pin / unpin

`POST /instances/:name/pin?model=...` and `/unpin`. These only set and report
the advisory `pinned` flag (5.3). They are kept for contract continuity with
the previous branch and because the reference config declares `pinned: true`
on `swarm` and `ledger`.

### 3.5 snapshots

- `POST /instances/:name/snapshot` body `{ "name": "readfiles" }` saves the
  instance's slot 0 KV to the model's snapshot directory and binds the slot to
  it. `400`/`501` when no snapshot directory is configured.
- `GET /instances/:name/snapshots` returns `{ "snapshots": [{name, size,
  mtime, n_ctx_seq}] }`.
- `DELETE /instances/:name/snapshot/:snapshot` deletes the file (and any bound
  slot's reference to it). Returns `200`.

Snapshot files live under `<slot_save_path>/<model_key>/`, where `model_key`
is the sanitized llama.cpp model name (`/` and `:` replaced by `_`), so
snapshots never collide across models. Snapshot names are validated with the
same character class as a slot-save filename (`[A-Za-z0-9._-]+`); an unsafe
name is `400`.

`snapshots` in the `GET /instances` envelope lists a model's on-disk
snapshots even when the model has no loaded instance, each tagged with its
owning `model`, so the sidecar can route a reactivation
(`POST /instances` to re-warm, then a `snapshot=X` request) to the right
server. Snapshot ids are NOT models and never appear in `/v1/models`.

### 3.6 resume (preserve-on-evict)

`resume` is a router-side flag Coral Router tracks and overlays on the
envelope (the fork knows nothing of it; nothing is forwarded to llama-server).
It means: "if this context must be evicted to free VRAM, snapshot its KV cache
first so the work can continue later."

- A context with `resume = true` that the router evicts is first saved via
  `POST /instances/:name/snapshot` under the deterministic name
  `<instance>-resume`. The session transcript is already durable in Coral
  Router's ledger, so the two together (KV + text log) preserve the workload.
- A later request to the same context with `snapshot=<instance>-resume` (and
  the same `session_id` for the ledger) restores and continues it.
- `resume` is cleared - and the `<instance>-resume` snapshot deleted - when
  Coral Router concludes the work is done:
  - explicitly: `POST /instances/:name/no-resume`; or
  - automatically: the sidecar clears any resume-marked context idle longer
    than `sidecar.resume_ttl_s` each residency pass.
- `pinned` contexts are never evicted, so `resume` is moot on them.
- Snapshotting is best-effort: a context with no configured
  `sidecar.slot_save_path` (or a failed save) drops unsnapshotted, logged at
  warn, and the eviction always proceeds.

Control endpoints:

| Endpoint | Purpose |
| --- | --- |
| `POST /instances/:name/resume` | set `resume = true` (preserve on evict) |
| `POST /instances/:name/no-resume` | set `resume = false` and delete the `<name>-resume` snapshot |

## 4. Request routing on the OpenAI endpoint

Every generation endpoint (`/v1/chat/completions`, `/v1/completions`, infill,
embeddings, rerank, transcribe, responses, Anthropic messages and token
count) accepts four routing fields, from BOTH the JSON body and the query
string (the body wins):

- `model` - the Coral Router model id, optionally suffixed with an instance or
  group: `<model_id>`, `<model_id>:<instance>`, `<model_id>:<group>`,
  `<model_id>:latest`. Empty selects the pool's default instance (400
  "ambiguous" when there is no default and more than one instance).
- `instance` - an instance or group; overrides any instance component of
  `model`.
- `snapshot` - the KV snapshot to make current in the selected slot before
  serving (see 4.1).
- `id_slot` - the slot to target for a snapshot switch; defaults to 0.

Coral Router resolves the model id to its server, strips the model id prefix
from the request, and forwards the remainder plus `instance` / `snapshot` /
`id_slot` to the owning llama-server, which resolves the instance from its own
config. Group "first available" and the group wait (`--instance-wait`) are
decided by the llama-server, never duplicated in Coral Router.

Endpoints with no `model` (`/tokenize`, `/detokenize`, `/apply-template`,
`/props`, `/health`, `/control`, LoRA adapters) run on the pool's default
instance. `/props` adds `total_slots` and an `instances` array on top of the
default instance's settings.

`/v1/models` and `/models` list one entry per instance, plus aliases for the
bare model, group, `latest`, and variants, including `n_ctx`, `parallel`,
`pinned`, `state`, and `last_used`. No auto-generated parameter suffixes ever
appear.

### 4.1 Snapshot restore

A request with `snapshot=X` switches the target instance's slot to snapshot
`X` before it is served. If the slot is already bound to `X` it serves
immediately (no re-prefill); otherwise the slot's current KV is saved back to
its bound snapshot (if any) and `X` is loaded. An unbound working context is
discarded on a switch.

A request without `snapshot` leaves the slot as-is (bound snapshot or live
working context). A request with a different `snapshot` unbinds the previous
one. Bindings are invalidated when the instance is deleted, not by ordinary
requests, so a conversation can continue across snapshot switches.

Errors: missing snapshot -> `404` `not_found_error`; restore failure (size
mismatch, no KV space) -> `400`/`409` with the instance left untouched; no
snapshot directory configured -> `400`.

## 5. Sidecar and eviction policy

VRAM intelligence lives entirely in Coral Router. The llama-server never
auto-unloads anything.

### 5.1 The sidecar component

The `sidecar` section of `env/coral-router.json` configures the internal
eviction loop:

```json
{ "poll_interval_s": 5, "vram_low_watermark_bytes": 1073741824,
  "evict_batch": 1, "vram_total_bytes": null,
  "slot_save_path": "/srv/slots", "resume_ttl_s": 3600, "api_key_env": null }
```

The sidecar:

1. Polls `GET /instances` every `poll_interval_s`. The envelope's `total` and
   per-instance `vram_bytes` are the VRAM signal. `vram_total_bytes = null`
   means "detect total VRAM from the host".
2. When the aggregate usage exceeds the allocation budget
   (`device_total - minimum_remaining_vram`), it evicts up to `evict_batch`
   units per poll.
3. The eviction target is the **largest coldest resident footprint** (a
   `freed_bytes x idle-time` score): a whole model with no pinned instances -
   its weights plus every context - outranks a handful of context buffers, so
   OOM pressure reclaims the big chunks (e.g. a 10.5 GB weight pool) instead
   of only trimming contexts. Pinned instances are never candidates; a model
   with any pinned context keeps its weights resident. Among equal scores the
   coldest unit goes first.
4. A resume-marked context is snapshotted (`<name>-resume`) before it drops;
   resume contexts idle past `resume_ttl_s` are concluded first (flag cleared,
   snapshot deleted).
5. When a request needs an instance that was evicted, `POST /instances`
   allocates fresh KV without reloading weights when the model is still
   resident, or re-warms a cold model automatically. A resume-marked evicted
   context is restored by dispatching with `snapshot=<name>-resume`.

### 5.2 Warm models

A model with a `pinned` instance, or with no instances, stays warm by policy:
the sidecar never evicts a pinned instance, and a resident model with zero
instances costs only the weights (instances are cheap to re-add). The sidecar
chooses to retire the process only when it must reclaim the weights.

### 5.3 What `pinned` means here

`pinned` is an advisory flag Coral Router enforces in its OWN eviction policy;
it has no meaning inside llama-server (nothing there auto-evicts or
auto-sleeps). Coral Router sets it from the config's `pinned: true`, reports
it in the envelope, and refuses to evict pinned instances. It also carries
`pinned` when creating instances so the flag survives in the envelope.

### 5.4 Config mapping at boot

`models.<id>.instances` is the declarative instance spec. Coral Router
materializes it with `POST /instances` at boot:

```json
"instances": {
    "swarm":   { "count": 2, "group": "swarm", "num_ctx": 8192,   "pinned": true },
    "ledger":  { "num_ctx": 65536, "pinned": true, "default": true },
    "scratch": { "num_ctx": 131072, "sleep_idle_seconds": 1 }
}
```

- A spec with `count: N` creates N instances named `<key>-0` .. `<key>-N-1`
  with the given `group`. Each of those N instances is its OWN full-size
  window of `num_ctx`; `count` is how you get N concurrent full-size contexts,
  it is not a substitute for `parallel` slots.
- A spec without `count` creates one instance named `<key>`, `group` defaulting
  to `<key>`.
- `num_ctx` is the window of each created instance, allocated exactly; it is
  never multiplied or divided by any `parallel` value in the spec (per
  `LLAMA_INSTANCES.md` 4.6).
- `pinned` maps to the advisory flag; `default: true` maps to `is_default`.
- `sleep_idle_seconds` is NOT forwarded to llama-server. It becomes an
  eviction-priority hint: the sidecar treats a short idle timeout as "this
  instance is a good eviction candidate" (`scratch` is evicted before `ledger`
  or the `swarm` workers).

## 6. Errors

Management endpoints return OpenAI-shaped errors:

```json
{ "error": { "code": 404, "message": "instance not found: 'nope'",
             "type": "not_found_error" } }
```

| Code | Meaning | Sidecar handling |
| --- | --- | --- |
| `409` | duplicate name / name-group collision | idempotent during reconcile |
| `503` | transient (group full, server starting) | retry / allocation trigger |
| `507` | insufficient device memory | eviction + allocation trigger |
| `404` | unknown instance or snapshot | mark unknown |
| `400` | invalid name/group/body | configuration bug |

## 7. Security

When `sidecar.api_key_env` names an environment variable, that key is
required on every `/instances` and snapshot endpoint (`401` otherwise). Coral
Router forwards the same key on its internal calls to llama-server. The
llama-servers bind to `127.0.0.1` only and are never exposed directly.
