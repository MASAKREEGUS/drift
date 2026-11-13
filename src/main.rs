use drift::check::SourceFile;
use drift::driver::{analyze, fix_files};
use drift::diag::{render_human, render_json, LineIndex};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "drift — compiler for the Drift language

usage:
  drift check [--json] [--fix] <file|dir>...   all diagnostics (syntax, types, effects, layout); --fix applies every certain fix
  drift fmt [--check] <file|dir>...            rewrite files in canonical form; --check only lists them
  drift run <file|dir> [args...]               check, compile through C and run `main`
  drift build <file|dir> [-o out]              native executable (gcc -O2)
  drift build --kernel <file|dir> -o <dir>     freestanding C for a kernel: <dir>/kernel.c, drift_rt.c, drift_rt.h (entry `drift_kmain`)
  drift test [--json] <file|dir>...            run every `examples` and `test` block, each in its own process
  drift outline <std.module|file>              signatures and types of a module
  drift parse <file> [--ast]                   parse one file; --ast prints the tree
  drift card                                   print the language card
  drift mcp                                    Model Context Protocol server on stdio (tools: check, test, run, fmt, outline, build, card)
  drift --version";

struct Input {
    path: PathBuf,
    module: String,
}

fn collect(root: &Path, path: &Path, out: &mut Vec<Input>) -> std::io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if p.is_dir() || p.extension().and_then(|e| e.to_str()) == Some("dr") {
                collect(root, &p, out)?;
            }
        }
    } else {
        let rel = path.strip_prefix(root).unwrap_or(path).with_extension("");
        let mut parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect();
        // a `src` directory is a project root: `app/src/shop/offers.dr` is module `shop.offers`
        if let Some(i) = parts[..parts.len() - 1].iter().rposition(|p| p == "src") {
            parts.drain(..=i);
        }
        out.push(Input { path: path.to_path_buf(), module: parts.join(".") });
    }
    Ok(())
}

fn inputs(args: &[String]) -> Result<Vec<Input>, ExitCode> {
    let args: Vec<String> = args.to_vec();
    let args = &args[..];
    let mut files = Vec::new();
    for a in args.iter().filter(|a| !a.starts_with("--")) {
        let p = Path::new(a);
        let root = if p.is_dir() { p.to_path_buf() } else { p.parent().map(|x| x.to_path_buf()).unwrap_or_default() };
        if let Err(e) = collect(&root, p, &mut files) {
            eprintln!("drift: cannot read {}: {}", a, e);
            return Err(ExitCode::from(2));
        }
    }
    if files.is_empty() {
        eprintln!("{}", USAGE);
        return Err(ExitCode::from(2));
    }
    Ok(files)
}

fn load(ins: &[Input]) -> Result<Vec<SourceFile>, ExitCode> {
    let mut files = Vec::new();
    for i in ins {
        match std::fs::read_to_string(&i.path) {
            Ok(src) => files.push(SourceFile { name: i.path.display().to_string(), src, module: i.module.clone(), is_std: false }),
            Err(e) => {
                eprintln!("drift: cannot read {}: {}", i.path.display(), e);
                return Err(ExitCode::from(2));
            }
        }
    }
    Ok(files)
}

/// Print diagnostics; returns (errors, holes).
fn report(files: &[SourceFile], diags: &[Vec<drift::diag::Diag>], json: bool) -> (usize, usize) {
    let (mut errors, mut holes) = (0, 0);
    for (i, ds) in diags.iter().enumerate() {
        let idx = LineIndex::new(&files[i].src);
        for d in ds {
            if d.is_error() {
                errors += 1;
            }
            if d.severity == drift::diag::Severity::Hole {
                holes += 1;
            }
            if json {
                println!("{}", render_json(d, &files[i].name, &idx));
            } else {
                print!("{}", render_human(d, &files[i].name, &idx));
            }
        }
    }
    (errors, holes)
}

