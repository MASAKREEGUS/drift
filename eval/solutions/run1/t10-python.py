import sys
from heapq import heappush, heappop

def main():
    data = sys.stdin.buffer.read().split()
    n = int(data[0]); m = int(data[1])
    end = 2 + 3 * m
    s = int(data[end]); t = int(data[end + 1])
    if s == t:
        sys.stdout.write("cost 0\npath %d\n" % s)
        return
    U = list(map(int, data[2:end:3]))
    V = list(map(int, data[3:end:3]))
    W = list(map(int, data[4:end:3]))
    del data
    adjv = [[] for _ in range(n)]
    adjw = [[] for _ in range(n)]
    for u, v, w in zip(U, V, W):
        if u != v:
            adjv[u].append(v)
            adjw[u].append(w)
    del U, V, W
    SH = 18
    MASK = (1 << SH) - 1
    INF = 1 << 62
    dist = [INF] * n
    par = [-1] * n
    done = [False] * n
    dist[s] = 0
    heap = [s]
    found = False
    while heap:
        k = heappop(heap)
        u = k & MASK
        if done[u]:
            continue
        if u == t:
            found = True
            break
        done[u] = True
        d = k >> SH
        for v, w in zip(adjv[u], adjw[u]):
            nd = d + w
            if nd < dist[v]:
                dist[v] = nd
                par[v] = u
                heappush(heap, (nd << SH) | v)
    if not found:
        sys.stdout.write("no path\n")
        return
    path = []
    x = t
    while x != -1:
        path.append(x)
        if x == s:
            break
        x = par[x]
    path.reverse()
    sys.stdout.write("cost %d\npath %s\n" % (dist[t], " ".join(map(str, path))))

main()
