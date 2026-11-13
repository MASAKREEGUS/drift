# Drift eval — task set v0

Protocol: the agent receives `CARD.md` + one task. Hidden tests run on the result. Same task is solved in Python and Rust as baselines.
Record per task and language: pass@1, pass@3 (with compiler/test feedback), iterations, tokens in/out, wall time, peak RSS on the HP.
Implemented for 8 tasks (t01 t03 t05 t06 t10 t12 t13 t25): statements in `tasks/<id>.md`, hidden tests from
`gen.py` (fixed seeds, expected outputs from `reference.py`) into `hidden/` (not committed), grading with
`grade.py submit|bench|report`. Results of each run: `RESULTS.md`, final solutions in `solutions/<run>/`.

| id | category | task |
|---|---|---|
| t01 | basics | Read integers from stdin, print min, max, mean (2 decimals), median. |
| t02 | basics | Roman numerals: convert both directions, reject invalid input with an error. |
| t03 | strings | Word frequency of a file, top-N, Unicode-aware lowercasing. |
| t04 | strings | Parse and validate RFC 3339 timestamps; print normalized UTC. |
| t05 | parsing | CSV with quoted fields → JSON array of objects. |
| t06 | parsing | Arithmetic expression evaluator with precedence, parentheses and error positions. |
| t07 | parsing | INI file parser with sections, comments and typed getters. |
| t08 | data | Group sales records by store and month; output totals sorted. |
| t09 | data | Deduplicate offers by (sku, store) keeping the lowest price; detect inflated reference prices vs 90-day median. |
| t10 | algorithms | Dijkstra on a weighted graph from a text file; print path and cost. |
| t11 | algorithms | LRU cache with capacity, get/put, O(1) operations. |
| t12 | algorithms | Topological sort with cycle detection reporting the cycle. |
| t13 | algorithms | Sudoku solver (backtracking) reading 81-char puzzles. |
| t14 | state | Bank account ledger: transfers, overdraft rejection, replay from an event log. |
| t15 | state | Inventory with reservations that expire after N ticks (deterministic clock). |
| t16 | fs | Recursive directory size report, largest 20 files, skip symlinks. |
| t17 | fs | Append-only key-value store with crash-safe recovery and compaction. |
| t18 | net | HTTP client: fetch 20 URLs concurrently with a limit of 4, timeouts, retries with backoff. |
| t19 | net | HTML scraping: extract product name, price and JSON-LD from saved store pages. |
| t20 | net | JSON REST API server: CRUD over an in-memory store with validation errors as 4xx. |
| t21 | db | SQLite: schema migration + parameterized queries + report. |
| t22 | concurrency | Parallel map-reduce word count over many files using all cores. |
| t23 | concurrency | Producer/consumer pipeline with bounded channels and graceful shutdown. |
| t24 | cli | CLI with subcommands, flags, `--help`, exit codes. |
| t25 | perf | Count primes below 50M (sieve); measured runtime on the HP. |
| t26 | perf | Parse a 200 MB log file, aggregate status codes per hour within a memory limit. |
| t27 | repair | Given a buggy Drift program and failing examples, fix it. |
| t28 | repair | Given a working program, add a feature without breaking existing examples. |
| t29 | safety | Program must not open any socket: verify that effect checking rejects a hidden `net` call. |
| t30 | large | Multi-module project (5+ modules): change a shared type and update all call sites. |
