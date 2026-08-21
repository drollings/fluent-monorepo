#!/usr/bin/env python3
"""
coral-router performance harness.

Scores a running coral-router on three axes, 0-10 each, and prints a score
matrix plus a JSON report:

  * ROUTING ACCURACY   did each route's prompt reach the expected model group?
                       expectation derived from env/coral-router.json
                       (routes -> model_groups -> cheapest qualifying model).
  * SPEED (TTFT)       wall time from request send to the first streamed
                       token ("beginning of inference") for the routed model,
                       scored against that model's configured total_timeout.
  * VRAM EFFICIENCY    used VRAM (aggregate /instances) vs the residency
                       budget (device_total - minimum_remaining_vram), and
                       how much of the configured fleet stays resident when it
                       has not been asked to serve.

The harness reads ./env/coral-router.json (override with --config) so every
route declared there is exercised automatically and the expected target model
per route is derived from the config, not hardcoded.

Usage:
    bin/coral-router-test.py                 # against http://127.0.0.1:8079
    bin/coral-router-test.py --base-url http://127.0.0.1:8079 --config env/coral-router.json
    bin/coral-router-test.py --json report.json
    bin/coral-router-test.py --warmup 1 --ttft-timeout 60

Exit status: 0 on success (all routes answered), 1 if a route hard-failed,
2 on harness/config error.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import statistics
import sys
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional, Tuple

DEFAULT_CONFIG = os.path.join(os.path.dirname(__file__), "..", "env", "coral-router.json")
DEFAULT_BASE = "http://127.0.0.1:8079"

# Prompt seeds per route, keyed by route name. These are demanding probes whose
# domain matches each route's description in the config: simple enough that a
# healthy router dispatches them, complex enough that they must reach the
# group's target model rather than being answered by the classifier directly.
# The harness also prints the config-derived description for operator sanity.
ROUTE_PROMPTS = {
    "local": "What is the capital of France? Answer in one short sentence.",
    "prose": "Write a 400-word gothic short story about a lighthouse keeper who discovers his light is powered by the souls of drowned sailors. Include dialogue and a dramatic climax.",
    "code": "Write a complete Rust program using the rayon crate that parses a CSV of numbers, computes the sum in parallel, and prints both the total and the count of rows.",
    "extract": "From the following email, extract every date, dollar amount, product name, and person name as structured JSON: 'Hi Sam, the Q3 invoice for the Nebula server upgrade is $12,400. Please wire it by October 15, 2025. Regards, Priya Patel.'",
    "summarize": "Provide a structured two-sentence executive summary of this quarterly report, including the revenue figure and the key risk: 'Q3 revenue reached $4.2M, up 12% YoY, driven by the Europe expansion. The principal risk is component supply chain delays from the Taiwan foundry, which may impact Q4 shipments by up to 15%.'",
    "translation": "Translate the following business contract clause into Japanese, preserving legal precision: 'The party shall be liable for consequential damages arising from gross negligence, subject to a limitation of liability cap of one million dollars.'",
    "science": "Explain the EPR paradox and how Bell's theorem rules out local hidden variables, being precise about the CHSH inequality and what a Bell-inequality-violating experiment demonstrates.",
    "legal": "Draft a confidentiality clause for a software licensing agreement that covers the definition of confidential information, permitted disclosures to counsel and subcontractors bound by similar terms, and a three-year survival obligation after termination.",
    "medical": "Describe the mechanism of action, indications, contraindications, and the most serious adverse effects of metformin, including the circumstances under which lactic acidosis is a clinical concern.",
}

# Family of "already warm" probes used for the warm TTFT score (models that
# booted pinned / were exercised by the route sweep). Warm = resident now.
WARM_PROMPT = "Say the word 'ready' and nothing else."

# Default time to let the non-blocking audit writer flush before reading the
# durable audit log after the route sweep (the live Needle attribution reads it).
DEFAULT_AUDIT_SETTLE_S = 1.0


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def load_config(path: str) -> Dict[str, Any]:
    with open(path) as fh:
        return json.load(fh)


def base_url_from_config(cfg: Dict[str, Any], override: Optional[str]) -> str:
    if override:
        return override.rstrip("/")
    bind = cfg.get("server", {}).get("bind_addr", "127.0.0.1:8079")
    host, _, port = bind.rpartition(":")
    if not port.isdigit():
        raise ValueError(f"unparseable server.bind_addr in config: {bind!r}")
    if not host or host in ("0.0.0.0", "::"):
        host = "127.0.0.1"
    return f"http://{host}:{port}"


def derive_expectations(cfg: Dict[str, Any]) -> Dict[str, Any]:
    """Build the per-route expectation table from the config alone.

    For each route: expected group + the ordered model ladder that group can
    dispatch to, and the *primary* target (cheapest qualifying model — the one
    the router prefers for a low-complexity prompt). The full ladder is the
    acceptance set: any member counts as correct routing; the primary is the
    10/10 outcome.
    """
    routes = cfg.get("routes", {})
    groups = cfg.get("model_groups", {})
    models = cfg.get("models", {})
    expectations = {}
    for route, rref in routes.items():
        group = rref.get("group", route)
        ladder = groups.get(group, [])
        # Primary = cheapest qualifying model in the group (the router resolves
        # low-complexity prompts to the cheapest model whose intelligence
        # meets the complexity; a simple probe therefore expects the cheapest).
        def cost(key: str) -> float:
            m = models.get(key, {})
            return float(m.get("cost_input", 0.0)) + float(m.get("cost_output", 0.0))

        ordered = sorted(ladder, key=cost) if ladder else []
        expectations[route] = {
            "group": group,
            "ladder": ordered,
            "primary": ordered[0] if ordered else None,
            "description": rref.get("description", ""),
            "always_route": bool(rref.get("always_route", False)),
        }
    return expectations


def needle_schema_overrides(cfg: Dict[str, Any]) -> Dict[str, Any]:
    """The `needle.schema_overrides` map (route key -> tool schema)."""
    return cfg.get("needle", {}).get("schema_overrides", {})


def is_general_route(cfg: Dict[str, Any], route: str) -> bool:
    """Whether a route is a `general` category (needle.schema_overrides.*.general).

    A general category is a classifier decision by design (roadmap Milestone 3
    guardrail): Needle declines it, so it must never be scored as a Needle miss.
    """
    return bool(needle_schema_overrides(cfg).get(route, {}).get("general", False))


def audit_dir_from_config(cfg: Dict[str, Any]) -> str:
    """The durable audit log dir the router writes `kind = "route"` records to."""
    return cfg.get("logging", {}).get("audit_log", {}).get("log_dir", "logs/audit")


def load_audit_records(audit_dir: str) -> List[Dict[str, Any]]:
    """Parse the durable `router.audit` JSON-lines files into route records.

    Each line is a tracing JSON record of the shape
    `{"fields": {"kind": "route", "detail": "<json string>"}}`; `detail` is the
    emitted audit payload (roadmap Milestone 4/5). Only `kind = "route"` records
    whose `detail.stage` is a deciding stage (needle/classifier/none) are kept —
    the per-request aggregate routing-source records the scorer attributes on.
    """
    if not os.path.isdir(audit_dir):
        return []
    records: List[Dict[str, Any]] = []
    for fname in sorted(os.listdir(audit_dir)):
        path = os.path.join(audit_dir, fname)
        if not os.path.isfile(path):
            continue
        try:
            with open(path) as fh:
                for line in fh:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        obj = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if not isinstance(obj, dict):
                        continue
                    fields = obj.get("fields", {})
                    if not isinstance(fields, dict) or fields.get("kind") != "route":
                        continue
                    raw = fields.get("detail")
                    if not raw:
                        continue
                    try:
                        detail = json.loads(raw) if isinstance(raw, str) else raw
                    except json.JSONDecodeError:
                        continue
                    if (
                        isinstance(detail, dict)
                        and detail.get("stage") in ("needle", "classifier", "none")
                    ):
                        records.append(detail)
        except OSError:
            continue
    return records


def attribute_decisions(
    records: List[Dict[str, Any]], cfg: Dict[str, Any]
) -> Dict[str, Dict[str, Any]]:
    """Map each configured route to its deciding aggregate record (last wins).

    The harness probes each route warmup-first then scores it, so the last
    aggregate record naming a route is that route's scored-request decision.
    General routes are attributed to the classifier regardless (roadmap
    guardrail: Needle never decides a general category), even on a misconfig
    that routed one.
    """
    per_route: Dict[str, Dict[str, Any]] = {}
    for rec in records:
        stage = rec.get("stage")
        if stage not in ("needle", "classifier"):
            continue
        route = rec.get("route") or rec.get("tool") or rec.get("target_route")
        if not route:
            continue
        if stage == "needle" and is_general_route(cfg, route):
            rec = dict(rec)
            rec["stage"] = "classifier"
            rec["verdict"] = "fallback"
        per_route[route] = rec
    return per_route


def needle_metrics(
    records: List[Dict[str, Any]],
    cfg: Dict[str, Any],
    exp_table: Dict[str, Dict[str, Any]],
) -> Dict[str, Any]:
    """Needle coverage / routing accuracy / direct-response rate (Milestone 5).

    Computed from the durable audit aggregate records keyed per route.

    * **coverage** — share of non-general routes decided by Needle.
    * **routing accuracy** — among Needle-decided routes, the share that
      dispatched through the route's configured `model_group` (a direct
      template response is inherently correct — it answered deterministically).
    * **direct-response rate** — share of Needle-decided routes answered
      directly from an `output_template`.

    General routes are excluded from every metric (the classifier decides them
    by design — never a Needle miss).
    """
    per_route = attribute_decisions(records, cfg)
    non_general = [r for r in exp_table if not is_general_route(cfg, r)]
    needle_decided = [
        r for r in non_general if per_route.get(r, {}).get("stage") == "needle"
    ]
    total = len(non_general)
    coverage = len(needle_decided) / total if total else 0.0

    correct = 0
    direct = 0
    for r in needle_decided:
        rec = per_route[r]
        if rec.get("verdict") == "direct_response":
            direct += 1
            correct += 1
        elif rec.get("group") == exp_table.get(r, {}).get("group"):
            correct += 1

    decided = len(needle_decided)
    accuracy = correct / decided if decided else 0.0
    direct_rate = direct / decided if decided else 0.0
    return {
        "needle_coverage": round_half(coverage),
        "needle_routing_accuracy": round_half(accuracy),
        "needle_direct_response_rate": round_half(direct_rate),
        "needle_decided_routes": decided,
        "non_general_routes": total,
        "audit_records_parsed": len(records),
    }


def print_needle_metrics(
    m: Dict[str, Any],
    cfg: Dict[str, Any],
    exp_table: Dict[str, Dict[str, Any]],
    per_route: Optional[Dict[str, Dict[str, Any]]] = None,
) -> None:
    """Print the Needle metric block (live and --audit-only share this)."""
    coverage = m["needle_coverage"]
    accuracy = m["needle_routing_accuracy"]
    direct = m["needle_direct_response_rate"]
    decided = m["needle_decided_routes"]
    non_general = m["non_general_routes"]
    print("=" * 78)
    print("NEEDLE ROUTING  (deciding stage attributed from the durable audit)")
    print("=" * 78)
    general_routes = sorted(r for r in exp_table if is_general_route(cfg, r))
    if general_routes:
        print(f"general (classifier-decided, excluded): {', '.join(general_routes)}")
    per_route = per_route or {}
    for route in sorted(exp_table):
        rec = per_route.get(route)
        if rec is None:
            stage = "classifier"
        else:
            stage = rec.get("stage", "classifier")
            if is_general_route(cfg, route):
                stage = "classifier"
        verdict = (rec or {}).get("verdict", "—")
        mark = "  [general → classifier]" if is_general_route(cfg, route) else ""
        print(f"  {route:<11} decided_by={stage:<10} verdict={verdict:<16}{mark}")
    print("-" * 78)
    print(
        f"Needle coverage:              {coverage:5.2f}  "
        f"({decided}/{non_general} non-general routes)"
    )
    print(f"Needle routing accuracy:      {accuracy:5.2f}  (correct model_group dispatch)")
    print(f"Needle direct-response rate:  {direct:5.2f}  (output_template answers)")
    print(f"Audit records parsed:         {m['audit_records_parsed']}")
    print("=" * 78)


def device_vram_total(cfg: Dict[str, Any]) -> Optional[int]:
    """Residency device ceiling: sidecar.vram_total_bytes, else ROCm sysfs."""
    sidecar = cfg.get("sidecar", {})
    total = sidecar.get("vram_total_bytes")
    if total:
        return int(total)
    # ROCm exposes the total under /sys/class/drm/card*/device/.
    base = "/sys/class/drm"
    if not os.path.isdir(base):
        return None
    for name in sorted(os.listdir(base)):
        path = os.path.join(base, name, "device", "mem_info_vram_total")
        try:
            with open(path) as fh:
                val = int(fh.read().strip())
            if val > 0:
                return val
        except (OSError, ValueError):
            continue
    return None


def min_remaining_vram(cfg: Dict[str, Any]) -> int:
    return int(cfg.get("sidecar", {}).get("minimum_remaining_vram", 0) or 0)


def http_json(url: str, body: Optional[Dict[str, Any]] = None, timeout: float = 30.0) -> Tuple[int, Any]:
    """Issue a request and return (http_status, parsed_json_or_text)."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url, data=data, method="POST" if body is not None else "GET",
        headers={"Content-Type": "application/json", "Accept": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode("utf-8", "replace")
            try:
                return resp.status, json.loads(raw)
            except json.JSONDecodeError:
                return resp.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode("utf-8", "replace")
        try:
            return e.code, json.loads(raw)
        except json.JSONDecodeError:
            return e.code, raw
    except (urllib.error.URLError, socket.timeout, ConnectionError) as e:
        return 0, {"error": str(e)}


def stream_ttft(url: str, body: Dict[str, Any], timeout: float) -> Tuple[Optional[float], int, str]:
    """POST a streaming chat request; return (ttft_seconds, status, model).

    ttft is the wall time from request send to the first SSE `data:` line
    (the beginning of inference). `model` is the routed model reported by the
    server (llama name, may carry `:instance`).
    """
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        url, data=data, method="POST",
        headers={"Content-Type": "application/json", "Accept": "text/event-stream"},
    )
    start = time.monotonic()
    ttft: Optional[float] = None
    status = 0
    model = ""
    tail = ""
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            status = resp.status
            # Read line by line; the first `data:` chunk is the start of output.
            for line in resp:
                if ttft is None and line.startswith(b"data:"):
                    ttft = time.monotonic() - start
                tail += line.decode("utf-8", "replace")
                if len(tail) > 1_000_000:  # safety cap
                    break
    except urllib.error.HTTPError as e:
        status = e.code
        tail = e.read().decode("utf-8", "replace")
    except (urllib.error.URLError, socket.timeout, ConnectionError) as e:
        status = 0
        tail = str(e)
    if ttft is None:
        ttft = time.monotonic() - start
    # Best-effort model extraction from the streamed body.
    for line in tail.splitlines():
        if line.startswith("data:") and line != "data: [DONE]":
            try:
                obj = json.loads(line[5:])
                if obj.get("model"):
                    model = obj["model"]
                    break
            except json.JSONDecodeError:
                continue
    return ttft, status, model


