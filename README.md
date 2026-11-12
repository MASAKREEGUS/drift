# Drift

A compiled, statically typed programming language designed for AI agents.

- `CARD.md` — the complete language, small enough to live in a prompt.
- `DESIGN.md` — why each decision was made, compilation pipeline, roadmap.
- `examples/` — corpus of Drift programs (parser/typechecker test suite).
- `eval/` — tasks used to measure agent performance in Drift vs Python/Rust.

Status: phases 0-3 done: parser, formatter, type/effect checker with certain fixes (`drift check --fix`), C backend with a refcounted copy-on-write runtime (`drift run`, `drift build`, `drift test`). Next: std (http, json, html, db) and the effect sandbox.
