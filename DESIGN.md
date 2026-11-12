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
The `drift mcp` server exposes the same operations as tools so the agent never parses human text: MCP over stdio
(JSON-RPC, one message per line), tools `drift_check`/`drift_test`/`drift_run`/`drift_fmt`/`drift_outline`/
`drift_build`/`drift_card` and the resource `drift://card`. Each tool runs the CLI as a subprocess (own process
group, killed on timeout), so the server survives anything the program does; a `source` argument is checked in a
temporary file whose path is reported as `main.dr`. Register it with `claude mcp add drift -- drift mcp`.

### Diagnostic shape (stable contract)
```json
{"code":"P0031","severity":"error","span":{"file":"src/a.dr","start":[1,18],"end":[1,19]},
 "message":"generic arguments use square brackets in Drift","expected":"`[`","found":"`<`",
 "fix":{"edits":[{"span":{"file":"src/a.dr","start":[1,18],"end":[1,19]},"text":"["},
                 {"span":{"file":"src/a.dr","start":[1,22],"end":[1,23]},"text":"]"}]}}
```
Rules: one JSON object per line, sorted by position; `fix` appears only when the edit is certain;
the parser recovers as if each fix were applied, so one mistake yields one diagnostic.

## 6. How "better for AI" is measured (eval/)
For each task in `eval/TASKS.md`, the agent gets only `CARD.md` plus the task text, in Drift, and the same task in Python and Rust.
Metrics: pass@1 (first attempt passes hidden tests), pass@3 with compiler feedback, iterations to green, tokens spent,
runtime and peak memory of the result on the HP. A design change is kept only if it doesn't regress these.

## 7. Back end (phase 3)
Typed AST → C11 → gcc. The checker leaves notes keyed by span (which function a method call resolves to,
field indices, how `?` converts an error, which variant a pattern names); the generator needs nothing else.
- Values are 16 bytes (`kind` + payload): Int/Float/Bool unboxed, everything else a refcounted object.
  Generic code is not monomorphized; `==`, `<`, `hash`, `show` are structural and dispatch on the kind.
- Ownership is uniform: every expression yields an owned value; locals are hoisted and released at the
  function's single exit, so `return`, `?`, `break`, `continue` need no cleanup paths.
- Mutation (`x.f = v`, `m[k] = v`, `var self` methods) goes through slots made unique first (copy-on-write);
  `s += t` and `xs + ys` on a unique left side grow in place.
- `drift test` forks one process per `examples`/`test` block and captures its stderr; holes panic only when reached.
- The runtime object and program binaries are cached by content hash in `~/.cache/drift`.
- Checked in CI-style runs with AddressSanitizer + UBSan + LeakSanitizer (`DRIFT_CFLAGS`).
Reified generics: `json.decode[T]` and `db` `query[T]`/`query_one[T]` are the generics whose type must exist at
run time. The checker records the concrete `T` of each call (E0803/E0804 if it is unknown, a type parameter, a
function, an opaque handle or, for rows, not made of column values) and the generator passes a static `DTy`
descriptor; every other generic stays erased. SQL parameters are checked (E0805) but read from the values.
Explicit type arguments also work on methods: `conn.query[(Str, Int)](sql, ())`.
JSON mapping: numbers, Bool, Str; Bytes as base64; `()`/`None` as null and `Some(x)` as `x`; List/Set/tuple as
arrays; `Map[Str, V]` as an object, other maps as `[[k, v], ...]`; structs as objects (a missing `Option` field
decodes as `None`, unknown keys are ignored); unit variants as `"Name"`, others as `{"Name": fields}`;
`Ok`/`Err` as `{"ok": x}`/`{"err": e}`. Decode errors carry a path: `at $.items[3].price: expected Int, found string`.
HTTP: HTTP/1.1 on plain sockets (connect 15 s, I/O 30 s timeouts, chunked, up to 10 redirects, 256 MiB body cap).
TLS uses the system OpenSSL 3 through `dlopen` on the first https URL, with peer and hostname verification and
TLS >= 1.2, so `drift build` output still has no link-time dependency. `http.serve` handles one request per
connection, each connection in its own task (10 s I/O timeout, 64 KiB headers, 16 MiB bodies, UTF-8 checked).
DB: SQLite (`libsqlite3.so.0`) and PostgreSQL (`libpq.so.5`) are loaded with `dlopen` on the first `db.open`,
so binaries keep no link-time dependency. One statement per `exec`/`query` (SQLite rejects trailing ones), `$n`
placeholders bound by number on both (SQLite checks every `$n` <= the parameter count and that all are used).
Postgres runs `PQexecParams` with text results parsed per target type (Int as int8, Float float8, Bool, Bytes as
binary bytea; Str parameters are untyped so the server infers them), client encoding UTF8, NOTICEs dropped.
SQLite opens with foreign keys on and a 5 s busy timeout; its constraint errors map to the Postgres SQLSTATEs.
Row decoding: struct fields by column name (a missing `Option` field is `None`), tuples by position, one value
needs one column; NULL into a non-Option, wrong types, out-of-range ints and invalid UTF-8 are `Err` with the row
and column. `tests/programs/db.dr` gives byte-identical output on both backends.
Regex: own engine (pattern → AST → instructions kept in a Bytes value → Pike VM over UTF-8), leftmost-first
like Perl/RE2, linear in the text, so no pattern can hang a program (`(a+)+$` on 5000 chars runs in
microseconds). No backreferences or lookaround: they are rejected with a reason. Case folding is simple
(ASCII, Latin-1, Latin Extended-A, Greek, Cyrillic). Programs are capped at 20000 instructions.
HTML: forgiving tokenizer with the implied end tags that matter in practice (p, li, dt/dd, option, tr, td/th,
thead/tbody/tfoot, h1-h6, a), raw script/style, entities, nesting capped at 512. The tree is stored flat in
one Bytes value in document order, so a Node is (Document, index), copying is free and descendants are a
contiguous range. `text()` collapses whitespace and separates block elements, like rendered text.
Both parsers were fuzzed under ASan/UBSan/LeakSanitizer (800 random and adversarial inputs).
`r"""..."""` raw multi-line strings exist because HTML, JSON, CSS and regexes are this language's daily data.
Sandbox: generated `main` calls `d_sandbox(effects of main)` before any user code; the test harness does the
same in each forked block with that block's `uses`. Landlock (when `fs`/`db` are absent) handles every
filesystem right of the running ABI with no rules, except that `net` keeps certificates, resolver files and
system libraries readable; OpenSSL is dlopen'ed before the restriction. seccomp checks the audit arch (foreign
ABIs are killed, x32 gets ENOSYS) and returns EACCES for `socket` without `net`/`db`, for exec/fork/non-thread
`clone` without `proc` (`clone3` gets ENOSYS so libc falls back), and EPERM for ptrace, bpf, io_uring, mount,
namespaces, keyrings and modules. Sanitizer builds keep ptrace, plain clone and /proc for LeakSanitizer.
Kernels without Landlock or seccomp run unrestricted unless `DRIFT_SANDBOX=strict`. `db` currently opens both
`fs` and `net` (sqlite files, postgres sockets).
Concurrency (phase 5): refcounts are thread-local by default (rc > 0, plain increments) and become shared
when a value crosses to another task: `d_share` flips rc negative for the value and everything it reaches
(iteratively, stopping at shared or immortal objects) on spawn captures, channel sends, task results,
par_map inputs/outputs and constants. Shared objects count atomically; a COW check that finds rc == -1 owns
the only reference and makes the object local again before mutating it, which keeps the invariant "a shared
object only reaches shared objects". Single-threaded cost: one extra branch per retain/release (about 3% on a
refcount-heavy benchmark). Scheduler: a FIFO of tasks, at most `ncpu` threads running Drift code (main
included); network, db, sleep and stdin calls are bracketed by `d_block_enter/exit` and waits on `join` or a
channel release their slot, so a worker is woken or started (<= 256 threads, 8 MiB stacks). `join` of a task
that has not started runs it inline. par_map: up to 63 helper tasks claim index chunks from an atomic counter
while the caller works too. Channels: mutex + condvar ring buffer; capacity 0 is a rendezvous; `close` ends
the stream; dropping the receiver makes sends no-ops. Deadlock: a wait that times out (1 s) with every busy
thread waiting, an empty queue and no progress panics. Verified with ThreadSanitizer (0 races over 40 runs of
`tests/programs/tasks.dr`), ASan/LSan, and the same output with `DRIFT_THREADS=1`.
Known limits: a value leaked on an early `?` exit is reclaimed at process end; a panic in any task ends the program.

