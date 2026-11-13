use std::io::{self, Read, Write};

#[derive(Clone, Copy)]
enum Tok { Num(i64), Plus, Minus, Star, Slash, Pct, LP, RP }

#[derive(Clone, Copy)]
enum Op { Push(i64), Neg, Add, Sub, Mul, Div(usize), Rem(usize) }

struct P<'a> { toks: &'a [(Tok, usize)], pos: usize, end_col: usize, out: Vec<Op> }

type R = Result<(), (usize, &'static str)>;

impl<'a> P<'a> {
    fn err_here(&self) -> (usize, &'static str) {
        if self.pos < self.toks.len() { (self.toks[self.pos].1, "unexpected token") } else { (self.end_col, "unexpected end") }
    }
    fn expr(&mut self) -> R {
        self.term()?;
        while self.pos < self.toks.len() {
            match self.toks[self.pos].0 {
                Tok::Plus => { self.pos += 1; self.term()?; self.out.push(Op::Add); }
                Tok::Minus => { self.pos += 1; self.term()?; self.out.push(Op::Sub); }
                _ => break,
            }
        }
        Ok(())
    }
    fn term(&mut self) -> R {
        self.unary()?;
        while self.pos < self.toks.len() {
            let (t, c) = self.toks[self.pos];
            match t {
                Tok::Star => { self.pos += 1; self.unary()?; self.out.push(Op::Mul); }
                Tok::Slash => { self.pos += 1; self.unary()?; self.out.push(Op::Div(c)); }
                Tok::Pct => { self.pos += 1; self.unary()?; self.out.push(Op::Rem(c)); }
                _ => break,
            }
        }
        Ok(())
    }
    fn unary(&mut self) -> R {
        let mut negs = 0usize;
        while self.pos < self.toks.len() {
            if let Tok::Minus = self.toks[self.pos].0 { negs += 1; self.pos += 1; } else { break; }
        }
        self.primary()?;
        for _ in 0..negs { self.out.push(Op::Neg); }
        Ok(())
    }
    fn primary(&mut self) -> R {
        if self.pos >= self.toks.len() { return Err(self.err_here()); }
        match self.toks[self.pos].0 {
            Tok::Num(v) => { self.pos += 1; self.out.push(Op::Push(v)); Ok(()) }
            Tok::LP => {
                self.pos += 1;
                self.expr()?;
                if self.pos < self.toks.len() {
                    if let Tok::RP = self.toks[self.pos].0 { self.pos += 1; return Ok(()); }
                }
                Err(self.err_here())
            }
            _ => Err(self.err_here()),
        }
    }
}

fn process(line: &str, out: &mut String) {
    let chars: Vec<char> = line.chars().collect();
    if chars.iter().all(|&c| c == ' ' || c == '\t') { return; }
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '0'..='9' | ' ' | '\t' | '+' | '-' | '*' | '/' | '%' | '(' | ')' => {}
            _ => { out.push_str(&format!("error at {}: unexpected character\n", i + 1)); return; }
        }
    }
    let mut toks: Vec<(Tok, usize)> = Vec::new();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        let col = i + 1;
        let t = match c {
            ' ' | '\t' => { i += 1; continue; }
            '0'..='9' => {
                let mut v: i64 = 0;
                while i < n && chars[i].is_ascii_digit() {
                    v = v.wrapping_mul(10).wrapping_add((chars[i] as u8 - b'0') as i64);
                    i += 1;
                }
                toks.push((Tok::Num(v), col));
                continue;
            }
            '+' => Tok::Plus, '-' => Tok::Minus, '*' => Tok::Star, '/' => Tok::Slash,
            '%' => Tok::Pct, '(' => Tok::LP, _ => Tok::RP,
        };
        toks.push((t, col));
        i += 1;
    }
    let mut p = P { toks: &toks, pos: 0, end_col: n + 1, out: Vec::with_capacity(toks.len()) };
    let r = p.expr().and_then(|_| if p.pos < toks.len() { Err(p.err_here()) } else { Ok(()) });
    if let Err((col, msg)) = r {
        out.push_str(&format!("error at {}: {}\n", col, msg));
        return;
    }
    let mut st: Vec<i64> = Vec::with_capacity(64);
    for op in p.out.iter() {
        match *op {
            Op::Push(v) => st.push(v),
            Op::Neg => { let a = st.pop().unwrap(); st.push(a.wrapping_neg()); }
            _ => {
                let b = st.pop().unwrap();
                let a = st.pop().unwrap();
                let r = match *op {
                    Op::Add => a.wrapping_add(b),
                    Op::Sub => a.wrapping_sub(b),
                    Op::Mul => a.wrapping_mul(b),
                    Op::Div(c) => { if b == 0 { out.push_str(&format!("error at {}: division by zero\n", c)); return; } a.wrapping_div(b) }
                    Op::Rem(c) => { if b == 0 { out.push_str(&format!("error at {}: division by zero\n", c)); return; } a.wrapping_rem(b) }
                    _ => unreachable!(),
                };
                st.push(r);
            }
        }
    }
    out.push_str(&st[0].to_string());
    out.push('\n');
}

fn run() {
    let mut buf = Vec::new();
    io::stdin().read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf);
    let mut out = String::with_capacity(buf.len() / 2 + 16);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if let Some(l) = lines.last() { if l.is_empty() { lines.pop(); } }
    for line in lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        process(line, &mut out);
    }
    io::stdout().write_all(out.as_bytes()).unwrap();
}

fn main() {
    let h = std::thread::Builder::new().stack_size(1 << 30).spawn(run).unwrap();
    h.join().unwrap();
}
