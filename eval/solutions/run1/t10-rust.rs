use std::collections::BinaryHeap;
use std::cmp::Reverse;
use std::io::{self, Read, Write};

fn main() {
    let mut buf = Vec::new();
    io::stdin().lock().read_to_end(&mut buf).unwrap();
    let mut pos = 0usize;
    let b = &buf[..];
    let mut next = || -> u64 {
        while pos < b.len() && !(b[pos] >= b'0' && b[pos] <= b'9') { pos += 1; }
        let mut x: u64 = 0;
        while pos < b.len() && b[pos] >= b'0' && b[pos] <= b'9' {
            x = x * 10 + (b[pos] - b'0') as u64;
            pos += 1;
        }
        x
    };
    let n = next() as usize;
    let m = next() as usize;
    let mut eu = Vec::with_capacity(m);
    let mut ev = Vec::with_capacity(m);
    let mut ew = Vec::with_capacity(m);
    let mut start = vec![0u32; n + 1];
    for _ in 0..m {
        let u = next() as u32;
        let v = next() as u32;
        let w = next();
        eu.push(u); ev.push(v); ew.push(w);
        start[u as usize + 1] += 1;
    }
    let s = next() as usize;
    let t = next() as usize;
    for i in 0..n { start[i + 1] += start[i]; }
    let mut fill = start.clone();
    let mut adj: Vec<(u32, u64)> = vec![(0, 0); m];
    for i in 0..m {
        let u = eu[i] as usize;
        adj[fill[u] as usize] = (ev[i], ew[i]);
        fill[u] += 1;
    }
    drop(eu); drop(ev); drop(ew); drop(fill);

    const INF: u64 = u64::MAX;
    let mut dist = vec![INF; n];
    let mut par = vec![u32::MAX; n];
    let mut done = vec![false; n];
    let mut heap = BinaryHeap::new();
    dist[s] = 0;
    heap.push(Reverse((0u64, s as u32)));
    while let Some(Reverse((d, u))) = heap.pop() {
        let u = u as usize;
        if done[u] { continue; }
        done[u] = true;
        if u == t { break; }
        for &(v, w) in &adj[start[u] as usize..start[u + 1] as usize] {
            let nd = d + w;
            let vi = v as usize;
            if nd < dist[vi] {
                dist[vi] = nd;
                par[vi] = u as u32;
                heap.push(Reverse((nd, v)));
            }
        }
    }
    let out = io::stdout();
    let mut o = io::BufWriter::new(out.lock());
    if dist[t] == INF {
        writeln!(o, "no path").unwrap();
        return;
    }
    let mut path = vec![t as u32];
    let mut c = t;
    while c != s { c = par[c] as usize; path.push(c as u32); }
    path.reverse();
    let mut sline = String::with_capacity(path.len() * 7 + 8);
    sline.push_str("path");
    for p in &path { sline.push(' '); sline.push_str(&p.to_string()); }
    writeln!(o, "cost {}", dist[t]).unwrap();
    writeln!(o, "{}", sline).unwrap();
}
