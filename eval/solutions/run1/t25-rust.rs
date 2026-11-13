use std::io::{self, Read, Write};

fn main() {
    let mut s = String::new();
    io::stdin().read_to_string(&mut s).unwrap();
    let n: u64 = s.trim().parse().unwrap_or(0);
    let out = count(n);
    let so = io::stdout();
    let mut so = so.lock();
    writeln!(so, "{}", out).unwrap();
}

fn count(n: u64) -> u64 {
    if n <= 2 {
        return 0;
    }
    let m = n - 1; // largest candidate
    let hi = ((m - 1) / 2) as usize; // odd index i represents 2i+1, i in 1..=hi
    // small primes up to sqrt(m)
    let mut r = (m as f64).sqrt() as usize + 2;
    while (r as u64) * (r as u64) > m && r > 0 {
        r -= 1;
    }
    let mut small = vec![false; r + 1];
    let mut sp: Vec<usize> = Vec::new();
    let mut p = 3;
    while p <= r {
        if !small[p] {
            sp.push(p);
            let mut q = p * p;
            while q <= r {
                small[q] = true;
                q += 2 * p;
            }
        }
        p += 2;
    }
    let mut next: Vec<usize> = sp.iter().map(|&p| (p * p - 1) / 2).collect();
    const S: usize = 32768;
    let mut buf = vec![0u8; S];
    let mut total: u64 = 1; // prime 2
    let mut low = 1usize;
    while low <= hi {
        let high = (low + S).min(hi + 1);
        let len = high - low;
        let b = &mut buf[..len];
        for x in b.iter_mut() {
            *x = 0;
        }
        for (k, &p) in sp.iter().enumerate() {
            let mut j = next[k];
            if j >= high {
                continue;
            }
            j -= low;
            while j < len {
                b[j] = 1;
                j += p;
            }
            next[k] = j + low;
        }
        let marked: usize = b.iter().map(|&x| x as usize).sum();
        total += (len - marked) as u64;
        low = high;
    }
    total
}