/// Check and compile to an executable; diagnostics go to stdout like `drift check`.
fn build_exe(path: &str, test: bool, opt: &str) -> Result<PathBuf, ExitCode> {
    let ins = inputs(&[path.to_string()])?;
    let files = load(&ins)?;
    let c = match drift::driver::compile(&files, test) {
        Ok(c) => c,
        Err(diags) => {
            let (errors, holes) = report(&files, &diags, false);
            if errors == 0 && holes > 0 {
                eprintln!("drift: {} `todo` hole(s) left; fill them before running (`drift test` runs the finished parts)", holes);
            } else {
                eprintln!("drift: {} error(s); nothing was run", errors);
            }
            return Err(ExitCode::from(1));
        }
    };
    drift::toolchain::link(&c, opt).map_err(|e| {
        eprintln!("drift: {}", e);
        ExitCode::from(3)
    })
}

fn exit_of(status: std::process::ExitStatus) -> ExitCode {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(c) => ExitCode::from(c as u8),
        None => ExitCode::from(128 + status.signal().unwrap_or(0) as u8),
    }
}

fn run_cmd(args: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let Some(path) = args.first() else {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    };
    let exe = match build_exe(path, false, "-O1") {
        Ok(e) => e,
        Err(c) => return c,
    };
    match std::process::Command::new(&exe).arg0(path).args(&args[1..]).status() {
        Ok(st) => exit_of(st),
        Err(e) => {
            eprintln!("drift: cannot run {}: {}", exe.display(), e);
            ExitCode::from(3)
        }
    }
}

fn build_cmd(args: &[String]) -> ExitCode {
    let mut out: Option<String> = None;
    let mut path: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-o" {
            out = it.next().cloned();
        } else if !a.starts_with('-') {
            path = Some(a.clone());
        }
    }
    let Some(path) = path else {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    };
    if args.iter().any(|a| a == "--kernel") {
        return kernel_cmd(&path, out.as_deref().unwrap_or("."));
    }
    let exe = match build_exe(&path, false, "-O2") {
        Ok(e) => e,
        Err(c) => return c,
    };
    let out = out.unwrap_or_else(|| {
        Path::new(&path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "a.out".into())
    });
    if let Err(e) = std::fs::copy(&exe, &out) {
        eprintln!("drift: cannot write {}: {}", out, e);
        return ExitCode::from(2);
    }
    eprintln!("built {}", out);
    ExitCode::SUCCESS
}

/// Emit the generated C and the runtime sources for a freestanding kernel build.
fn kernel_cmd(path: &str, dir: &str) -> ExitCode {
    let files = match inputs(&[path.to_string()]).and_then(|ins| load(&ins)) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let c = match drift::driver::compile_for(&files, false, true) {
        Ok(c) => c,
        Err(diags) => {
            let (errors, holes) = report(&files, &diags, false);
            eprintln!("drift: {} error(s), {} hole(s); nothing was built", errors, holes);
            return ExitCode::from(1);
        }
    };
    let dir = Path::new(dir);
    let outs = [("kernel.c", c.as_str()), ("drift_rt.c", drift::codegen::RT_C), ("drift_rt.h", drift::codegen::RT_H)];
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("drift: cannot create {}: {}", dir.display(), e);
        return ExitCode::from(2);
    }
    for (name, text) in outs {
        if let Err(e) = std::fs::write(dir.join(name), text) {
            eprintln!("drift: cannot write {}: {}", dir.join(name).display(), e);
            return ExitCode::from(2);
        }
    }
    eprintln!("built {}/kernel.c (entry `drift_kmain`)", dir.display());
    ExitCode::SUCCESS
}

fn test_cmd(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if paths.is_empty() {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    }
    let mut status = ExitCode::SUCCESS;
    for p in paths {
        let exe = match build_exe(p, true, "-O1") {
            Ok(e) => e,
            Err(c) => return c,
        };
        let mut cmd = std::process::Command::new(&exe);
        if json {
            cmd.arg("--json");
        }
        match cmd.status() {
            Ok(st) if st.success() => {}
            Ok(st) => status = exit_of(st),
            Err(e) => {
                eprintln!("drift: cannot run {}: {}", exe.display(), e);
                return ExitCode::from(3);
            }
        }
    }
    status
}

