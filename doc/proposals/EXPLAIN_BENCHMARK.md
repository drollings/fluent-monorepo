You are a code intelligence benchmarking agent. Your task is to discover two facts about this codebase and compare the methods used.
Codebase root: /opt/src/danielr/coral
---
Fact 1 — Free exploration - do NOT use anything in AGENTS.md or Makefile for Fact 1!
Discover: What storage backends does LanceDB support in this codebase, and how does the Zig wrapper select between them at runtime?
Use whatever tools you have: read files, grep, glob, etc. Record:
- Every file you read (path + lines read)
- Every search query you ran
- Approximate token cost (count the text you consumed to arrive at the answer)
- The answer itself
---
Fact 2 — AGENTS.md-guided exploration - NOW use AGENTS.md and what it'll tell you about make.
First, read /opt/src/danielr/coral/AGENTS.md. Follow its discovery workflow exactly:
1. Run make explain QUERY="StorageEngine" from /opt/src/danielr/coral
2. Use only the output of that command (plus any sources it cites) to answer the same class of question: What storage backends does LanceDB support in this codebase, and how does the Zig wrapper select between them at runtime?
Record:
- The exact make explain output (verbatim)
- Whether the output was sufficient to answer without further file reads
- Approximate token cost (the explain output itself)
- The answer itself
---
Comparison
Produce a final section with a markdown table:
| Dimension | Free exploration | make explain |
|-----------|-----------------|--------------|
| Files read | | |
| Searches run | | |
| Tokens consumed (est.) | | |
| Answer quality (1–5) | | |
| Time to answer | |  |
Then write 2–3 sentences: which method produced better signal-to-noise for an orchestrator agent, and why.





You are a code intelligence benchmarking agent. Your task is to discover two facts about this codebase and compare the methods used.
Codebase root: /opt/src/danielr/coral

---

Fact 1 — AGENTS.md-guided exploration - use AGENTS.md and what it'll tell you about make.
First, read /opt/src/danielr/coral/AGENTS.md. Follow its discovery workflow exactly:
1. Run make explain QUERY="StorageEngine" from /opt/src/danielr/coral
2. Use the output of that command (plus any sources it cites) to answer the same class of question: What storage backends does LanceDB support in this codebase, and how does the Zig wrapper select between them at runtime?
Record:
- The exact make explain output (verbatim)
- Whether the output was sufficient to answer without further file reads
- If any file reads were required, whether make explain gave good recommendations where to look
- Approximate token cost (the explain output itself, and overall)
- The answer itself

---

Fact 2 — Free exploration - do NOT use anything in AGENTS.md or Makefile for Fact 1!
Discover: What storage backends does LanceDB support in this codebase, and how does the Zig wrapper select between them at runtime?
Use whatever tools you have: read files, grep, glob, etc. Record:
- Every file you read (path + lines read)
- Every search query you ran
- Approximate token cost (count the text you consumed to arrive at the answer)
- The answer itself
---
Comparison
Produce a final section with a markdown table:
| Dimension | Free exploration | make explain |
|-----------|-----------------|--------------|
| Files read | | |
| Searches run | | |
| Tokens consumed (est.) | | |
| Answer quality (1–5) | | |
| Time to answer | |  |
Then write 2–3 sentences: which method produced better signal-to-noise for an orchestrator agent, and why.







You are a code intelligence benchmarking agent. Your task is to discover two facts about this codebase and compare the methods used.
Codebase root: /opt/src/danielr/coral

---

Fact 1 — AGENTS.md-guided exploration - use AGENTS.md and what it'll tell you about make.
First, read /opt/src/danielr/coral/AGENTS.md. Follow its discovery workflow exactly:
1. Run make explain QUERY="What storage backends does LanceDB support in this codebase, and how does the Zig wrapper select between them at runtime?" from /opt/src/danielr/coral
2. Use the output of that command (plus any sources it cites) to answer the same class of question: What storage backends does LanceDB support in this codebase, and how does the Zig wrapper select between them at runtime?
Record:
- The exact make explain output (verbatim)
- Whether the output was sufficient to answer without further file reads
- If any file reads were required, whether make explain gave good recommendations where to look
- Approximate token cost (the explain output itself, and overall)
- The answer itself

---

Fact 2 — Free exploration - do NOT use anything in AGENTS.md or Makefile for Fact 1!
Discover: What storage backends does LanceDB support in this codebase, and how does the Zig wrapper select between them at runtime?
Use whatever tools you have: read files, grep, glob, etc. Record:
- Every file you read (path + lines read)
- Every search query you ran
- Approximate token cost (count the text you consumed to arrive at the answer)
- The answer itself
---
Comparison
Produce a final section with a markdown table:
| Dimension | Free exploration | make explain |
|-----------|-----------------|--------------|
| Files read | | |
| Searches run | | |
| Tokens consumed (est.) | | |
| Answer quality (1–5) | | |
| Time to answer | |  |
Then write 2–3 sentences: which method produced better signal-to-noise for an orchestrator agent, and why.
