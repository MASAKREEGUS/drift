use std::io::{self, Read, Write};
use std::process::exit;

fn fail(msg: &str) -> ! {
    eprintln!("error: {}", msg);
    exit(1);
}

fn parse_token(t: &[u8]) -> Option<i64> {
    let (neg, digits) = if t[0] == b'-' { (true, &t[1..]) } else { (false, t) };
    if digits.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for &c in digits {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as i64)?;
    }
    Some(if neg { -v } else { v })
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn main() {
    let mut buf = Vec::new();
    if io::stdin().lock().read_to_end(&mut buf).is_err() {
        fail("cannot read standard input");
    }
    let mut vals: Vec<i64> = Vec::new();
    for tok in buf.split(|&c| is_ws(c)).filter(|t| !t.is_empty()) {
        match parse_token(tok) {
            Some(v) => vals.push(v),
            None => fail(&format!("invalid token: {}", String::from_utf8_lossy(tok))),
        }
    }
    if vals.is_empty() {
        fail("no integers in input");
    }
    let n = vals.len() as i128;
    let sum: i128 = vals.iter().map(|&v| v as i128).sum();
    vals.sort_unstable();
    let min = vals[0];
    let max = vals[vals.len() - 1];

    // mean rounded to 2 decimals (half away from zero; never exactly halfway)
    let q = sum * 100;
    let neg = q < 0;
    let a = q.abs();
    let r = (2 * a + n) / (2 * n);
    let sign = if neg && r > 0 { "-" } else { "" };
    let mean = format!("{}{}.{:02}", sign, r / 100, r % 100);

    let len = vals.len();
    let median = if len % 2 == 1 {
        format!("{}", vals[len / 2])
    } else {
        let s = vals[len / 2 - 1] + vals[len / 2];
        if s % 2 == 0 {
            format!("{}", s / 2)
        } else {
            let sign = if s < 0 { "-" } else { "" };
            format!("{}{}.5", sign, s.abs() / 2)
        }
    };

    let out = io::stdout();
    let mut w = out.lock();
    let _ = write!(w, "min {}\nmax {}\nmean {}\nmedian {}\n", min, max, mean, median);
}
