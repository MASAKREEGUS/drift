use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::io::{self, Read, Write};

fn main() {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input).unwrap();
    let text = String::from_utf8_lossy(&input);
    let mut ids: HashMap<&str, usize> = HashMap::new();
    let mut names: Vec<&str> = Vec::new();
    let mut raw_edges: Vec<(usize, usize)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (parts, is_edge): ([&str; 2], bool) = match line.find("->") {
            Some(p) => ([line[..p].trim(), line[p + 2..].trim()], true),
            None => ([line, ""], false),
        };
        let cnt = if is_edge { 2 } else { 1 };
        let mut got = [0usize; 2];
        for k in 0..cnt {
            let s = parts[k];
            let id = match ids.get(s) {
                Some(&i) => i,
                None => {
                    let i = names.len();
                    ids.insert(s, i);
                    names.push(s);
                    i
                }
            };
            got[k] = id;
        }
        if is_edge {
            raw_edges.push((got[0], got[1]));
        }
    }
    let n = names.len();
    // rank nodes by byte order
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by(|&a, &b| names[a].as_bytes().cmp(names[b].as_bytes()));
    let mut rank = vec![0usize; n];
    for (r, &i) in order.iter().enumerate() {
        rank[i] = r;
    }
    let sorted_names: Vec<&str> = order.iter().map(|&i| names[i]).collect();
    let mut edges: Vec<(usize, usize)> = raw_edges.iter().map(|&(a, b)| (rank[a], rank[b])).collect();
    edges.sort_unstable();
    edges.dedup();
    // CSR adjacency
    let mut start = vec![0usize; n + 1];
    for &(a, _) in &edges {
        start[a + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let adj: Vec<usize> = edges.iter().map(|&(_, b)| b).collect();
    let mut indeg = vec![0usize; n];
    for &(_, b) in &edges {
        indeg[b] += 1;
    }
    let mut heap: BinaryHeap<Reverse<usize>> = BinaryHeap::new();
    for i in 0..n {
        if indeg[i] == 0 {
            heap.push(Reverse(i));
        }
    }
    let mut result: Vec<usize> = Vec::with_capacity(n);
    while let Some(Reverse(u)) = heap.pop() {
        result.push(u);
        for &v in &adj[start[u]..start[u + 1]] {
            indeg[v] -= 1;
            if indeg[v] == 0 {
                heap.push(Reverse(v));
            }
        }
    }
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    if result.len() == n {
        for &u in &result {
            out.write_all(sorted_names[u].as_bytes()).unwrap();
            out.write_all(b"\n").unwrap();
        }
        out.flush().unwrap();
        return;
    }
    // find a cycle with iterative DFS among remaining nodes
    let mut color = vec![0u8; n]; // 0 white, 1 on stack, 2 done
    let mut pos_in_stack = vec![usize::MAX; n];
    let mut cycle: Vec<usize> = Vec::new();
    'outer: for s in 0..n {
        if indeg[s] == 0 || color[s] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(s, start[s])];
        color[s] = 1;
        pos_in_stack[s] = 0;
        while let Some(top) = stack.last_mut() {
            let u = top.0;
            if top.1 < start[u + 1] {
                let v = adj[top.1];
                top.1 += 1;
                if indeg[v] == 0 {
                    continue;
                }
                if color[v] == 1 {
                    let p = pos_in_stack[v];
                    cycle = stack[p..].iter().map(|&(x, _)| x).collect();
                    break 'outer;
                } else if color[v] == 0 {
                    color[v] = 1;
                    pos_in_stack[v] = stack.len();
                    stack.push((v, start[v]));
                }
            } else {
                color[u] = 2;
                stack.pop();
            }
        }
    }
    let mut line = String::from("cycle: ");
    for &u in &cycle {
        line.push_str(sorted_names[u]);
        line.push_str(" -> ");
    }
    line.push_str(sorted_names[cycle[0]]);
    line.push('\n');
    out.write_all(line.as_bytes()).unwrap();
    out.flush().unwrap();
    drop(out);
    std::process::exit(1);
}
