//! `drift mcp`: a Model Context Protocol server on stdio (JSON-RPC 2.0, one message per line).
//! Every tool runs this same binary as a subprocess, so a crash or a runaway program never takes the
//! server down; results come back as structured JSON (`structuredContent`) and as text.

use crate::json::{parse, Json};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_OUT: usize = 100_000;
const INSTRUCTIONS: &str = "Drift is a compiled, statically typed language with declared effects. Before writing Drift, read the \
resource drift://card (or call drift_card): it is the whole language. Loop: write code (leave `todo` holes when unsure) -> \
drift_check (every diagnostic has code, span, expected/found and, when certain, a fix; use fix=true to apply them) -> \
drift_test (runs `examples` and `test` blocks) -> drift_run. Tools take `path` (file or project directory) or `source` (one \
file's text). Programs run sandboxed by the kernel according to the `uses` of `main`.";

fn tools() -> Json {
    let path_or_source = |extra: Vec<(&str, Json)>, required: Vec<&str>| {
        let mut props = vec![
            ("path", Json::obj(vec![("type", Json::str("string")), ("description", Json::str("a .dr file or a project directory (with src/)"))])),
            ("source", Json::obj(vec![("type", Json::str("string")), ("description", Json::str("the text of a single .dr file, instead of path"))])),
        ];
        props.extend(extra);
        Json::obj(vec![
            ("type", Json::str("object")),
            ("properties", Json::obj(props)),
            ("required", Json::Arr(required.into_iter().map(Json::str).collect())),
        ])
    };
    let boolean = |d: &str| Json::obj(vec![("type", Json::str("boolean")), ("description", Json::str(d))]);
    let tool = |name: &str, title: &str, desc: &str, schema: Json, read_only: bool| {
        Json::obj(vec![
            ("name", Json::str(name)),
            ("title", Json::str(title)),
            ("description", Json::str(desc)),
            ("inputSchema", schema),
            ("annotations", Json::obj(vec![("readOnlyHint", Json::Bool(read_only)), ("openWorldHint", Json::Bool(false))])),
        ])
    };
    Json::Arr(vec![
        tool("drift_card", "Language card", "The complete Drift language card (syntax, types, effects, std, toolchain). Read it before writing Drift.", Json::obj(vec![("type", Json::str("object")), ("properties", Json::obj(vec![]))]), true),
        tool("drift_check", "Check", "All diagnostics (syntax, types, effects, layout) as structured objects, each with code, span, message, expected/found and a certain fix when there is one. With fix=true the fixes are applied (to the files, or returned as `fixed_source`).", path_or_source(vec![("fix", boolean("apply every certain fix"))], vec![]), false),
        tool("drift_test", "Test", "Run every `examples` and `test` block, each in its own sandboxed process. Returns each test's name, line, ok and output.", path_or_source(vec![], vec![]), true),
        tool("drift_run", "Run", "Check, compile and run `main`. Returns exit_code, stdout, stderr; if the program does not check, returns the diagnostics instead.", path_or_source(vec![
            ("args", Json::obj(vec![("type", Json::str("array")), ("items", Json::obj(vec![("type", Json::str("string"))])), ("description", Json::str("command-line arguments"))])),
            ("stdin", Json::obj(vec![("type", Json::str("string")), ("description", Json::str("text given on standard input"))])),
            ("timeout_ms", Json::obj(vec![("type", Json::str("integer")), ("description", Json::str("default 30000, at most 600000"))])),
        ], vec![]), false),
        tool("drift_fmt", "Format", "Canonical formatting. With `source`, returns the formatted text; with `path`, rewrites the files (check=true only lists them).", path_or_source(vec![("check", boolean("only report which files would change"))], vec![]), false),
        tool("drift_outline", "Outline", "Signatures and doc comments of a module: `std` lists the std modules, `std.http` one of them, or a path to a .dr file.", Json::obj(vec![
            ("type", Json::str("object")),
            ("properties", Json::obj(vec![("target", Json::obj(vec![("type", Json::str("string")), ("description", Json::str("std, std.<module> or a file path"))]))])),
            ("required", Json::Arr(vec![Json::str("target")])),
        ]), true),
        tool("drift_build", "Build", "Native executable (gcc -O2) at `output`.", path_or_source(vec![("output", Json::obj(vec![("type", Json::str("string")), ("description", Json::str("path of the executable"))]))], vec!["output"]), false),
    ])
}

