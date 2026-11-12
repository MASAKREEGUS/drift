"""Regenerates the Unicode section at the end of src/runtime/drift_rt.c from Python's unicodedata.

    python3 tools/gen_unicode.py

Drift's text functions are defined to agree with Python's str methods of the generating interpreter:
to_lower/to_upper = str.lower/str.upper, nfkd = unicodedata.normalize("NFKD"), strip_marks drops
code points whose canonical combining class is not 0, is_alphabetic/is_alphanumeric/is_whitespace =
str.isalpha/isalnum/isspace (per code point), regex \\w = isalnum or "_".
"""
import pathlib
import sys
import unicodedata

BEGIN = "/* BEGIN GENERATED UNICODE"
END = "/* END GENERATED UNICODE */"
FUNCS = "/* ------------------------------------------------------------------ unicode"
MAX = 0x110000


def cps():
    for c in range(MAX):
        if 0xD800 <= c <= 0xDFFF:
            continue
        yield c


def ranges(pred):
    out, start, prev = [], None, None
    for c in cps():
        if pred(chr(c)):
            if start is None:
                start = c
            elif c != prev + 1:
                out.append((start, prev))
                start = c
            prev = c
    if start is not None:
        out.append((start, prev))
    return out


def mapping(f):
    keys, pool = [], []
    for c in cps():
        if 0xAC00 <= c <= 0xD7A3 and f is nfkd:
            continue  # Hangul syllables are decomposed algorithmically
        s = f(chr(c))
        if s != chr(c):
            keys.append((c, len(pool), len(s)))
            pool.extend(ord(x) for x in s)
    return keys, pool


def nfkd(ch):
    return unicodedata.normalize("NFKD", ch)


def emit_ranges(name, rs):
    body = ",".join(f"{{{a:#x},{b:#x}}}" for a, b in rs)
    return f"static const uint32_t {name}[][2] = {{{body}}};\n#define {name}_n {len(rs)}\n"


def main():
    rt = pathlib.Path(__file__).resolve().parent.parent / "src/runtime/drift_rt.c"
    parts = [f"{BEGIN}: tools/gen_unicode.py, Python {sys.version.split()[0]}, Unicode {unicodedata.unidata_version} */\n"]
    parts.append(emit_ranges("uc_alpha", ranges(str.isalpha)))
    parts.append(emit_ranges("uc_alnum", ranges(str.isalnum)))
    parts.append(emit_ranges("uc_space", ranges(str.isspace)))
    ccc = []
    for c in cps():
        k = unicodedata.combining(chr(c))
        if k:
            if ccc and ccc[-1][1] == c - 1 and ccc[-1][2] == k:
                ccc[-1][1] = c
            else:
                ccc.append([c, c, k])
    parts.append("static const uint32_t uc_ccc[][3] = {" + ",".join(f"{{{a:#x},{b:#x},{k}}}" for a, b, k in ccc) + f"}};\n#define uc_ccc_n {len(ccc)}\n")
    for name, f in (("uc_lower", str.lower), ("uc_upper", str.upper), ("uc_nfkd", nfkd)):
        keys, pool = mapping(f)
        parts.append(f"static const uint32_t {name}_k[][3] = {{" + ",".join(f"{{{c:#x},{o},{n}}}" for c, o, n in keys) + f"}};\n#define {name}_n {len(keys)}\n")
        parts.append(f"static const uint32_t {name}_v[] = {{" + ",".join(f"{x:#x}" for x in pool) + "};\n")
    parts.append(END + "\n")
    text = rt.read_text()
    if BEGIN in text:
        text = text[: text.index(BEGIN)] + text[text.index(END) + len(END) + 1 :]
    mark = text.index(FUNCS)  # the functions that use the tables follow them
    rt.write_text(text[:mark] + "".join(parts) + "\n" + text[mark:])
    print("wrote", rt, "unicode", unicodedata.unidata_version)


if __name__ == "__main__":
    main()