## 7b. Phase 6: a real El Dato component
`ports/eldato` is El Dato's `offers/identity.py` (product identity across stores: `identity_key`, a SHA-1 of the
exact word set plus sizes, and `contains_product`) in Drift, with the original's test cases. Checked against the
Python original on all 557,863 products of a restored backup: every key identical (402,326 with an identity) and
all 1,115,724 `contains_product` results on neighbouring titles identical. CPU time on the HP (loaded by El Dato,
so only user time is meaningful): keys 18.0 s vs 29.2 s in Python, pairs 85 s vs 108 s; memory 384 MB vs 615 MB.
What the port needed, now in std: Unicode tables generated from Python's `unicodedata` (`tools/gen_unicode.py`,
Unicode 16.0; `tools/check_unicode.py` compares all 1,112,053 code points: 0 mismatches) for case mapping,
NFKD, combining classes and the alpha/alnum/space properties that regex `\w`/`\b` and `trim` use; `hash`
(SHA-1, SHA-256, FNV-1a); `Set.difference`/`is_subset`/`is_empty`. Profiling (gprof) showed the regex engine at
~65% of the time: unanchored searches now skip to bytes that can start a match (first-byte set computed at
compile time and stored in the program) and reuse a per-thread VM instead of allocating one per call (30.7 s ->
18.3 s). Next speedup: a lazy DFA for regexes. Writing the port also surfaced one habit the checker fixed on its
own: `!x` → `not x` (`drift check --fix`).

## 8. Roadmap
| Phase | Deliverable |
|---|---|
| 0 | CARD, DESIGN, example corpus, eval tasks |
| 1 | Rust bootstrap: lexer, lossless parser, `drift fmt`, parse the whole corpus |
| 2 | Name resolution, types with local inference, effects, JSON diagnostics with fixes, `todo` holes |
| 3 | ✅ C backend + runtime (refcount, copy-on-write Str/Bytes/List/Map/Set), `drift run/build/test`, contracts, isolated tests |
| 4 | std: io, fs, json, str, time, env, proc, http client, re, html, db (sqlite/postgres); sandbox from `uses` |
| 5 | ✅ Tasks + `spawn`/`par_map`/channels runtime (parallel, shared refcounts); next: `drift mcp`; eval runs vs Python/Rust |
| 6 | ✅ Real workload: El Dato identity.py ported, identical on 557,863 products, 1.6x less CPU than Python |
| 7 | Self-hosting; own backend (connects to the compiler project) |
