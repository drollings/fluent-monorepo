# .guidance/todo/ — Work Item Lifecycle

Each subdirectory is a **work item** that moves through a dependency chain:

```
TODO.md → TRIAGE.md → WORK.md → COMPLETE.md → COMMITTED.md
```

## Lifecycle Stages

| Stage | File | Created by | Meaning |
|-------|------|------------|---------|
| **TODO** | `TODO.md` | Human / agent | Work identified, not yet analyzed |
| **TRIAGE** | `TRIAGE.md` | `guidance triage "<name>"` | Risk assessed, steps planned |
| **WORK** | `WORK.md` | Human / agent | Implementation in progress |
| **COMPLETE** | `COMPLETE.md` | Human / agent | Implementation done, tests pass |
| **COMMITTED** | `COMMITTED.md` | `guidance commit` | Changes committed to git |

## Creating a Work Item

```bash
mkdir .guidance/todo/my-feature
# Write the work description:
cat > .guidance/todo`/my-feature/TODO.md << 'EOF'
# Add feature X

## Goal
Describe what needs to be done.

## Files to change
- src/common/foo.zig
- src/guidance/main.zig

## Acceptance criteria
- Tests pass
- STRUCTURE.md updated
EOF

# Triage it (generates TRIAGE.md with risk assessment + steps):
guidance triage "my-feature"
```

## Committing with AI-summarized message

```bash
git add src/* .guidance/src/*
guidance commit   # Opens editor with AI-generated commit message
```

## Makefile targets

| Target | Description |
|--------|-------------|
| `guidance explain "<term>"` | Explain a module, function, or concept |
| `guidance triage "<name>"` | Generate TRIAGE.md for a TODO work item |
| `guidance gen` | Regenerate STRUCTURE.md from all guidance JSON |
| `guidance commit` | AI-summarize staged diff → open editor → commit |
| `guidance diary "..."` | Append timestamped diary entry |
| `guidance learn` | Drain inbox files into structured knowledge |