struct Out {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    timed_out: bool,
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_OUT {
        let mut cut = MAX_OUT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        let total = s.len();
        s.truncate(cut);
        s.push_str(&format!("\n... [{} more bytes]", total - cut));
    }
    s
}

fn run_drift(args: &[String], stdin: Option<&str>, timeout: Duration) -> Out {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("drift"));
    let mut cmd = Command::new(exe);
    cmd.args(args).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.process_group(0); // the program `drift run` starts is killed with it on timeout
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Out { stdout: String::new(), stderr: format!("cannot start drift: {}", e), code: None, timed_out: false },
    };
    if let (Some(text), Some(mut w)) = (stdin, child.stdin.take()) {
        let text = text.to_string();
        std::thread::spawn(move || {
            let _ = w.write_all(text.as_bytes());
        });
    }
    let reader = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let so = reader(child.stdout.take().map(|x| Box::new(x) as Box<dyn Read + Send>));
    let se = reader(child.stderr.take().map(|x| Box::new(x) as Box<dyn Read + Send>));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() > timeout => {
                timed_out = true;
                let _ = Command::new("kill").arg("-KILL").arg(format!("-{}", child.id())).status();
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
    };
    let code = status.and_then(|s| s.code());
    Out { stdout: clip(so.join().unwrap_or_default()), stderr: clip(se.join().unwrap_or_default()), code, timed_out }
}

static TMP: AtomicU64 = AtomicU64::new(0);

/// A `source` argument becomes a temporary file (removed afterwards); `path` is used as given.
struct Target {
    path: String,
    tmp: Option<PathBuf>,
}

