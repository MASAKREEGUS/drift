import sys
import re
import codecs
from collections import Counter

# [^\W_] matches exactly the Unicode categories Lu Ll Lt Lm Lo Nd Nl No
# (verified against unicodedata for the whole code space on this CPython).
WORD = re.compile(r"[^\W_]+")
# ASCII fast path: letters/digits kept, everything else becomes a space.
ASCII_TAB = bytes(
    c if (48 <= c <= 57 or 65 <= c <= 90 or 97 <= c <= 122) else 32
    for c in range(256)
)


def usage(msg):
    sys.stderr.write("error: " + msg + "\nusage: main.py N\n")
    sys.exit(2)


def main():
    if len(sys.argv) < 2:
        usage("missing N")
    arg = sys.argv[1]
    if not (arg.isascii() and arg.isdigit()):
        usage("N must be a positive integer")
    n = int(arg)
    if n <= 0:
        usage("N must be a positive integer")

    raw = Counter()    # str keys
    raw_b = Counter()  # bytes keys (ASCII chunks)
    dec = codecs.getincrementaldecoder("utf-8")("replace")
    inp = sys.stdin.buffer
    pending = []  # pieces of a word that may continue into the next chunk
    findall = WORD.findall
    wmatch = WORD.match
    while True:
        b = inp.read(1 << 20)
        final = not b
        text = dec.decode(b, final)
        if pending and text:
            m = wmatch(text)
            if m:
                e = m.end()
                pending.append(text[:e])
                text = text[e:]
                if not text and not final:
                    continue
            raw["".join(pending)] += 1
            pending = []
        if text:
            trailing = not final and wmatch(text, len(text) - 1)
            if text.isascii():
                words = text.encode("ascii").translate(ASCII_TAB).split()
                if words and trailing:
                    pending.append(words.pop().decode("ascii"))
                raw_b.update(words)
            else:
                words = findall(text)
                if words and trailing:
                    pending.append(words.pop())
                raw.update(words)
        if final:
            if pending:
                raw["".join(pending)] += 1
            break

    counts = {}
    for w, c in raw_b.items():
        lw = w.decode("ascii").lower()
        counts[lw] = counts.get(lw, 0) + c
    for w, c in raw.items():
        lw = w.lower()
        counts[lw] = counts.get(lw, 0) + c

    items = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))
    if n < len(items):
        items = items[:n]
    out = "".join("%d %s\n" % (c, w) for w, c in items)
    sys.stdout.buffer.write(out.encode("utf-8", "surrogateescape"))
    sys.stdout.flush()


main()