def normalize_model(model: str, cfg: Dict[str, Any]) -> Optional[str]:
    """Map a served llama model id back to a config model key.

    The server reports the llama name (e.g. `abiray/lfm2.5-2.6b-heretic-...`,
    possibly `:instance`-qualified). We match it against each model entry's
    `name`. Returns the config key or None when unmanaged/unmatched.
    """
    base = model.split(":", 1)[0].strip()
    for key, entry in cfg.get("models", {}).items():
        if entry.get("name") == base:
            return key
    # Fall back: the config key itself may equal the reported base.
    if base in cfg.get("models", {}):
        return base
    return None


def served_as(served: str, route: str, cfg: Dict[str, Any]) -> Tuple[Optional[str], bool]:
    """Classify who actually served a route request.

    Returns (served_key, direct_response). A streamed response whose `model`
    equals the requested route name (or is empty) means the pipeline's
    classifier answered the prompt directly and NO target model was dispatched;
    that response is attributed to the configured `classifier_model`.
    """
    key = normalize_model(served, cfg)
    direct = served.strip() == route or served.strip() == ""
    if direct:
        classifier = cfg.get("pipelines", {})
        ck = None
        for p in classifier.values():
            if isinstance(p, dict) and p.get("classifier_model"):
                ck = p.get("classifier_model")
                break
        return ck, True
    return key, False


