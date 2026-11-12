"""Checks every code point against Python: to_lower, to_upper, nfkd, strip_marks, is_alphabetic,
is_alphanumeric, is_whitespace, and to_lower of the code point before and after a capital sigma (Final_Sigma). Run from the repository root after `cargo build --release`:

    python3 tools/check_unicode.py
"""
import pathlib, subprocess, sys, tempfile, unicodedata

SKIP = {0x0A, 0x0B, 0x0C, 0x0D, 0x1C, 0x1D, 0x1E, 0x85, 0x2028, 0x2029, 0x09}  # line/field separators
PROGRAM = """pub fn main() -> Result[(), Error] uses io, fs, env {
  let text = fs.read_str(env.args().get(1).unwrap_or(""))?
  for line in text.lines() {
    io.println("{line.to_lower()}\\t{line.to_upper()}\\t{line.nfkd()}\\t{line.strip_marks()}\\t{line.is_alphabetic()}\\t{line.is_alphanumeric()}\\t{line.is_whitespace()}\\t{"A{line}\\u{3a3}".to_lower()}\\t{"A\\u{3a3}{line}".to_lower()}")
  }
  Ok(())
}
"""


def main():
    root = pathlib.Path(__file__).resolve().parent.parent
    cps = [c for c in range(0x110000) if not 0xD800 <= c <= 0xDFFF and c not in SKIP]
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)
        (d / "u.dr").write_text(PROGRAM)
        (d / "cps.txt").write_text("".join(chr(c) + "\n" for c in cps), encoding="utf-8")
        drift = root / "target/release/drift"
        subprocess.run([drift, "build", d / "u.dr", "-o", d / "u"], check=True, stdout=subprocess.DEVNULL)
        out = subprocess.run([d / "u", d / "cps.txt"], check=True, capture_output=True, env={"DRIFT_SANDBOX": "off"}).stdout.decode()
    bad = 0
    for c, line in zip(cps, out.split("\n")):
        s = chr(c)
        want = [s.lower(), s.upper(), unicodedata.normalize("NFKD", s), "".join(x for x in s if not unicodedata.combining(x)),
                str(s.isalpha()).lower(), str(s.isalnum()).lower(), str(s.isspace()).lower(),
                ("A" + s + "\u03a3").lower(), ("A\u03a3" + s).lower()]
        if line.split("\t") != want:
            bad += 1
            if bad <= 10:
                print(hex(c), line.split("\t"), want)
    print(f"{len(cps)} code points, {bad} mismatches (Unicode {unicodedata.unidata_version})")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
