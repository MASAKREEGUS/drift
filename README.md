# Drift

**A compiled, statically typed programming language designed to be written, read and repaired by AI agents.**

Most languages were designed for people typing in an editor. Drift is designed for a model that writes a whole
function at once, sees only a slice of the codebase, cannot be fully trusted, and repairs its work by reading
compiler output. Every decision in the language follows from that.

```drift
fn count_words(text: Str) -> Map[Str, Int] {
  var counts: Map[Str, Int] = {}
  for word in text.to_lower().split_whitespace() {
    let w = word.trim_matches(|c| not c.is_alphanumeric())
    if not w.is_empty() {
      counts[w] = counts.get(w).unwrap_or(0) + 1
    }
  }
  counts
}

examples count_words {
  count_words("a b a") == {"a": 2, "b": 1}
  count_words("Hola, hola!") == {"hola": 2}
}

pub fn main() -> Result[(), Error] uses io, fs, env {
  let path = env.args().get(1).ok_or(error("usage: word_count <file>"))?
  for (w, n) in count_words(fs.read_str(path)?).entries().sorted_by(|(w, n)| (-n, w)).take(10) {
    io.println("{n}\t{w}")
  }
  Ok(())
}
```

Status: **v0.1, experimental.** The compiler, runtime, standard library and tools below all work and are covered
by 105 tests, but the language is young and will change. Linux x86-64 only for now.

## What is different from other languages

| | Drift | Python | TypeScript | Go | Rust |
|---|---|---|---|---|---|
| Whole language fits in one prompt | yes, [`CARD.md`](CARD.md) (~17 KB) | no | no | no | no |
| Compiler output made for a program to read | JSON, all errors at once, with exact edits | text traceback, one at a time | text (fixes via language server) | text | JSON available, some suggestions |
| Side effects visible in the signature | `uses net, fs` on every function | no | no | no | no |
| Declared effects enforced by the OS | yes (Landlock + seccomp) | no | no | no | no |
| Null / exceptions | none; `Option`, `Result`, `?` | both | both | `nil`, `panic` | none |
| One accepted layout | enforced by the checker | optional tools | optional tools | `gofmt`, optional | `rustfmt`, optional |
| Unfinished code is a first-class state | `todo` holes report expected type and scope | no | no | no | `todo!()` without a report |
| Intent written next to the code and executed | `examples`, `requires`, `ensures` | doctest (optional) | no | example tests | doc tests |
| Aliasing and lifetimes to reason about | none (value semantics) | shared mutable references | shared mutable references | pointers | borrow checker |
| Tool server for agents built in | `drift mcp` | no | no | no | no |
| Output | native binary, no runtime dependency | interpreter | JS runtime | native binary | native binary |

What each of those buys an agent:

- **The language fits in context.** A new language has no training data, so the complete language is one card that
  an agent reads before writing. If something is not in the card, it does not exist. Syntax reuses the most common
  shapes from Rust, TypeScript and Python, so what a model already knows carries over.