def score_routing(served_key: Optional[str], exp: Dict[str, Any], classifier_key: Optional[str]) -> float:
    """0-10: 10 primary, 9 other member of the route's ladder, 3 wrong group,
    0 unmanaged/unresolved. A *direct classifier response* (no dispatch) is
    attributed to the configured classifier model: if that model is the route's
    primary target it is a perfect cheap answer (10); if it is another ladder
    member it is still on-domain (9); otherwise the request never reached the
    group's model (3)."""
    if served_key is None:
        return 0.0
    if served_key == exp.get("primary"):
        return 10.0
    if served_key in exp.get("ladder", []):
        return 9.0
    if served_key is not None and served_key == classifier_key:
        # The classifier answered directly but is not a member of this route's
        # group: the request did not reach the expected target model.
        return 3.0
    return 3.0


def score_speed(ttft: float, served_key: Optional[str], cfg: Dict[str, Any]) -> float:
    """0-10 relative to the served model's total_timeout_ms (the router's own
    latency contract for that model). A model that starts output well within
    its timeout scores high; one that burns most of it scores low."""
    entry = cfg.get("models", {}).get(served_key or "", {})
    budget_s = float(entry.get("total_timeout_ms", 60_000)) / 1000.0
    if budget_s <= 0:
        budget_s = 60.0
    # Clamp pathological cases.
    ttft = min(max(ttft, 0.0), budget_s * 10)
    return max(0.0, min(10.0, 10.0 * (1.0 - ttft / budget_s)))


