use std::io::{self, Read, Write};
use std::process;

fn fail(msg: &str) -> ! {
    eprintln!("error: {}", msg);
    process::exit(1);
}

// Field: (start, end, quoted). For quoted, raw content between quotes (with "" pairs).
fn write_json_str(out: &mut Vec<u8>, data: &[u8], quoted: bool) {
    out.push(b'"');
    let mut run = 0;
    let mut i = 0;
    let n = data.len();
    while i < n {
        let c = data[i];
        let esc: &[u8];
        let mut skip = 1;
        let mut tmp = [0u8; 6];
        match c {
            b'"' => {
                esc = b"\\\"";
                if quoted { skip = 2; }
            }
            b'\\' => esc = b"\\\\",
            b'\n' => esc = b"\\n",
            b'\r' => esc = b"\\r",
            b'\t' => esc = b"\\t",
            0..=0x1f => {
                const HEX: &[u8] = b"0123456789abcdef";
                tmp = [b'\\', b'u', b'0', b'0', HEX[(c >> 4) as usize], HEX[(c & 15) as usize]];
                esc = &tmp;
            }
            _ => { i += 1; continue; }
        }
        out.extend_from_slice(&data[run..i]);
        out.extend_from_slice(esc);
        let _ = tmp;
        i += skip;
        run = i;
    }
    out.extend_from_slice(&data[run..n]);
    out.push(b'"');
}

// Parses one record starting at pos; fills fields; returns new pos.
fn parse_record(inp: &[u8], mut pos: usize, fields: &mut Vec<(usize, usize, bool)>) -> usize {
    fields.clear();
    let n = inp.len();
    loop {
        if pos < n && inp[pos] == b'"' {
            let start = pos + 1;
            let mut i = start;
            loop {
                match inp[i..].iter().position(|&b| b == b'"') {
                    None => fail("unterminated quoted field"),
                    Some(off) => {
                        let q = i + off;
                        if q + 1 < n && inp[q + 1] == b'"' {
                            i = q + 2;
                        } else {
                            fields.push((start, q, true));
                            pos = q + 1;
                            break;
                        }
                    }
                }
            }
            if pos >= n {
                return pos;
            }
            match inp[pos] {
                b',' => { pos += 1; continue; }
                b'\n' => return pos + 1,
                b'\r' if pos + 1 < n && inp[pos + 1] == b'\n' => return pos + 2,
                _ => fail("unexpected character after closing quote"),
            }
        } else {
            let start = pos;
            let mut i = pos;
            loop {
                if i >= n {
                    fields.push((start, i, false));
                    return i;
                }
                match inp[i] {
                    b',' => { fields.push((start, i, false)); pos = i + 1; break; }
                    b'\n' => { fields.push((start, i, false)); return i + 1; }
                    b'\r' if i + 1 < n && inp[i + 1] == b'\n' => {
                        fields.push((start, i, false));
                        return i + 2;
                    }
                    b'"' => fail("quote inside unquoted field"),
                    _ => i += 1,
                }
            }
        }
    }
}

fn main() {
    let mut inp = Vec::new();
    if io::stdin().lock().read_to_end(&mut inp).is_err() {
        fail("cannot read input");
    }
    let n = inp.len();
    let stdout = io::stdout();
    let mut so = stdout.lock();
    if n == 0 {
        let _ = so.write_all(b"[]\n");
        return;
    }
    let mut fields = Vec::new();
    let mut pos = parse_record(&inp, 0, &mut fields);
    let headers: Vec<Vec<u8>> = fields
        .iter()
        .map(|&(s, e, q)| {
            let mut v = Vec::new();
            write_json_str(&mut v, &inp[s..e], q);
            v.push(b':');
            v
        })
        .collect();
    let nh = headers.len();
    let mut out: Vec<u8> = Vec::with_capacity(1 << 20);
    out.push(b'[');
    let mut first = true;
    let mut rec = 1usize;
    while pos < n {
        pos = parse_record(&inp, pos, &mut fields);
        rec += 1;
        if fields.len() != nh {
            fail(&format!("record {} has {} fields, expected {}", rec, fields.len(), nh));
        }
        if !first { out.push(b','); }
        first = false;
        out.push(b'{');
        for (k, &(s, e, q)) in fields.iter().enumerate() {
            if k > 0 { out.push(b','); }
            out.extend_from_slice(&headers[k]);
            write_json_str(&mut out, &inp[s..e], q);
        }
        out.push(b'}');
        if out.len() >= 1 << 20 {
            if so.write_all(&out).is_err() { process::exit(1); }
            out.clear();
        }
    }
    out.extend_from_slice(b"]\n");
    if so.write_all(&out).is_err() || so.flush().is_err() { process::exit(1); }
}