impl Target {
    fn from(args: &Json) -> Result<Target, String> {
        if let Some(src) = args.get("source").and_then(|s| s.as_str()) {
            let dir = std::env::temp_dir().join(format!("drift-mcp-{}-{}", std::process::id(), TMP.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let file = dir.join("main.dr");
            std::fs::write(&file, src).map_err(|e| e.to_string())?;
            return Ok(Target { path: file.to_string_lossy().into_owned(), tmp: Some(dir) });
        }
        match args.get("path").and_then(|s| s.as_str()) {
            Some(p) if Path::new(p).exists() => Ok(Target { path: p.to_string(), tmp: None }),
            Some(p) => Err(format!("no such file or directory: {}", p)),
            None => Err("pass `path` (a .dr file or project directory) or `source` (the text of one file)".into()),
        }
    }
    /// Diagnostics of a `source` name the file "main.dr" rather than the temporary path.
    fn clean(&self, s: &str) -> String {
        if self.tmp.is_some() { s.replace(&self.path, "main.dr") } else { s.to_string() }
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        if let Some(d) = &self.tmp {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn json_lines(text: &str) -> Vec<Json> {
    text.lines().filter_map(|l| parse(l.trim()).ok()).filter(|v| matches!(v, Json::Obj(_))).collect()
}

fn count(diags: &[Json], sev: &str) -> usize {
    diags.iter().filter(|d| d.get("severity").and_then(|s| s.as_str()) == Some(sev)).count()
}

fn check(t: &Target, fix: bool) -> (Vec<Json>, Out) {
    let mut args = vec!["check".to_string(), "--json".to_string()];
    if fix {
        args.push("--fix".into());
    }
    args.push(t.path.clone());
    let out = run_drift(&args, None, Duration::from_secs(120));
    (json_lines(&t.clean(&out.stdout)), out)
}

fn num(n: usize) -> Json {
    Json::Num(n as f64)
}

/// (structured result, is_error)
fn call(name: &str, args: &Json) -> Result<(Json, bool), String> {
    match name {
        "drift_card" => Ok((Json::obj(vec![("card", Json::str(crate::CARD))]), false)),
        "drift_outline" => {
            let target = args.get("target").and_then(|t| t.as_str()).ok_or("`target` is required")?;
            let out = run_drift(&["outline".into(), target.into()], None, Duration::from_secs(60));
            let ok = out.code == Some(0);
            Ok((Json::obj(vec![("outline", Json::str(if ok { out.stdout } else { out.stderr }))]), !ok))
        }
        "drift_check" => {
            let t = Target::from(args)?;
            let fix = args.get("fix").and_then(|f| f.as_bool()).unwrap_or(false);
            let (diags, out) = check(&t, fix);
            let errors = count(&diags, "error");
            let mut res = vec![
                ("ok", Json::Bool(errors == 0 && out.code == Some(0))),
                ("errors", num(errors)),
                ("warnings", num(count(&diags, "warning"))),
                ("diagnostics", Json::Arr(diags)),
            ];
            if fix && t.tmp.is_some() {
                res.push(("fixed_source", Json::str(std::fs::read_to_string(&t.path).unwrap_or_default())));
            }
            if out.code.unwrap_or(2) >= 2 {
                res.push(("stderr", Json::str(t.clean(&out.stderr))));
            }
            Ok((Json::obj(res), false))
        }
        "drift_fmt" => {
            let t = Target::from(args)?;
            let only = args.get("check").and_then(|f| f.as_bool()).unwrap_or(false);
            let mut a = vec!["fmt".to_string()];
            if only && t.tmp.is_none() {
                a.push("--check".into());
            }
            a.push(t.path.clone());
            let out = run_drift(&a, None, Duration::from_secs(60));
            let ok = out.code == Some(0);
            let mut res = vec![("ok", Json::Bool(ok)), ("report", Json::str(t.clean(&format!("{}{}", out.stdout, out.stderr)).trim().to_string()))];
            if t.tmp.is_some() && ok {
                res.push(("formatted", Json::str(std::fs::read_to_string(&t.path).unwrap_or_default())));
            }
            Ok((Json::obj(res), !ok))
        }
        "drift_test" | "drift_run" | "drift_build" => {
            let t = Target::from(args)?;
            let (diags, _) = check(&t, false);
            // same rule as the driver: layout (F0001) is reported but never stops a program from running
            let blocking = |d: &Json| d.get("severity").and_then(|s| s.as_str()) == Some("error") && d.get("code").and_then(|c| c.as_str()) != Some("F0001");
            if diags.iter().any(blocking) {
                return Ok((Json::obj(vec![("ok", Json::Bool(false)), ("phase", Json::str("check")), ("diagnostics", Json::Arr(diags))]), true));
            }
            let warnings: Vec<Json> = diags;
            if name == "drift_test" {
                let out = run_drift(&["test".into(), "--json".into(), t.path.clone()], None, Duration::from_secs(600));
                let tests = json_lines(&t.clean(&out.stdout));
                let passed = tests.iter().filter(|x| x.get("ok").and_then(|o| o.as_bool()) == Some(true)).count();
                let mut res = vec![
                    ("ok", Json::Bool(out.code == Some(0) && passed == tests.len())),
                    ("passed", num(passed)),
                    ("failed", num(tests.len() - passed)),
                    ("tests", Json::Arr(tests)),
                ];
                if out.code != Some(0) && !out.stderr.is_empty() {
                    res.push(("stderr", Json::str(t.clean(&out.stderr))));
                }
                return Ok((Json::obj(res), false));
            }
            if name == "drift_build" {
                let output = args.get("output").and_then(|o| o.as_str()).ok_or("`output` is required")?;
                let out = run_drift(&["build".into(), t.path.clone(), "-o".into(), output.into()], None, Duration::from_secs(600));
                let ok = out.code == Some(0);
                return Ok((Json::obj(vec![("ok", Json::Bool(ok)), ("output", Json::str(output)), ("report", Json::str(t.clean(&format!("{}{}", out.stdout, out.stderr)).trim().to_string()))]), !ok));
            }
            let mut a = vec!["run".to_string(), t.path.clone()];
            if let Some(list) = args.get("args").and_then(|x| x.as_arr()) {
                a.extend(list.iter().filter_map(|x| x.as_str()).map(String::from));
            }
            let ms = args.get("timeout_ms").and_then(|x| x.as_f64()).unwrap_or(30000.0).clamp(100.0, 600000.0) as u64;
            let out = run_drift(&a, args.get("stdin").and_then(|x| x.as_str()), Duration::from_millis(ms));
            let mut res = vec![
                ("exit_code", out.code.map(|c| Json::Num(c as f64)).unwrap_or(Json::Null)),
                ("stdout", Json::str(out.stdout.clone())),
                ("stderr", Json::str(t.clean(&out.stderr))),
                ("timed_out", Json::Bool(out.timed_out)),
            ];
            if !warnings.is_empty() {
                res.push(("warnings", Json::Arr(warnings)));
            }
            Ok((Json::obj(res), out.timed_out))
        }
        _ => Err(format!("unknown tool `{}`", name)),
    }
}

fn reply(id: &Json, result: Json) -> String {
    Json::obj(vec![("jsonrpc", Json::str("2.0")), ("id", id.clone()), ("result", result)]).to_string()
}

fn error(id: &Json, code: i64, msg: &str) -> String {
    Json::obj(vec![
        ("jsonrpc", Json::str("2.0")),
        ("id", id.clone()),
        ("error", Json::obj(vec![("code", Json::Num(code as f64)), ("message", Json::str(msg))])),
    ])
    .to_string()
}

/// Handle one message; None for notifications.
pub fn handle(line: &str) -> Option<String> {
    let msg = match parse(line) {
        Ok(m) => m,
        Err(e) => return Some(error(&Json::Null, -32700, &format!("parse error: {}", e))),
    };
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let Some(id) = id else {
        return None; // notifications/initialized, notifications/cancelled, ...
    };
    let params = msg.get("params").cloned().unwrap_or(Json::Obj(vec![]));
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(VERSIONS[0]);
            let version = if VERSIONS.contains(&asked) { asked } else { VERSIONS[0] };
            reply(
                &id,
                Json::obj(vec![
                    ("protocolVersion", Json::str(version)),
                    ("capabilities", Json::obj(vec![("tools", Json::obj(vec![("listChanged", Json::Bool(false))])), ("resources", Json::obj(vec![]))])),
                    ("serverInfo", Json::obj(vec![("name", Json::str("drift")), ("title", Json::str("Drift compiler")), ("version", Json::str(env!("CARGO_PKG_VERSION")))])),
                    ("instructions", Json::str(INSTRUCTIONS)),
                ]),
            )
        }
        "ping" => reply(&id, Json::obj(vec![])),
        "tools/list" => reply(&id, Json::obj(vec![("tools", tools())])),
        "resources/list" => reply(
            &id,
            Json::obj(vec![(
                "resources",
                Json::Arr(vec![Json::obj(vec![
                    ("uri", Json::str("drift://card")),
                    ("name", Json::str("card")),
                    ("title", Json::str("Drift language card")),
                    ("mimeType", Json::str("text/markdown")),
                ])]),
            )]),
        ),
        "resources/read" => match params.get("uri").and_then(|u| u.as_str()) {
            Some("drift://card") => reply(
                &id,
                Json::obj(vec![(
                    "contents",
                    Json::Arr(vec![Json::obj(vec![("uri", Json::str("drift://card")), ("mimeType", Json::str("text/markdown")), ("text", Json::str(crate::CARD))])]),
                )]),
            ),
            _ => error(&id, -32002, "resource not found"),
        },
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Json::Obj(vec![]));
            match call(name, &args) {
                Ok((structured, is_error)) => {
                    let text = match (name, &structured) {
                        ("drift_card", s) => s.get("card").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                        ("drift_outline", s) => s.get("outline").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                        (_, s) => s.to_string(),
                    };
                    reply(
                        &id,
                        Json::obj(vec![
                            ("content", Json::Arr(vec![Json::obj(vec![("type", Json::str("text")), ("text", Json::str(text))])])),
                            ("structuredContent", structured),
                            ("isError", Json::Bool(is_error)),
                        ]),
                    )
                }
                Err(e) if e.starts_with("unknown tool") => error(&id, -32602, &e),
                Err(e) => reply(
                    &id,
                    Json::obj(vec![
                        ("content", Json::Arr(vec![Json::obj(vec![("type", Json::str("text")), ("text", Json::str(e))])])),
                        ("isError", Json::Bool(true)),
                    ]),
                ),
            }
        }
        _ => error(&id, -32601, &format!("method not found: {}", method)),
    })
}

pub fn serve() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(out) = handle(&line) {
            writeln!(stdout, "{}", out)?;
            stdout.flush()?;
        }
    }
    Ok(())
}