def score_vram(used: int, device_total: Optional[int], min_rem: int) -> float:
    """0-10 VRAM efficiency.

    10 when used is at/under the budget (device_total - minimum_remaining)
    with headroom; grades down as used approaches the budget from below and
    hits 0 when the budget is exceeded (residency failed to hold its floor).
    Unknown device total (no ceiling, no ROCm) is scored 5 with a note.
    """
    if device_total is None:
        return 5.0
    budget = max(device_total - min_rem, 1)
    ratio = used / budget
    if ratio <= 0.9:
        return 10.0
    if ratio <= 1.0:
        # Between 90% and 100% of budget: squeeze remaining headroom.
        return 10.0 - (ratio - 0.9) / 0.1 * 5.0
    # Over budget: 0 at 110%+, linear decay between 100% and 110%.
    return max(0.0, 10.0 - (ratio - 1.0) / 0.1 * 10.0)


def instances_aggregate(base: str) -> Tuple[Optional[int], List[Dict[str, Any]], int]:
    """GET /instances; return (used_total, instance_list, http_status)."""
    status, data = http_json(f"{base}/instances")
    if status != 200 or not isinstance(data, dict):
        return None, [], status
    total = data.get("total", {})
    used = int(total.get("total", 0)) if isinstance(total, dict) else 0
    return used, data.get("instances", []), status


