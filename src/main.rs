use drift::diag::{render_human, render_json, LineIndex};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "drift — compiler for the Drift language

usage:
  drift check [--json] <file|dir>...   report all diagnostics (phase 1: syntax only)
  drift parse <file> [--ast]           parse one file; --ast prints the tree
  drift card                           print the language card
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

fn check(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let mut files = Vec::new();
    for a in args.iter().filter(|a| !a.starts_with("--")) {
        if let Err(e) = collect(Path::new(a), &mut files) {
            eprintln!("drift: cannot read {}: {}", a, e);
            return ExitCode::from(2);
        }
    }
    if files.is_empty() {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    }
    let mut errors = 0;
    for f in &files {
        let name = f.display().to_string();
        let src = match std::fs::read_to_string(f) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("drift: cannot read {}: {}", name, e);
                return ExitCode::from(2);
            }
        };
        let out = drift::parser::parse(&src);
        let idx = LineIndex::new(&src);
        for d in &out.diags {
            if d.is_error() {
                errors += 1;
            }
            if json {
                println!("{}", render_json(d, &name, &idx));
            } else {
                print!("{}", render_human(d, &name, &idx));
            }
        }
    }
    if !json {
        eprintln!("checked {} file(s): {} error(s)", files.len(), errors);
    }
    if errors > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
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
