//! C compiler driver with a content-addressed cache (runtime object and program binaries).

use crate::codegen::{RT_C, RT_H};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for p in parts {
        for b in *p {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("DRIFT_CACHE") {
        if !d.is_empty() {
            return d.into();
        }
    }
    if let Ok(d) = std::env::var("XDG_CACHE_HOME") {
        if !d.is_empty() {
            return Path::new(&d).join("drift");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".cache").join("drift")
}

fn cc() -> String {
    std::env::var("DRIFT_CC").unwrap_or_else(|_| "gcc".into())
}

/// Extra flags for every C compilation, e.g. `DRIFT_CFLAGS="-fsanitize=address,undefined -g"`.
fn extra_flags() -> Vec<String> {
    std::env::var("DRIFT_CFLAGS").unwrap_or_default().split_whitespace().map(String::from).collect()
}

fn run_cc(args: &[&str], what: &str) -> Result<(), String> {
    let out = Command::new(cc())
        .args(extra_flags())
        .args(args)
        .output()
        .map_err(|e| format!("cannot run the C compiler `{}` ({}); install gcc or set DRIFT_CC", cc(), e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{} failed:\n{}", what, String::from_utf8_lossy(&out.stderr)))
    }
}

fn io(e: std::io::Error, p: &Path) -> String {
    format!("{}: {}", p.display(), e)
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| io(e, &tmp))?;
    std::fs::rename(&tmp, path).map_err(|e| io(e, path))
}

/// Include directory and object file of the runtime, compiled once per runtime version.
pub fn runtime() -> Result<(PathBuf, PathBuf), String> {
    let flags = extra_flags().join(" ");
    let h = fnv(&[RT_H.as_bytes(), RT_C.as_bytes(), flags.as_bytes()]);
    let dir = cache_dir().join(format!("rt-{:016x}", h));
    let obj = dir.join("drift_rt.o");
    if obj.exists() {
        return Ok((dir, obj));
    }
    std::fs::create_dir_all(&dir).map_err(|e| io(e, &dir))?;
    write_atomic(&dir.join("drift_rt.h"), RT_H.as_bytes())?;
    let c = dir.join("drift_rt.c");
    write_atomic(&c, RT_C.as_bytes())?;
    let tmp = dir.join(format!("drift_rt.{}.o", std::process::id()));
    run_cc(&["-O2", "-std=gnu11", "-w", "-c", &c.to_string_lossy(), "-o", &tmp.to_string_lossy()], "compiling the Drift runtime")?;
    std::fs::rename(&tmp, &obj).map_err(|e| io(e, &obj))?;
    Ok((dir, obj))
}

/// Compile generated C into an executable (cached by content). `opt` is a gcc flag such as `-O2`.
pub fn link(c_src: &str, opt: &str) -> Result<PathBuf, String> {
    let (inc, obj) = runtime()?;
    let flags = extra_flags().join(" ");
    let h = fnv(&[c_src.as_bytes(), opt.as_bytes(), obj.to_string_lossy().as_bytes(), flags.as_bytes()]);
    let dir = cache_dir().join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| io(e, &dir))?;
    let exe = dir.join(format!("{:016x}", h));
    if exe.exists() {
        return Ok(exe);
    }
    let c = dir.join(format!("{:016x}.c", h));
    write_atomic(&c, c_src.as_bytes())?;
    let tmp = dir.join(format!("{:016x}.{}.tmp", h, std::process::id()));
    run_cc(
        &[opt, "-std=gnu11", "-w", "-I", &inc.to_string_lossy(), &c.to_string_lossy(), &obj.to_string_lossy(), "-lm", "-o", &tmp.to_string_lossy()],
        "compiling the generated C (this is a bug in drift, please report it)",
    )?;
    std::fs::rename(&tmp, &exe).map_err(|e| io(e, &exe))?;
    Ok(exe)
}