def round_half(x: float) -> float:
    return round(x, 2)


def main() -> int:
    ap = argparse.ArgumentParser(description="coral-router performance score matrix")
    ap.add_argument("--config", default=DEFAULT_CONFIG, help="coral-router config JSON (source of truth)")
    ap.add_argument("--base-url", default=None, help="override router base URL (default: from config server.bind_addr)")
    ap.add_argument("--json", default=None, help="write JSON report to this path")
    ap.add_argument("--ttft-timeout", type=float, default=300.0, help="per-request stream timeout seconds")
    ap.add_argument("--warmup", type=int, default=1, help="number of warmup calls per route before scoring")
    ap.add_argument("--skip-warm-ttft", action="store_true", help="skip the post-sweep warm TTFT probe")
    ap.add_argument("--audit-dir", default=None, help="durable audit log dir (default: from config logging.audit_log.log_dir)")
    ap.add_argument("--audit-settle", type=float, default=DEFAULT_AUDIT_SETTLE_S, help="seconds to let the audit writer flush before reading (live mode)")
    ap.add_argument("--audit-only", metavar="AUDIT_DIR", default=None, help="hermetic mode: parse audit records and report Needle metrics only; requires no live router")
    args = ap.parse_args()

    try:
        cfg = load_config(args.config)
    except (OSError, json.JSONDecodeError) as e:
        log(f"ERROR: cannot load config {args.config}: {e}")
        return 2
    exp_table = derive_expectations(cfg)

    if args.audit_only:
        # Hermetic audit-only mode: no router contact at all. Reports the Needle
        # metrics from the given audit dir/file of aggregate route records.
        records = load_audit_records(args.audit_only)
        m = needle_metrics(records, cfg, exp_table)
        print(f"audit source:  {args.audit_only}")
        print(f"config:        {args.config}")
        print(f"routes:        {len(exp_table)} ({', '.join(sorted(exp_table))})")
        print("")
        print_needle_metrics(m, cfg, exp_table, attribute_decisions(records, cfg))
        return 0

    base = base_url_from_config(cfg, args.base_url)
    device_total = device_vram_total(cfg)
    min_rem = min_remaining_vram(cfg)

    log(f"config:      {args.config}")
    log(f"base url:    {base}")
    log(f"routes:      {len(exp_table)} ({', '.join(sorted(exp_table))})")
    log(f"vram total:  {device_total or 'unknown'}")
    log(f"min remain:  {min_rem}")
    log("")

    # Health gate.
    status, health = http_json(f"{base}/health")
    if status != 200:
        log(f"ERROR: router not healthy at {base} (health={status}: {health})")
        return 2

    results: Dict[str, Dict[str, Any]] = {}
    hard_failures = 0

    # The configured classifier model (direct responses are attributed to it).
    classifier_key: Optional[str] = None
    for p in cfg.get("pipelines", {}).values():
        if isinstance(p, dict) and p.get("classifier_model"):
            classifier_key = p.get("classifier_model")
            break

    # ---- Route sweep: routing accuracy + TTFT per route ----
    for route in sorted(exp_table):
        exp = exp_table[route]
        prompt = ROUTE_PROMPTS.get(route, exp.get("description") or "Hello.")
        body = {
            "model": route,
            "messages": [{"role": "user", "content": prompt}],
            "stream": True,
            "max_tokens": 64,
        }

        # Warmup (loads lazy models, primes caches) — not scored.
        for _ in range(max(0, args.warmup)):
            stream_ttft(f"{base}/v1/chat/completions", dict(body), args.ttft_timeout)

        ttft, status, served = stream_ttft(f"{base}/v1/chat/completions", body, args.ttft_timeout)
        served_key, direct = served_as(served, route, cfg)
        routing = score_routing(served_key, exp, classifier_key)
        speed = score_speed(ttft, served_key, cfg)

        ok = status == 200 and served_key is not None
        if not ok:
            hard_failures += 1

        served_label = f"{served_key} [direct classifier]" if direct else (served_key or f"?? {served}")
        always_mark = " [always-route]" if exp["always_route"] else ""
        results[route] = {
            "group": exp["group"],
            "ladder": exp["ladder"],
            "primary": exp["primary"],
            "expected_description": exp["description"],
            "always_route": exp["always_route"],
            "status": status,
            "ttft_s": round_half(ttft),
            "served_model": served,
            "served_key": served_key,
            "direct_response": direct,
            "routing_score": round_half(routing),
            "speed_score": round_half(speed),
        }
        log(
            f"[{route:<11}] status={status:<3} ttft={ttft:6.2f}s "
            f"routed={served_label} "
            f"routing={routing:4.1f} speed={speed:4.1f}{always_mark}"
        )

    # ---- Warm TTFT probe: latency for a now-resident model ----
    warm_ttft: Optional[float] = None
    if not args.skip_warm_ttft and exp_table:
        # Exercise a pinned/default model until resident, then measure a
        # back-to-back request (the steady-state "beginning of inference" cost).
        probe_route = "local" if "local" in exp_table else sorted(exp_table)[0]
        probe = {
            "model": probe_route,
            "messages": [{"role": "user", "content": WARM_PROMPT}],
            "stream": True,
            "max_tokens": 16,
        }
        for _ in range(max(1, args.warmup)):
            stream_ttft(f"{base}/v1/chat/completions", dict(probe), args.ttft_timeout)
        warm_ttft, _, _ = stream_ttft(f"{base}/v1/chat/completions", probe, args.ttft_timeout)
        log(f"\nwarm TTFT (resident {probe_route} route): {warm_ttft:.2f}s")

    # ---- VRAM efficiency ----
    used, instances, inst_status = instances_aggregate(base)
    if used is None:
        log(f"\nWARNING: /instances unreachable (status={inst_status}); VRAM score unknown")
    vram = score_vram(used or 0, device_total, min_rem)
    resident_keys = sorted({normalize_model(i.get("id", ""), cfg) for i in (instances or []) if normalize_model(i.get("id", ""), cfg)})
    managed_keys = sorted(cfg.get("models", {}))
    pinned_resident = sum(1 for i in (instances or []) if i.get("pinned"))
    total_resident = len(instances or [])
    log(
        f"vram: used={used or 0} device_total={device_total or '?'} "
        f"budget={max((device_total or 0) - min_rem, 0)} "
        f"resident={total_resident} contexts ({pinned_resident} pinned), "
        f"models resident={resident_keys} vram_score={vram:.1f}"
    )

    # ---- Needle attribution (Milestone 5): from the durable audit ----
    audit_dir = args.audit_dir or audit_dir_from_config(cfg)
    time.sleep(max(0.0, args.audit_settle))
    records = load_audit_records(audit_dir)
    needle = needle_metrics(records, cfg, exp_table)
    per_route = attribute_decisions(records, cfg)
    log(
        f"\nneedle audit: {len(records)} route records parsed from {audit_dir} "
        f"(settle {args.audit_settle}s); "
        f"coverage={needle['needle_coverage']:.2f} "
        f"accuracy={needle['needle_routing_accuracy']:.2f} "
        f"direct={needle['needle_direct_response_rate']:.2f}"
    )

    # ---- Aggregate scores ----
    routing_scores = [r["routing_score"] for r in results.values()]
    speed_scores = [r["speed_score"] for r in results.values()]
    mean_routing = statistics.mean(routing_scores) if routing_scores else 0.0
    mean_speed = statistics.mean(speed_scores) if speed_scores else 0.0
    overall = (mean_routing + mean_speed + vram) / 3.0

    # ---- Report ----
    width = max([len(r) for r in results] or [10]) + 2
    print("\n" + "=" * 78)
    print("CORAL-ROUTER SCORE MATRIX  (0-10 each axis)")
    print("=" * 78)
    header = f"{'route':<{width}}{'group':<12}{'routing':>8}{'speed':>7}{'vram':>7}{'total':>7}"
    print(header)
    print("-" * 78)
    for route in sorted(results):
        r = results[route]
        total = (r["routing_score"] + r["speed_score"]) / 2.0
        print(f"{route:<{width}}{r['group']:<12}{r['routing_score']:>8.1f}{r['speed_score']:>7.1f}{'—':>7}{total:>7.1f}")
    print("-" * 78)
    print(
        f"{'MEAN':<{width}}{'':<12}{mean_routing:>8.1f}{mean_speed:>7.1f}{vram:>7.1f}{overall:>7.1f}"
    )
    print("=" * 78)
    print(f"Overall score: {overall:.2f} / 10")
    if warm_ttft is not None:
        print(f"Warm TTFT:     {warm_ttft:.2f}s (steady-state beginning of inference)")
    print(f"Residency:     {total_resident}/{sum(len(exp['ladder']) for exp in exp_table.values())} "
          f"contexts resident, {len(resident_keys)}/{len(managed_keys)} model keys loaded")
    print("")
    print_needle_metrics(needle, cfg, exp_table, per_route)

    report = {
        "config": args.config,
        "base_url": base,
        "audit_dir": audit_dir,
        "device_vram_total": device_total,
        "minimum_remaining_vram": min_rem,
        "vram_used": used,
        "vram_budget": max((device_total or 0) - min_rem, 0),
        "resident_instances": total_resident,
        "pinned_instances": pinned_resident,
        "resident_model_keys": resident_keys,
        "managed_model_keys": managed_keys,
        "warm_ttft_s": warm_ttft,
        "needle": needle,
        "needle_decisions": per_route,
        "scores": {
            "routing_accuracy": round_half(mean_routing),
            "speed_ttft": round_half(mean_speed),
            "vram_efficiency": round_half(vram),
            "overall": round_half(overall),
        },
        "routes": results,
    }
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(report, fh, indent=2)
        log(f"\nJSON report written to {args.json}")

    return 1 if hard_failures and mean_routing == 0 else (0 if not hard_failures else 1)


if __name__ == "__main__":
    sys.exit(main())
