import sys
from heapq import heapify, heappush, heappop


def main():
    data = sys.stdin.buffer.read().decode('ascii', 'replace')
    succ = {}
    pred = {}
    indeg = {}
    for line in data.splitlines():
        i = line.find('->')
        if i < 0:
            a = line.strip()
            if a and a not in succ:
                succ[a] = []
                pred[a] = []
                indeg[a] = 0
            continue
        a = line[:i].strip()
        b = line[i + 2:].strip()
        if a not in succ:
            succ[a] = []
            pred[a] = []
            indeg[a] = 0
        if b not in succ:
            succ[b] = []
            pred[b] = []
            indeg[b] = 0
        succ[a].append(b)
        pred[b].append(a)
        indeg[b] += 1

    heap = [n for n, d in indeg.items() if d == 0]
    heapify(heap)
    out = []
    done = set()
    while heap:
        n = heappop(heap)
        out.append(n)
        done.add(n)
        for m in succ[n]:
            d = indeg[m] - 1
            indeg[m] = d
            if d == 0:
                heappush(heap, m)

    if len(out) == len(indeg):
        sys.stdout.write('\n'.join(out) + ('\n' if out else ''))
        return 0

    # Every remaining node has a remaining predecessor; walk backwards.
    start = next(n for n in indeg if n not in done)
    pos = {}
    path = []
    cur = start
    while cur not in pos:
        pos[cur] = len(path)
        path.append(cur)
        for p in pred[cur]:
            if p not in done:
                cur = p
                break
    cyc = path[pos[cur]:]
    cyc.reverse()  # path follows predecessor links; reverse to edge direction
    cyc.append(cyc[0])
    sys.stdout.write('cycle: ' + ' -> '.join(cyc) + '\n')
    return 1


if __name__ == '__main__':
    sys.exit(main())