fn check(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let fix = args.iter().any(|a| a == "--fix");
    let ins = match inputs(args) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let mut files = match load(&ins) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let mut fixed = vec![false; files.len()];
    if fix {
        fixed = fix_files(&mut files);
        for (i, f) in files.iter().enumerate() {
            if fixed[i] {
                if let Err(e) = std::fs::write(&ins[i].path, &f.src) {
                    eprintln!("drift: cannot write {}: {}", f.name, e);
                    return ExitCode::from(2);
                }
            }
        }
    }
    let diags = analyze(&files);
    let (errors, holes) = report(&files, &diags, json);
    if !json {
        eprintln!(
            "checked {} file(s), fixed {}: {} error(s), {} hole(s)",
            files.len(),
            fixed.iter().filter(|x| **x).count(),
            errors,
            holes
        );
    }
    if errors > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn fmt_cmd(args: &[String]) -> ExitCode {
    let only_check = args.iter().any(|a| a == "--check");
    let ins = match inputs(args) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let mut status = ExitCode::SUCCESS;
    let mut changed = 0;
    for i in &ins {
        let name = i.path.display().to_string();
        let Ok(src) = std::fs::read_to_string(&i.path) else {
            eprintln!("drift: cannot read {}", name);
            return ExitCode::from(2);
        };
        match drift::fmt::format_source(&src) {
            Ok(out) if out != src => {
                changed += 1;
                if only_check {
                    println!("{}", name);
                    status = ExitCode::from(1);
                } else if let Err(e) = std::fs::write(&i.path, out) {
                    eprintln!("drift: cannot write {}: {}", name, e);
                    return ExitCode::from(2);
                }
            }
            Ok(_) => {}
            Err(drift::fmt::FmtError::Syntax(diags)) => {
                let idx = LineIndex::new(&src);
                for d in &diags {
                    print!("{}", render_human(d, &name, &idx));
                }
                status = ExitCode::from(1);
            }
        }
    }
    eprintln!("{} file(s), {} {}", ins.len(), changed, if only_check { "need formatting" } else { "reformatted" });
    status
}

fn outline_cmd(args: &[String]) -> ExitCode {
    let Some(target) = args.first() else {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    };
    let src = if let Some(short) = target.strip_prefix("std.") {
        match drift::check::STD_MODULES.iter().find(|(n, _)| *n == short) {
            Some((_, s)) => s.to_string(),
            None => {
                let names: Vec<&str> = drift::check::STD_MODULES.iter().map(|(n, _)| *n).collect();
                eprintln!("drift: no std module `{}` (have: {})", short, names.join(", "));
                return ExitCode::from(2);
            }
        }
    } else if target == "std" {
        let names: Vec<String> = drift::check::STD_MODULES.iter().map(|(n, _)| format!("std.{}", n)).collect();
        println!("{}", names.join("\n"));
        return ExitCode::SUCCESS;
    } else {
        match std::fs::read_to_string(target) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("drift: cannot read {}: {}", target, e);
                return ExitCode::from(2);
            }
        }
    };
    match drift::fmt::outline(&src) {
        Ok(o) => {
            print!("{}", o);
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("drift: {} has syntax errors; run `drift check`", target);
            ExitCode::from(1)
        }
    }
}

fn parse_cmd(args: &[String]) -> ExitCode {
    let Some(file) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    };
    let src = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("drift: cannot read {}: {}", file, e);
            return ExitCode::from(2);
        }
    };
    let out = drift::parser::parse(&src);
    let idx = LineIndex::new(&src);
    for d in &out.diags {
        print!("{}", render_human(d, file, &idx));
    }
    if args.iter().any(|a| a == "--ast") {
        println!("{:#?}", out.module);
    }
    if out.diags.iter().any(|d| d.is_error()) { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("check") => check(&args[1..]),
        Some("run") => run_cmd(&args[1..]),
        Some("build") => build_cmd(&args[1..]),
        Some("test") => test_cmd(&args[1..]),
        Some("fmt") => fmt_cmd(&args[1..]),
        Some("outline") => outline_cmd(&args[1..]),
        Some("parse") => parse_cmd(&args[1..]),
        Some("mcp") => match drift::mcp::serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("drift mcp: {}", e);
                ExitCode::from(1)
            }
        },
        Some("card") => {
            print!("{}", drift::CARD);
            ExitCode::SUCCESS
        }
        Some("--version") => {
            println!("drift {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{}", USAGE);
            ExitCode::from(2)
        }
    }
}
