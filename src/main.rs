use drift::diag::{apply_edits, render_human, render_json, Diag, Edit, LineIndex};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "drift — compiler for the Drift language

usage:
  drift check [--json] [--fix] <file|dir>...   report all diagnostics; --fix applies every fix first
  drift fmt [--check] <file|dir>...            rewrite files in canonical form; --check only reports
  drift parse <file> [--ast]                   parse one file; --ast prints the tree
  drift card                                   print the language card
  drift --version";

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if p.is_dir() || p.extension().and_then(|e| e.to_str()) == Some("dr") {
                collect(&p, out)?;
            }
        }
    } else {
        out.push(path.to_path_buf());
    }
    Ok(())
}

fn files_from(args: &[String]) -> Result<Vec<PathBuf>, ExitCode> {
    let mut files = Vec::new();
    for a in args.iter().filter(|a| !a.starts_with("--")) {
        if let Err(e) = collect(Path::new(a), &mut files) {
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

/// All diagnostics for one source text: syntax, then (when it parses) canonical form.
fn diagnose(src: &str) -> Vec<Diag> {
    let mut diags = drift::parser::parse(src).diags;
    if !diags.iter().any(|d| d.is_error()) {
        if let Some(d) = drift::fmt::check_formatted(src) {
            diags.push(d);
        }
    }
    diags
}

/// Apply non-overlapping fixes repeatedly until nothing changes (max 8 rounds).
fn fix_all(mut src: String) -> String {
    for _ in 0..8 {
        let diags = diagnose(&src);
        let mut chosen: Vec<Edit> = Vec::new();
        for d in diags.iter().filter(|d| !d.fix.is_empty()) {
            let overlaps = d.fix.iter().any(|e| chosen.iter().any(|c| e.span.lo < c.span.hi.max(c.span.lo + 1) && c.span.lo < e.span.hi.max(e.span.lo + 1)));
            if !overlaps {
                chosen.extend(d.fix.iter().cloned());
            }
        }
        if chosen.is_empty() {
            break;
        }
        src = apply_edits(&src, &chosen);
    }
    src
}

fn check(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let fix = args.iter().any(|a| a == "--fix");
    let files = match files_from(args) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let mut errors = 0;
    let mut fixed_files = 0;
    for f in &files {
        let name = f.display().to_string();
        let mut src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("drift: cannot read {}: {}", name, e);
                return ExitCode::from(2);
            }
        };
        if fix {
            let new = fix_all(src.clone());
            if new != src {
                if let Err(e) = std::fs::write(f, &new) {
                    eprintln!("drift: cannot write {}: {}", name, e);
                    return ExitCode::from(2);
                }
                fixed_files += 1;
                src = new;
            }
        }
        let idx = LineIndex::new(&src);
        for d in diagnose(&src) {
            if d.is_error() {
                errors += 1;
            }
            if json {
                println!("{}", render_json(&d, &name, &idx));
            } else {
                print!("{}", render_human(&d, &name, &idx));
            }
        }
    }
    if !json {
        eprintln!("checked {} file(s), fixed {}: {} error(s)", files.len(), fixed_files, errors);
    }
    if errors > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn fmt_cmd(args: &[String]) -> ExitCode {
    let only_check = args.iter().any(|a| a == "--check");
    let files = match files_from(args) {
        Ok(f) => f,
        Err(c) => return c,
    };
    let mut status = ExitCode::SUCCESS;
    let mut changed = 0;
    for f in &files {
        let name = f.display().to_string();
        let Ok(src) = std::fs::read_to_string(f) else {
            eprintln!("drift: cannot read {}", name);
            return ExitCode::from(2);
        };
        match drift::fmt::format_source(&src) {
            Ok(out) if out != src => {
                changed += 1;
                if only_check {
                    println!("{}", name);
                    status = ExitCode::from(1);
                } else if let Err(e) = std::fs::write(f, out) {
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
    eprintln!("{} file(s), {} {}", files.len(), changed, if only_check { "need formatting" } else { "reformatted" });
    status
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
