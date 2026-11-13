use std::io::{self, Read, Write};

#[derive(Clone, Copy)]
struct S {
    g: [u8; 81],
    r: [u16; 9],
    c: [u16; 9],
    b: [u16; 9],
    left: u32,
}

const ALL: u16 = 0x1FF;

#[inline]
fn bx(i: usize) -> usize { (i / 27) * 3 + (i % 9) / 3 }

impl S {
    #[inline]
    fn cand(&self, i: usize) -> u16 {
        !(self.r[i / 9] | self.c[i % 9] | self.b[bx(i)]) & ALL
    }
    #[inline]
    fn place(&mut self, i: usize, bit: u16) -> bool {
        let (r, c, b) = (i / 9, i % 9, bx(i));
        if (self.r[r] | self.c[c] | self.b[b]) & bit != 0 { return false; }
        self.r[r] |= bit; self.c[c] |= bit; self.b[b] |= bit;
        self.g[i] = bit.trailing_zeros() as u8 + 1;
        self.left -= 1;
        true
    }
    // returns false on contradiction
    fn propagate(&mut self, units: &[[u8; 9]; 27]) -> bool {
        loop {
            let mut changed = false;
            for i in 0..81 {
                if self.g[i] == 0 {
                    let m = self.cand(i);
                    if m == 0 { return false; }
                    if m & (m - 1) == 0 {
                        self.place(i, m);
                        changed = true;
                    }
                }
            }
            if self.left == 0 { return true; }
            for u in units.iter() {
                let mut once = 0u16; let mut twice = 0u16; let mut used = 0u16;
                for &k in u.iter() {
                    let k = k as usize;
                    if self.g[k] != 0 { used |= 1 << (self.g[k] - 1); continue; }
                    let m = self.cand(k);
                    twice |= once & m;
                    once |= m;
                }
                let need = !used & ALL;
                if need & !once != 0 { return false; }
                let mut ex = once & !twice & need;
                while ex != 0 {
                    let bit = ex & ex.wrapping_neg();
                    ex &= ex - 1;
                    for &k in u.iter() {
                        let k = k as usize;
                        if self.g[k] == 0 && self.cand(k) & bit != 0 {
                            if !self.place(k, bit) { return false; }
                            changed = true;
                            break;
                        }
                    }
                }
            }
            if self.left == 0 { return true; }
            if !changed { return true; }
        }
    }
}

fn solve(s: &mut S, units: &[[u8; 9]; 27]) -> bool {
    if !s.propagate(units) { return false; }
    if s.left == 0 { return true; }
    let mut best = 99; let mut bc = 10;
    for i in 0..81 {
        if s.g[i] == 0 {
            let n = s.cand(i).count_ones();
            if n < bc { bc = n; best = i; if n <= 2 { break; } }
        }
    }
    let mut m = s.cand(best);
    while m != 0 {
        let bit = m & m.wrapping_neg();
        m &= m - 1;
        let mut t = *s;
        t.place(best, bit);
        if solve(&mut t, units) { *s = t; return true; }
    }
    false
}

fn main() {
    let mut units = [[0u8; 9]; 27];
    for k in 0..9 {
        for j in 0..9 {
            units[k][j] = (k * 9 + j) as u8;
            units[9 + k][j] = (j * 9 + k) as u8;
            units[18 + k][j] = (((k / 3) * 3 + j / 3) * 9 + (k % 3) * 3 + j % 3) as u8;
        }
    }
    let mut inp = Vec::new();
    io::stdin().read_to_end(&mut inp).unwrap();
    let mut out = Vec::with_capacity(inp.len() + 1024);
    let mut lines: Vec<&[u8]> = inp.split(|&b| b == b'\n').collect();
    if let Some(l) = lines.last() { if l.is_empty() { lines.pop(); } }
    for mut line in lines {
        if line.last() == Some(&b'\r') { line = &line[..line.len() - 1]; }
        if line.len() != 81 || !line.iter().all(|&ch| ch == b'.' || (b'0'..=b'9').contains(&ch)) {
            out.extend_from_slice(b"invalid\n");
            continue;
        }
        let mut s = S { g: [0; 81], r: [0; 9], c: [0; 9], b: [0; 9], left: 81 };
        let mut ok = true;
        for (i, &ch) in line.iter().enumerate() {
            if ch >= b'1' && ch <= b'9' {
                if !s.place(i, 1 << (ch - b'1')) { ok = false; break; }
            }
        }
        if ok && solve(&mut s, &units) {
            for i in 0..81 { out.push(b'0' + s.g[i]); }
            out.push(b'\n');
        } else {
            out.extend_from_slice(b"no solution\n");
        }
    }
    io::stdout().write_all(&out).unwrap();
}
