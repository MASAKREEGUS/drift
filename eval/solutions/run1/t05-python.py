import sys
import re
import json

FIELD = re.compile(r'(?:"([^"]*(?:""[^"]*)*)"|([^",\r\n]*(?:\r(?!\n)[^",\r\n]*)*))(,|\r?\n|\Z)')
NL = re.compile(r'\r?\n')


def fail(msg):
    sys.stderr.write("error: " + msg + "\n")
    sys.exit(1)


def parse(s):
    n = len(s)
    if n == 0:
        return []
    if '"' not in s:
        lines = NL.split(s)
        if lines and lines[-1] == '' and (s.endswith('\n')):
            lines.pop()
        return [ln.split(',') for ln in lines]
    records = []
    pos = 0
    match = FIELD.match
    while True:
        rec = []
        while True:
            m = match(s, pos)
            if m is None:
                if s[pos:pos + 1] == '"' and s.find('"', pos + 1) == -1:
                    fail("unterminated quoted field")
                fail("malformed field at offset %d" % pos)
            q, u, term = m.groups()
            if q is not None:
                rec.append(q.replace('""', '"') if '"' in q else q)
            else:
                rec.append(u)
            pos = m.end()
            if term != ',':
                break
        records.append(rec)
        if term == '' or pos >= n:
            break
    return records


def main():
    data = sys.stdin.buffer.read()
    s = data.decode('utf-8')
    recs = parse(s)
    out = sys.stdout
    if len(recs) <= 1:
        out.write("[]\n")
        return
    header = recs[0]
    w = len(header)
    for i in range(1, len(recs)):
        if len(recs[i]) != w:
            fail("record %d has %d fields, expected %d" % (i + 1, len(recs[i]), w))
    enc = json.encoder.encode_basestring_ascii
    tmpl = "{" + ",".join(enc(h).replace("%", "%%") + ":%s" for h in header) + "}"
    parts = [tmpl % tuple(map(enc, recs[i])) for i in range(1, len(recs))]
    out.write("[" + ",\n".join(parts) + "]\n")

main()
