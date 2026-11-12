# Drift — design rationale

Primary user: an AI coding agent (Claude). Humans are not the target reader.
Goal: an agent produces correct, fast, safe programs in Drift with fewer iterations than in any other language,
and those programs run well on weak hardware.

## 1. Constraints that shaped everything

| Constraint | Consequence |
|---|---|
| A new language has zero training data | The whole language must fit in context (`CARD.md` ≤ ~3.5k tokens). Syntax reuses the most common token shapes from Rust/TypeScript/Python, so priors transfer. Novelty is spent only where it buys correctness. |
| Agents see a slice of the codebase, not all of it | Local reasoning: complete signatures (types + errors + effects), no globals, no shadowing, no macros, no extension methods, no implicit conversions. Reading one function and the outlines of what it calls is always enough. |
| Agents generate plausible-but-wrong code | The compiler is a repair engine: every diagnostic is structured, complete (all errors at once) and, when deterministic, carries a machine-applicable fix. Intent lives in code (`examples`, `requires`, `ensures`) and is executed. |
| Agents generate code that must be trusted | Effects in signatures + OS-enforced sandbox from `main`'s `uses` list. Generated code cannot touch net/fs/proc unless the signature says so. |
| Several equivalent forms cause drift between generations | One canonical form, enforced by `drift fmt` and `drift check`. One loop syntax family, one closure syntax, one error mechanism, one string interpolation. |
| Target machine: Pentium 3556U (2 cores, SSE4.2, no AVX), 7 GB RAM | Ahead-of-time native code, no JIT, no tracing GC, small runtime, low memory per task, baseline `x86-64-v2`. |

## 2. Token-level choices
- Keywords are English words already frequent in code: `fn let var if else match for in while loop return pub import uses requires ensures examples test todo spawn`.
- `and or not` instead of `&& || !`: one meaning per token, and `|` stays free for closures and or-patterns.
- Generics use `[]` (`List[Int]`), never `<>`: no parse ambiguity with comparison operators, and Python type hints already taught this shape.
- String interpolation `"{x}"` only — no `format!`, no `%`, no concatenation idioms competing with it (`+` on `Str` is allowed but `fmt` rewrites `a + "x"` chains to interpolation).
- Newline-terminated statements: fewer tokens, no missing-semicolon diagnostics.
- Identifiers in English. Code text is data for the model; English maximizes prior knowledge.

## 3. Semantics
- **Value semantics + refcounting + copy-on-write.** No references means no aliasing bugs and no borrow checker (the main source of agent failure in Rust). Because values are immutable and there are no references, reference cycles cannot be constructed, so refcounting is complete — no cycle collector.
- **In-place reuse.** When a refcount is 1 at a mutation or a `with` update, the compiler reuses the buffer (Perceus-style reuse analysis). Typical `var xs` loops compile to in-place mutation.
- **Checked arithmetic** by default; wrapping/saturating ops are explicit std functions.
- **Deterministic:** insertion-ordered maps, defined evaluation order (left to right), no UB. Same input ⇒ same output, which makes `examples` reliable.
- **Effects** are a closed set (v0). Inference is not used for effects on `pub` functions: they must be written, because they are part of the contract the agent reads.
- **Contracts** are runtime checks in v0, a target for static discharge later (SMT) without changing syntax.

## 4. Compilation pipeline
```
.dr → lexer → parser (lossless CST, keeps spans) → name resolution → type + effect check
    → lowering to Drift IR (SSA-ish, explicit refcount ops) → reuse/drop optimization
    → C11 codegen → system C compiler (gcc/clang) → static native binary
```
- **C as backend** (v0–v1): runs everywhere a C compiler exists (the HP, ARM boards, future own RISC-V CPU), inherits decades of optimizer work, keeps the compiler small. A direct backend is a later project.
- **Lossless CST** so `fmt`, `apply_fix` and MCP edits are exact and round-trip.
- **Incremental by module**: a module's `outline` hash decides whether dependents recompile.
- Bootstrap compiler in Rust. Long-term: self-host in Drift.

## 5. Agent workflow the toolchain is built around
1. `drift outline` the modules involved (cheap, compact context).
2. Write code; leave `todo` holes where uncertain.
3. `drift check --json` → apply every `fix`, read expected types for holes, repeat.
4. `drift test` → `examples`/contracts tell what is wrong in value terms.
5. `drift build --release`.
The `drift mcp` server exposes the same operations as tools so the agent never parses human text.

### Diagnostic shape (stable contract)
```json
{"code":"E0204","severity":"error","span":{"file":"src/offers.dr","start":[12,3],"end":[12,41]},
 "message":"function returns Result[Int, OfferError] but this expression is Int",
 "expected":"Result[Int, OfferError]","found":"Int",
 "fix":{"edits":[{"span":{"start":[12,3],"end":[12,41]},"insert_before":"Ok(","insert_after":")"}]},
 "explain":"drift explain E0204"}
```

## 6. How "better for AI" is measured (eval/)
For each task in `eval/TASKS.md`, the agent gets only `CARD.md` plus the task text, in Drift, and the same task in Python and Rust.
Metrics: pass@1 (first attempt passes hidden tests), pass@3 with compiler feedback, iterations to green, tokens spent,
runtime and peak memory of the result on the HP. A design change is kept only if it doesn't regress these.

## 7. Roadmap
| Phase | Deliverable |
|---|---|
| 0 | CARD, DESIGN, example corpus, eval tasks |
| 1 | Rust bootstrap: lexer, lossless parser, `drift fmt`, parse the whole corpus |
| 2 | Name resolution, types with local inference, effects, JSON diagnostics with fixes, `todo` holes |
| 3 | IR + C backend + runtime (refcount, Str, List, Map, Set), `drift run/build`; `examples`/contracts in `drift test` |
| 4 | std: io, fs, json, str, time, env, proc, http client, re, html, db (sqlite/postgres); sandbox from `uses` |
| 5 | Tasks + `spawn`/`par_map`/channels runtime; `drift mcp`; eval runs vs Python/Rust |
| 6 | Real workload: port an El Dato component and run it on the HP |
| 7 | Self-hosting; own backend (connects to the compiler project) |
