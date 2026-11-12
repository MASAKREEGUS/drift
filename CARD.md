# Drift v0 — language card

Drift is a statically typed, compiled language built to be written, read and repaired by AI agents.
This card is the complete language. If something is not here, it does not exist.
Files end in `.dr`. One file = one module; module path = file path (`src/shop/offers.dr` → `shop.offers`).

## Invariants
- One canonical form. `drift fmt` output is the only accepted layout; `drift check` reports unformatted code with a fix.
- No null, no exceptions, no implicit conversions, no undefined behavior, no shadowing, no global mutable state, no macros, no operator overloading, no inheritance.
- Every function signature is complete: parameter types, return type, effects. Local types are inferred.
- Errors are values (`Result[T, E]`, `Option[T]`); `?` propagates them. `panic` is for bugs only.
- Pure by default. Side effects must be declared with `uses`.
- Statements end at newline. A line continues when it ends with a binary operator, `,`, `(`, `[`, `{`, `=>`, or the next line starts with `.`.

## Lexical
```
// comment            /// doc comment, attached to the next item
42  1_000  0xff  0b1010  3.14  1e-9  true  false
"text {expr} text"    escapes: \n \t \r \\ \" \{ \u{1F600}
r"raw, {not} interpolated"     b"bytes"     """multi-line, common indent stripped"""
r"""raw multi-line: HTML, JSON, CSS, regex as-is; no escapes, no {interpolation}"""
```
Interpolations hold one expression, string literals included: `"{m.get("k").unwrap_or("-")}"`.
Naming is enforced: `snake_case` for values, functions, fields, modules; `PascalCase` for types and variants; `UPPER_CASE` for consts.

## Types
```
Int (i64)  I32  U8  U32  U64  Float (f64)  F32  Bool  Str (UTF-8)  Bytes  ()  (unit)  Never (`panic`, `return`)
List[T]  Map[K, V] (insertion-ordered)  Set[T]  Option[T]  Result[T, E]
(A, B)  tuple          fn(A, B) -> R  function type (effects: fn(A) -> R uses net)
```
- Value semantics everywhere. There are no references or pointers. Copying is O(1) (refcount + copy-on-write).
- Literals: `[1, 2]` List, `{"k": 1}` Map, `{}` empty Map, `Set.of([1, 2])`, `(a, b)` tuple. Trailing commas allowed in multi-line literals.
- Optional local annotation: `let xs: List[Int] = []`. Required only when the type cannot be inferred.
- `+` concatenates `Str`, `Bytes` and `List`. Float `<` is IEEE total order, so sorting floats is defined.
- Resources (`fs.File`, sockets, `db.Conn`, `Task[T]`) are opaque handle values; the OS resource is released when the last copy is dropped.
- Integer overflow, division by zero and out-of-range indexing `xs[i]` panic. Use `xs.get(i)` for `Option`.
- Mixing numeric types is an error: `n.to_float()`, `x.to_int()` (truncates), `n.to_u8()?` (checked, returns Option).
- Every struct/enum gets `==`, `<` (ordering by declaration), `hash` and `show` when all of its fields support them.

## Items
```
pub struct Point { x: Float, y: Float }

pub enum Shape {
  Circle { r: Float }
  Rect { w: Float, h: Float }
  Empty
}

pub const MAX_RETRIES: Int = 3                   // the only global binding allowed

pub type Prices = Map[Str, Int]                   // alias

pub fn area(self: Shape) -> Float { ... }         // first param `self` ⇒ method, called only as shape.area()
pub fn grow(var self: Point, by: Float) -> () { self.x += by }   // `var self` mutates the caller's var
fn add(a: Int, b: Int) -> Int { a + b }           // the last expression is the result
fn first[T](xs: List[T]) -> Option[T] { xs.get(0) }
pub fn Point.origin() -> Point { Point { x: 0.0, y: 0.0 } }   // associated fn: Point.origin()
```
- Methods must live in the module that defines the type and are called only as `x.f(a)`. Associated functions `fn T.f(...)` (no `self`) are called as `T.f(...)`.
- Construction: `Point { x: 1.0, y: 2.0 }`, shorthand `Point { x, y }`, `Shape.Circle { r: 2.0 }`, `Shape.Empty`.
- Functional update: `{ p with x: 5.0 }`.
- Generic constraints are inferred from use of `==`, `<`, `hash`, `show`; no user traits in v0.

## Bindings and mutation
```
let x = 1            // immutable
var total = 0        // mutable, local only
total += x           // = += -= *= /= %=
var xs = [1, 2]
xs.push(3)           // methods with `var self` require a `var` receiver
var p = Point { x: 0.0, y: 0.0 }
p.x = 2.0
```
Parameters are immutable unless declared `var self`. To change a value elsewhere, return a new one.

