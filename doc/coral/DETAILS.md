# Coral Context: Detailed Engineering Specification

**Status:** Current (Zig/SQLite implementation, Milestone 3-5 in progress)
**Last Updated:** March 2026
**Architecture:** SQLite backend with Zig deterministic core

---

## Executive Summary

Coral Context is a neurosymbolic orchestration framework optimized for edge environments. It systematically decouples probabilistic reasoning (LLMs) from deterministic execution (DAGs and Graph Databases). By utilizing Zig's high-performance, low-memory footprint alongside embedded SQLite, the system delegates workflow routing, knowledge retrieval, and tool orchestration to a strictly typed engine. LLMs are treated solely as unstructured data compilers accessed over HTTP, preventing hallucinations in the critical path and enabling complex agentic behaviors on devices as small as a Raspberry Pi.

**Core Technologies:**
- **Zig** for bitwise, deterministic DAG execution
- **SQLite** (embedded relational database) for semantic graphs and recursive CTE traversal
- **Extism** for secure WASM sandboxing
- **HTTP LLMs** for probabilistic inference

**ALERT - stale documentation omitted.  This space reserved for fresh implementation details.**

---

End of specification.
