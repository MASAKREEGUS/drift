//! Project-level pipeline shared by the CLI and the tests.

use crate::check::{std_sources, SourceFile};
use crate::diag::{apply_edits, Diag, Edit};

/// Diagnostics for every user file: syntax; then types/effects (if everything parses); then layout.
pub fn analyze(user: &[SourceFile]) -> Vec<Vec<Diag>> {
    let mut per_file: Vec<Vec<Diag>> = Vec::new();
    let mut asts = Vec::new();
    let mut syntax_ok = true;
    for f in user {
        let out = crate::parser::parse(&f.src);
        if out.diags.iter().any(|d| d.is_error()) {
            syntax_ok = false;
        }
        per_file.push(out.diags);
        asts.push(out.module);
    }
    if syntax_ok {
        let mut all = std_sources();
        let n_std = all.len();
        let mut std_asts: Vec<_> = all.iter().map(|f| crate::parser::parse(&f.src).module).collect();
        std_asts.extend(asts);
        all.extend(user.iter().map(|f| SourceFile { name: f.name.clone(), src: f.src.clone(), module: f.module.clone(), is_std: false }));
        for (file, d) in crate::check::check(&all, std_asts) {
            if file >= n_std {
                per_file[file - n_std].push(d);
            } else {
                eprintln!("drift: internal error in std module {}: {}", all[file].name, d.message);
            }
        }
    }
    for (i, f) in user.iter().enumerate() {
        if !per_file[i].iter().any(|d| d.is_error() && d.code.starts_with(['L', 'P'])) {
            if let Some(d) = crate::fmt::check_formatted(&f.src) {
                per_file[i].push(d);
            }
        }
        per_file[i].sort_by_key(|d| (d.span.lo, d.span.hi));
    }
    per_file
}



fn overlaps(a: &Edit, b: &Edit) -> bool {
    a.span.lo < b.span.hi.max(b.span.lo + 1) && b.span.lo < a.span.hi.max(a.span.lo + 1)
}

/// Apply every certain fix, round after round, until nothing changes. Layout (F0001) goes last.
/// Returns which files changed.
pub fn fix_files(files: &mut [SourceFile]) -> Vec<bool> {
    let mut fixed = vec![false; files.len()];
    for _ in 0..12 {
        let diags = analyze(files);
        let mut changed = false;
        for (i, ds) in diags.iter().enumerate() {
            let mut chosen: Vec<Edit> = Vec::new();
            for d in ds.iter().filter(|d| !d.fix.is_empty() && d.code != "F0001") {
                if !d.fix.iter().any(|e| chosen.iter().any(|c| overlaps(e, c))) {
                    chosen.extend(d.fix.iter().cloned());
                }
            }
            if chosen.is_empty() {
                if let Some(f) = ds.iter().find(|d| d.code == "F0001") {
                    chosen.extend(f.fix.iter().cloned());
                }
            }
            if !chosen.is_empty() {
                files[i].src = apply_edits(&files[i].src, &chosen);
                fixed[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    fixed
}

/// Convenience for tests and tools: analyze one file as module `main`.
pub fn analyze_one(src: &str) -> Vec<Diag> {
    let f = SourceFile { name: "main.dr".into(), src: src.into(), module: "main".into(), is_std: false };
    analyze(&[f]).remove(0)
}

/// Fix one file as module `main`.
pub fn fix_one(src: &str) -> String {
    let mut f = vec![SourceFile { name: "main.dr".into(), src: src.into(), module: "main".into(), is_std: false }];
    fix_files(&mut f);
    f.remove(0).src
}