## Expressions
```
+ - * / %            (Int / Int truncates toward zero)
== != < <= > >=      and  or  not      a..b  a..=b (ranges)      xs[i]  m[k] (panics if missing)
if c { a } else { b }                          // an expression; `else` required when used as value
match shape {
  Shape.Circle { r } => 3.14159 * r * r
  Shape.Rect { w, h } if w == h => w * w
  Shape.Rect { w, h } => w * h
  Shape.Empty => 0.0
}
```
- `match` must be exhaustive (missing cases are reported as patterns) and unreachable arms warn. Or-patterns cannot bind names. Patterns: literals, names, `_`, `Variant { f, f: pat, .. }`, `(a, b)`, `[x, ..rest]`, `[]`, `Some(x)`, `None`, `Ok(x)`, `Err(e)`, `a | b`.
- Closures: `|x| x * 2`, `|(k, v)| k`, `|a, b| { let s = a + b; s * s }`, annotated `|x: Int| -> Result[Int, E] { ... }`. Params are patterns. Closures capture by value and cannot assign captured variables. Inside a closure, `?` and `return` exit the closure, not the enclosing function.
- `{` in expression position starts a Map literal or a `{ v with ... }` update. A block `{ ... }` appears only after `fn`, `if`, `else`, `for`, `while`, `loop`, `=>`, `|...|`, `examples`, `test`. To return a Map from a closure or arm, wrap it: `=> ({"k": 1})`.
- Struct literals cannot appear bare in the head of `if`/`while`/`for`/`match`: write `if p == (Point { x: 0.0, y: 0.0 }) { ... }`.
- Blocks separate statements with newlines; `;` is allowed only to join statements on one line.
- Chains instead of loops when it reads clearly: `xs.filter(|x| x > 0).map(|x| x * 2).sum()`.
- `?` on `Result` returns `Err` early; on `Option` returns `None` early (only inside a function returning `Option`; otherwise `.ok_or(err)?`).
- A `Result` used as a statement must be handled: ignoring one is an error.
- No `unwrap`: use `?` or `.unwrap_or(default)`. There is no char type: `.chars()` yields one-character `Str`s.
- `todo` is a typed hole: it type-checks as anything, `drift check` reports it (severity `hole`, not an error) with its expected type and the names in scope, and `drift build` rejects it.

## Statements
```
for x in xs { }      for (k, v) in m { }      for (i, x) in xs.enumerate() { }      for i in 0..n { }
while cond { }       loop { }                 break      continue      return value
assert cond, "message"
panic("message")
```

## Effects
```
pub fn fetch(url: Str) -> Result[Str, HttpError] uses net {
  http.get(url)?.text()
}
pub fn main() -> Result[(), Error] uses io, net {
  io.println(fetch("https://example.com")?)
  Ok(())
}
```
- Effects: `io` (stdin/stdout/stderr), `fs`, `net`, `db`, `env`, `time`, `rand`, `proc`, `spawn`.
- A function may call another only if the callee's effects are a subset of its own. Closures inherit the effects of what they call.
- `main` declares what the program may do and the kernel enforces it (Landlock + seccomp): without `fs` no file can be opened, without `net` no socket, without `proc` no exec or fork. Each `test` block runs sandboxed with its own `uses`; `examples` get nothing. `DRIFT_SANDBOX=off` disables it, `DRIFT_SANDBOX_REPORT=1` shows what is enforced.

## Contracts and examples
```
fn percent(part: Int, whole: Int) -> Int
  requires whole > 0
  requires part >= 0
  ensures result >= 0
{
  part * 100 / whole
}

examples percent {
  percent(20, 100) == 20
  percent(1, 3) == 33
}

test "reads fixture" uses fs {
  let html = fs.read_str("fixtures/page.html")?
  assert html.contains("<title>"), "fixture has a title"
}
```
- `requires`/`ensures` are checked in every build except `--unchecked`. `result` names the return value inside `ensures`.
- `examples` must be pure and are run by `drift test`; they double as documentation. `test` blocks may declare effects and may use `?`.

## Errors
```
pub enum AppError {
  Http(HttpError)
  Parse(ParseError)
  NotFound { id: Str }
}
```
`?` converts automatically when the function's error enum has exactly one single-field variant wrapping the source error.
`Error` (`{ message: Str }`, built with `error("text {x}")`) is the catch-all for scripts: `?` converts any error into `Error` using its `show`.
`main` returning `Err(e)` prints `error: ...` and exits with status 1. A panic prints `panic at file:line: message` and exits with status 101.

