# Eval run1 — 2026-11-12

8 tasks from `TASKS.md` (t01 t03 t05 t06 t10 t12 t13 t25), each solved by a fresh Claude Opus 5.5 agent per language
(24 agents). Drift agents got only `drift card` (CARD.md) + the task; Python 3.14 and Rust 1.99 agents only the task.
Standard library only. Hidden tests: `gen.py` (fixed seeds) with expected outputs from `reference.py`; grading by
`grade.py` (at most 3 submissions; the first one is pass@1). Run time: median CPU (user+sys) of 3 runs on the HP
(Pentium 3556U, shared with El Dato in production, so wall time is not comparable; CPU time is).

## Correctness

| task | Drift | Python | Rust |
|---|---|---|---|
| t01 stats | pass@1 | pass@1 | pass@1 |
| t03 word freq (Unicode) | **fail** 12/16 | pass@1 | pass@1 |
| t05 CSV → JSON | pass@1 | pass@1 | pass@1 |
| t06 expression evaluator | pass@1 * | pass@1 | pass@1 |
| t10 Dijkstra | pass@1 | pass@1 | pass@1 |
| t12 toposort + cycle | pass@1 | pass@1 | pass@1 |
| t13 sudoku | pass@1 | pass@1 | pass@1 |
| t25 primes | pass@1 | pass@1 | pass@1 |
| **pass@1** | **7/8** | **8/8** | **8/8** |

t03 Drift: the counting matched exactly, but Drift has **no way to exit with status 2** (`Err` from `main` is 1,
panic 101), which the task requires for a bad argument. A language gap, not an agent mistake.
\* t06: a harness bug (7 generated lines overflowed i64 before a division by zero, which the task promises never
happens; Drift's overflow check caught it, Python's big ints and Rust's release wrapping hid it). Fixed in `gen.py`,
all t06 submissions re-graded; Drift's first submission passes. The pre-fix results are kept apart.

## Effort (per agent, from the agent tool: context tokens at the end, tool calls, wall time)

| | Drift | Python | Rust |
|---|---|---|---|
| tokens (mean) | 62.4k | 46.9k | 41.6k |
| tool calls (mean) | 12 | 9.6 | 4.5 |
| lines of code (total, 8 tasks) | 1095 | 629 | 787 for 7 tasks + 2499 for t03 (mostly generated Unicode tables) |

Several agents were interrupted by an API rate limit and resumed; their counts are approximate (see `usage-run1.txt`).
Drift's extra cost is mostly learning (reading the card, probing what is undocumented) and working around missing
primitives (below).

## Run time on the HP (CPU seconds, median of 3; peak RSS)

| case | Drift | Python | Rust |
|---|---|---|---|
| t01 10^6 integers | 1.13 s, 209 MB | 1.49 s, 121 MB | 0.09 s, 20 MB |
| t05 55 MB CSV | 1.21 s, 180 MB | 1.09 s, 100 MB | 0.09 s, 7 MB |
| t06 100k expressions | 0.67 s, 23 MB | 1.54 s, 30 MB | 0.16 s, 8 MB |
| t10 1M edges | 1.92 s, 123 MB | 2.61 s, 300 MB | 0.25 s, 57 MB |
| t10 200k-node path | 0.87 s, 71 MB | 1.15 s, 93 MB | 0.12 s, 14 MB |
| t12 300k-edge DAG | 1.16 s, 115 MB | 1.41 s, 105 MB | 0.34 s, 29 MB |
| t12 50k-node cycle | 1.27 s, 130 MB | 1.34 s, 102 MB | 0.35 s, 35 MB |
| t13 sudoku, t25 primes | < 0.15 s all | < 0.5 s all | < 0.1 s all |

t13/t25 are dominated by the algorithm each agent chose (t25 Drift switched to Lucy_Hedgehog O(n^¾) after a plain
sieve took 11 s), so they say little about the languages. **Where the work is comparable, Drift runs at about 0.8x
Python's CPU time and 5–13x Rust's, often with more memory than Python.** Drift is not yet the fast language it is
meant to be.

## What the agents ran into in Drift (reported independently, count of agents)

1. **CARD.md lists things that do not exist** (5): std modules `str list map set math net proc log`, the command
   `drift explain`. The card is the only documentation, so every wrong line costs every agent.
2. **No cheap access to characters or bytes** (5): no char type, no byte indexing, no bitwise operators; three
   agents independently used `bytes.read_u32_le(i) % 256`. `.chars()` builds one heap string per character:
   22 MB → 32 s and 2 GB (t05), 4.9 MB → 5 s (t06).
3. **Slow scalar loops** (2): ~25 ns per integer loop iteration, ~70 ns per list element write.
4. **Missing std pieces**: exit status (t03 failed on it), priority queue (written by hand twice), fixed-size list
   allocation, `Map.insert` (only `m[k] = v`, undocumented).
5. **Undocumented semantics**: `xs[i] = v` allowed, what `%` does with negatives, what `parse_int` accepts (`+5`),
   what `split_whitespace` splits on, cost of operations.
6. **Tooling friction**: `drift check` reports layout (F0001) as an *error*, so agents believed it blocked `run` (it
   does not); `9_000_000` is "not canonical"; the fix text shows byte offsets. A `match` arm cannot be an assignment
   (`None => c += 1`) and `--fix` does not offer the block.

What worked: the diagnostics with certain fixes (`--fix` "only ever had to fix layout"), `to_lower` with final sigma
and Unicode `is_alphanumeric` (t03 Rust needed 2400 lines of generated tables for the same thing), overflow checks
(they found the harness bug), `examples` blocks, and the sandbox never got in the way.
