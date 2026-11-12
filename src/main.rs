use drift::check::SourceFile;
use drift::driver::{analyze, fix_files};
use drift::diag::{render_human, render_json, LineIndex};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "drift — compiler for the Drift language

usage:
  drift check [--json] [--fix] <file|dir>...   all diagnostics (syntax, types, effects, layout); --fix applies every certain fix
  drift fmt [--check] <file|dir>...            rewrite files in canonical form; --check only lists them
  drift outline <std.module|file>              signatures and types of a module
  drift parse <file> [--ast]                   parse one file; --ast prints the tree
  drift card                                   print the language card
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
        if parts.len() > 1 && parts[0] == "src" {
            parts.remove(0);
        }
        out.push(Input { path: path.to_path_buf(), module: parts.join(".") });
    }
    Ok(())
}

fn inputs(args: &[String]) -> Result<Vec<Input>, ExitCode> {
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

fn check(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let fix = args.iter().any(|a| a == "--fix");
    let ins = match inputs(args) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let mut files = Vec::new();
    for i in &ins {
        match std::fs::read_to_string(&i.path) {
            Ok(src) => files.push(SourceFile { name: i.path.display().to_string(), src, module: i.module.clone(), is_std: false }),
            Err(e) => {
                eprintln!("drift: cannot read {}: {}", i.path.display(), e);
                return ExitCode::from(2);
            }
        }
    }
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
    let mut errors = 0;
    let mut holes = 0;
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
        Some("fmt") => fmt_cmd(&args[1..]),
        Some("outline") => outline_cmd(&args[1..]),
        Some("parse") => parse_cmd(&args[1..]),
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