- **The compiler is a repair engine.** `drift check --json` reports every problem in one pass, one JSON object per
  line, with `code`, `span`, `expected`, `found` and, when the repair is certain, a `fix` with exact edits.
  `drift check --fix` applies them. Habits from other languages are caught and translated:

  ```json
  {"code":"P0031","severity":"error","span":{"file":"a.dr","start":[1,17],"end":[1,18]},"message":"generic arguments use square brackets in Drift","expected":"`[`","found":"`<`","fix":{"edits":[{"span":{"file":"a.dr","start":[1,17],"end":[1,18]},"text":"["},{"span":{"file":"a.dr","start":[1,21],"end":[1,22]},"text":"]"}]}}
  {"code":"L0001","severity":"error","span":{"file":"a.dr","start":[3,6],"end":[3,7]},"message":"`!` is not Drift; use `not`","found":"`!`","fix":{"edits":[{"span":{"file":"a.dr","start":[3,6],"end":[3,7]},"text":"not "}]}}
  ```

- **Generated code cannot do more than its signature says.** Functions are pure unless they declare effects
  (`io fs net db env time rand proc spawn hw`). `main`'s `uses` list is what the program may do, and the kernel
  enforces it: without `net` no socket can be opened, without `fs` no file, without `proc` no exec or fork. You
  can run code a model wrote and know its reach by reading one line.
- **Local reasoning is always enough.** Complete signatures, no globals, no shadowing, no macros, no implicit
  conversions, no operator overloading, no inheritance, no references. Reading one function and the outlines of
  what it calls (`drift outline`) tells an agent everything it needs.
- **One way to write each thing.** One layout, one loop family, one closure syntax, one error mechanism, one string
  interpolation. Two generations of the same program do not drift apart, and diffs stay small.
- **No memory model to fight.** Values are immutable and copied in O(1) (reference counting with copy-on-write).
  There is no borrow checker and no garbage collector pause; cycles cannot be built, so reference counting is
  complete.
- **No undefined behavior.** Integer overflow, division by zero and out-of-range indexing stop the program with a
  message. Maps keep insertion order and evaluation order is defined, so the same input gives the same output.
- **Unfinished work is explicit.** `todo` type-checks as anything; `drift check` reports each hole with the type
  expected there and the names in scope, and `drift build` refuses to ship one.

## What is measured, honestly

Drift is checked against Python and Rust with fresh agents solving the same tasks blind
([`eval/RESULTS.md`](eval/RESULTS.md); Drift agents get only the card). Run 1, 8 tasks, 24 agents:

| | Drift | Python | Rust |
|---|---|---|---|
| Passed hidden tests on first submission | 7/8 | 8/8 | 8/8 |
| Mean tokens per agent | 62.4k | 46.9k | 41.6k |
| CPU time on comparable tasks | about 0.8x Python | 1x | 5 to 13 times faster than Drift |

So today Drift does **not** beat Python or Rust on first-try correctness or on tokens spent, and it is far from
Rust's speed. The one failure was a missing feature (no way to exit with a chosen status), and the extra tokens
were spent reading the card and working around gaps. Those gaps (bit operators, `exit`, heaps, byte indexing,
card errors) have been fixed since, and integer loops are about twice as fast, but the eval has not been re-run
yet. On a real workload, a port of a production product-matching module gave byte-identical results to the
Python original on 557,863 products with 1.6x less CPU and 1.6x less memory (`ports/eldato`).

The claim Drift does make today is about the properties in the table above: what an agent can know, verify and
be prevented from doing. Whether that turns into fewer iterations than mainstream languages is what the eval
exists to find out.

## Install

Requirements: Linux x86-64, Rust 1.85 or newer (to build the compiler), `gcc` (Drift compiles through C).
Optional at run time, loaded only when used: OpenSSL 3 (https), `libsqlite3`, `libpq`.

```sh
git clone https://github.com/MASAKREEGUS/drift
cd drift
cargo build --release
sudo cp target/release/drift /usr/local/bin/     # one self-contained binary: std and runtime are embedded
drift run examples/hello.dr
```

## Using Drift with an AI agent

### 1. Give the agent the language

The model has never seen Drift, so the card must be in its context. Either:

- paste the output of `drift card` (the same text as [`CARD.md`](CARD.md)) into the system prompt, or
- let the agent fetch it through the MCP tool `drift_card`.

Nothing else is needed: the card is the complete language and standard library summary.

### 2. Give the agent the tools

`drift mcp` is a Model Context Protocol server on stdio. Register it once:

```sh
claude mcp add drift -- drift mcp        # Claude Code
```

For any other MCP client, the command is `drift` with the argument `mcp`:

```json
{ "mcpServers": { "drift": { "command": "drift", "args": ["mcp"] } } }
```

| tool | what it does |
|---|---|
| `drift_card` | the language card |
| `drift_outline` | signatures, types and effects of a module (`std.http`, or a file) |
| `drift_check` | every diagnostic as JSON; with `fix` it applies the certain ones |
| `drift_test` | runs every `examples` and `test` block, each isolated |
| `drift_run` | compiles and runs `main` with stdin, args and a timeout, sandboxed by its `uses` |
| `drift_fmt` | canonical layout |
| `drift_build` | native executable |

Every tool takes a `path` or a `source` string, so an agent can check code before any file exists.
Agents without MCP can call the same commands in a shell; `--json` gives the same structured output.

### 3. The loop the toolchain is built for

1. `drift outline` the modules involved. It is cheap context.
2. Write the code. Put `todo` where unsure, and write `examples` for what the function should return.
3. `drift check --fix`, then read what is left: each diagnostic says what was expected and found, each hole says
   which type goes there and what is in scope.
4. `drift test`. Failing `examples`, `requires` and `ensures` describe the bug in terms of values.
5. `drift build -o app`.

### 4. A prompt that works

```
You write Drift. The language card below is the complete language: if something is not in it, it does not exist.
Declare effects with `uses`. Give every non-trivial function an `examples` block.
After writing, run drift_check with fix, then drift_test, and repeat until both are clean.

<output of `drift card`>
```

### 5. Running code you do not trust

Read `main`'s `uses` line. A program declared `uses io` can read stdin and write stdout and nothing else, and the
kernel holds it to that. `DRIFT_SANDBOX_REPORT=1` prints what is enforced; `DRIFT_SANDBOX=strict` refuses to run
on a kernel without Landlock or seccomp.

## Commands

| command | purpose |
|---|---|
| `drift check [--json] [--fix] <file\|dir>` | all diagnostics: syntax, types, effects, exhaustiveness, layout |
| `drift run <file\|dir> [args...]` | check, compile and run `main` |
| `drift test [--json] <file\|dir>` | run every `examples` and `test` block |
| `drift build <file\|dir> [-o out]` | native executable |
| `drift build --kernel <dir> -o <out>` | freestanding C for an operating system kernel |
| `drift fmt [--check]` | canonical layout |
| `drift outline <module>` | compact signatures of a module |
| `drift card` | print the language card |
| `drift mcp` | MCP server on stdio |

## What is in the box

- **Language:** structs, enums with data, exhaustive `match`, generics, closures, `Option`/`Result` with `?`,
  contracts, typed holes, modules and packages.
- **Standard library:** `io fs env http` (client with TLS, server) `json` (typed encode/decode) `db` (SQLite and
  PostgreSQL with typed rows) `re` (linear-time regex) `html` (parser with CSS selectors) `bytes hash time rand`,
  full Unicode text.
- **Concurrency:** `spawn`, `par_map` and channels on a thread pool, no async/await, with deadlock detection.
- **Back end:** Drift to C11 to `gcc`, a reference-counted runtime, binaries with no link-time dependencies.
- **Bare metal:** the `hw` effect and `drift build --kernel` for writing kernels in Drift.

## Repository

| path | contents |
|---|---|
| [`CARD.md`](CARD.md) | the complete language, sized for a prompt |
| [`DESIGN.md`](DESIGN.md) | why each decision was made, the compilation pipeline, the roadmap |
| `src/` | the compiler (Rust), the runtime (`src/runtime`, C) and the standard library (`src/std`) |
| `examples/` | small Drift programs |
| `tests/` | the compiler and runtime test suite (`cargo test --release`) |
| `eval/` | the tasks, harness and results of Drift vs Python vs Rust with agents |
| `ports/` | real code ported to Drift |

## Roadmap

Faster scalar code and unboxed lists, a second eval run, a self-hosted compiler, a native back end, more targets.
See [`DESIGN.md`](DESIGN.md).

## License

MIT. See [`LICENSE`](LICENSE).