## Concurrency
No async/await, no function coloring. Tasks run in parallel on a thread pool (one running thread per core; a task blocked on network, db, sleep, `join` or a channel frees its core).
```
let t = spawn fetch(url)                       // requires `uses spawn` + fetch's effects; `spawn` takes a call
let body = t.join()                            // the value fetch returned
let pages = urls.par_map(|u| fetch(u))         // in order; all finish before the next line
let (tx, rx) = channel[Int](16)                // capacity 0: send waits for the receiver
tx.send(1)
tx.close()                                     // the producer closes when done
let next = rx.recv()                           // Option[Int]; None once closed and drained
```
Values are immutable, so tasks share them safely; the call after `spawn` runs in the task with the values it uses captured at that moment. A program where every task waits forever panics with `deadlock: ...` (usually a sender that was never closed). `http.serve` handles each connection in its own task.

## Modules and packages
```
import shop.offers.{discount, Offer}
import shop.offers                 // then: offers.discount(...)
```
Every std module is always in scope (`io.println`, `fs.read_str`, `http.get`, `json.encode`); never import it. `pub` exports; everything else is module-private. No import cycles. A project is a directory with `drift.toml`; sources in `src/`, entry `src/main.dr`.

## Standard library (signatures: `drift outline std.<module>`)
`io fs net http json str list map set math time rand env proc db (sqlite, postgres) re (regex) html (parse, select) bytes hash (sha256, fnv) log`
- `env.args()` starts with the program name: the first argument is `env.args().get(1)`.
- `json.decode` needs the target type at the call: `let o: Offer = json.decode(text)?` or `json.decode[List[Offer]](text)?`.
- `env.get("NAME")` → `Option[Str]`.
- Text is Unicode and behaves like Python's `str`: `to_lower`/`to_upper` (full mappings, "ß" → "SS"), `trim` and `is_whitespace` use Unicode spaces, regex `\w`/`\b` use Unicode letters and digits (`\d` is ASCII). Accent folding: `s.to_lower().nfkd().strip_marks()` ("Ñandú" → "nandu").
- `hash.sha1(bytes)` / `hash.sha256(bytes)` → lowercase hex `Str`; `hash.fnv1a(bytes)` → `Int`. Sets: `union`, `intersection`, `difference`, `is_subset`, `is_empty`.
- `db.open(":memory:" | "file.db" | "postgres://user@host/db")?`, then `c.exec(sql, params)?` (rows changed), `c.query[Row](sql, params)?` → `List[Row]`, `c.query_one[Row](sql, params)?` → `Option[Row]` (`Err` if several), `c.exec_script(sql)?` for several statements. Placeholders are `$1, $2` on both backends; `params` is `()`, one value or a tuple `(id, name)`. `Row` is a struct (fields matched to columns by name), a tuple (by position) or one value; columns are Int/Float/Bool/Str/Bytes, NULL needs `Option`. `e.code` is the SQLSTATE on both backends (`"23505"` unique violation).
- `http.get`/`http.post` follow redirects and return `Err` (with `.status`) for status >= 400; `http.request(method, url, headers, body)` returns any status. `.text()` honors the response charset.
- `re.compile(r"(\d+)-(\d+)")?` then `.find`, `.find_all`, `.is_match`, `.replace_all(text, "$2/$1")`, `.split`; `Match` has `text`, `start`, `end` (character indices) and `groups: List[Option[Str]]`. Linear time; no backreferences or lookaround.
- `html.parse(src).select("ul#list > li.item:not(.agotado)")` → `List[html.Node]`; a node has `.text()` (whitespace collapsed), `.attr("href")`, `.attrs()`, `.tag()`, `.html()`, `.inner_html()`, `.children()`, `.parent()`, `.select(css)`, `.select_first(css)`.

## Toolchain — single binary `drift`
| command | purpose |
|---|---|
| `drift check [--json]` | all diagnostics at once, each with `code`, `span`, `message`, `expected`, `found`, `fix` (machine-applicable edit) |
| `drift check --fix` | apply every certain fix (syntax, layout, …) in place, then report what is left |
| `drift run <file\|dir> [args...]` | check, compile through C and run `main`; `args` reach `env.args()` |
| `drift test [--json] <file\|dir>` | run every `examples` and `test` block, each isolated; a `todo` fails only the test that reaches it |
| `drift build <file\|dir> [-o out]` | native executable through C (`gcc -O2`; no runtime dependency) |
| `drift fmt [--check]` | canonical layout (width 100, 2 spaces); comments and single blank lines are kept |
| `drift outline <module>` | compact signatures, types and effects of a module |
| `drift explain <code>` | full explanation of a diagnostic |
| `drift mcp` | MCP server on stdio: `drift_check` (fix), `drift_test`, `drift_run` (stdin, args, timeout), `drift_fmt`, `drift_outline`, `drift_build`, `drift_card`; each takes `path` or `source` |
| `drift card` | prints this card |
