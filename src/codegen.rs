//! C11 code generation from a checked program.
//!
//! Every Drift value is a 16-byte `V`. Ownership is uniform: each expression yields an owned (+1)
//! value that its consumer takes over. Locals are hoisted to the top of the C function and released
//! at its single exit (`out:`), so `return`, `?`, `break` and `continue` need no cleanup code.
//! Mutation goes through `V*` slots made unique first (copy-on-write).

use crate::ast::*;
use crate::check::{subst, DefKind, Note, Owner, Program, Ty, VFields};
use std::collections::{BTreeSet, HashMap, HashSet};

pub const RT_H: &str = include_str!("runtime/drift_rt.h");
pub const RT_C: &str = include_str!("runtime/drift_rt.c");

pub enum Target {
    /// Program entry: the `main` function id.
    Run { main: usize },
    /// Test harness over every `examples` and `test` block; `todo` panics when reached.
    Test,
    /// Freestanding kernel: no `main(argc, argv)`, no sandbox; the boot code calls `drift_kmain`.
    Kernel { main: usize },
}

#[derive(Clone)]
enum Loc {
    Var(String),
    Ptr(String),
    Env(usize),
}

struct Ctx {
    module: usize,
    decls: Vec<String>,
    owned: Vec<String>,
    body: String,
    scopes: Vec<Vec<(String, Loc)>>,
    captures: Vec<String>,
    closure: bool,
    /// Last `D_AT` location emitted, restored after calls so panics point at the caller's line.
    at: Option<(usize, usize)>,
}

impl Ctx {
    fn new(module: usize, closure: bool) -> Ctx {
        Ctx { module, decls: Vec::new(), owned: Vec::new(), body: String::new(), scopes: vec![Vec::new()], captures: Vec::new(), closure, at: None }
    }
}

struct Gen<'p> {
    p: &'p Program,
    ctxs: Vec<Ctx>,
    n: usize,
    funcs: String,
    protos: String,
    lits: Vec<(Vec<u8>, bool)>,
    lit_ids: HashMap<(Vec<u8>, bool), usize>,
    natives: BTreeSet<usize>,
    fn_wrappers: BTreeSet<usize>,
    ctor_wrappers: BTreeSet<(usize, usize)>,
    lines: Vec<Vec<usize>>,
    implemented: HashSet<String>,
    /// Runtime type descriptors (`DTy`) for reified generics, keyed by the type.
    tys: HashMap<String, String>,
    ty_fwd: String,
    ty_out: String,
}

fn c_str(s: &[u8]) -> String {
    let mut o = String::from("\"");
    for &b in s {
        match b {
            b'"' => o.push_str("\\\""),
            b'\\' => o.push_str("\\\\"),
            b'\n' => o.push_str("\\n"),
            b'?' => o.push_str("\\077"),
            0x20..=0x7e => o.push(b as char),
            _ => o.push_str(&format!("\\{:03o}", b)),
        }
    }
    o.push('"');
    o
}

fn int_lit(s: &str) -> i128 {
    let t: String = s.chars().filter(|c| *c != '_').collect();
    let (digits, radix) = if let Some(x) = t.strip_prefix("0x") {
        (x.to_string(), 16)
    } else if let Some(x) = t.strip_prefix("0b") {
        (x.to_string(), 2)
    } else if let Some(x) = t.strip_prefix("0o") {
        (x.to_string(), 8)
    } else {
        (t, 10)
    };
    i128::from_str_radix(&digits, radix).unwrap_or(0)
}

fn c_int(n: i128) -> String {
    if n == i64::MIN as i128 { "INT64_MIN".into() } else { format!("INT64_C({})", n) }
}

fn float_lit(s: &str) -> String {
    let t: String = s.chars().filter(|c| *c != '_').collect();
    if t.contains(['.', 'e', 'E']) { t } else { format!("{}.0", t) }
}

/// Effect bits of a `uses` list (unknown names were already reported by the checker).
fn effect_bits(uses: &[Ident]) -> u16 {
    uses.iter().filter_map(|u| crate::check::EFFECTS.iter().position(|e| *e == u.name)).fold(0, |b, i| b | 1 << i)
}

/// Names of natives defined in the runtime (`V d_n_...(` at the start of a line).
fn implemented_natives() -> HashSet<String> {
    RT_C.lines()
        .filter_map(|l| l.strip_prefix("V d_n_"))
        .filter_map(|rest| rest.split('(').next())
        .map(|n| format!("d_n_{}", n))
        .collect()
}

pub fn generate(p: &Program, target: &Target) -> String {
    let mut g = Gen {
        p,
        ctxs: Vec::new(),
        n: 0,
        funcs: String::new(),
        protos: String::new(),
        lits: Vec::new(),
        lit_ids: HashMap::new(),
        natives: BTreeSet::new(),
        fn_wrappers: BTreeSet::new(),
        ctor_wrappers: BTreeSet::new(),
        lines: p.files.iter().map(|(_, src, _)| std::iter::once(0).chain(src.match_indices('\n').map(|(i, _)| i + 1)).collect()).collect(),
        implemented: implemented_natives(),
        tys: HashMap::new(),
        ty_fwd: String::new(),
        ty_out: String::new(),
    };
    for fid in 0..p.fns.len() {
        if !p.mods[p.fns[fid].module].is_std_mod() {
            g.compile_fn(fid);
        }
    }
    for c in 0..p.consts.len() {
        if !p.mods[p.consts[c].module].is_std_mod() {
            g.compile_const(c);
        }
    }
    let mut tests = Vec::new();
    let mut main_code = String::new();
    match target {
        Target::Test => {
            for m in 0..p.mods.len() {
                if p.mods[m].is_std_mod() {
                    continue;
                }
                for it in &p.asts[m].items {
                    match &it.kind {
                        ItemKind::Examples { target, cases } => {
                            let f = g.compile_examples(m, cases);
                            tests.push((format!("examples {}", target.name), f, true, m, g.line(m, it.span.lo), 0));
                        }
                        ItemKind::Test { name, body, uses } => {
                            let f = g.compile_test(m, body);
                            let bits = effect_bits(uses);
                            tests.push((format!("test {:?}", name), f, false, m, g.line(m, it.span.lo), bits));
                        }
                        _ => {}
                    }
                }
            }
            let entries: Vec<String> = tests
                .iter()
                .map(|(name, f, ex, m, line, bits)| format!("  {{{}, {}, {}, F{}, {}, {}}},", c_str(name.as_bytes()), f, *ex as i32, m, line, bits))
                .collect();
            main_code.push_str(&format!(
                "static const DTest d_tests[] = {{\n{}\n  {{0, 0, 0, 0, 0, 0}}\n}};\n\nint main(int argc, char** argv) {{\n  d_init(argc, argv);\n  d_init_lits();\n  return d_run_tests(d_tests, {}, argc, argv);\n}}\n",
                entries.join("\n"),
                tests.len()
            ));
        }
        Target::Kernel { main } => {
            let info = &p.fns[*main];
            let is_result = match &p.asts[info.module].items[info.item].kind {
                ItemKind::Fn(f) => matches!(&f.ret.kind, TypeKind::Named { path, .. } if path.last().map_or(false, |x| x.name == "Result")),
                _ => false,
            };
            main_code.push_str(&format!("int drift_kmain(void) {{\n  d_init_lits();\n  return d_finish_main(f_{}(), {});\n}}\n", main, is_result as i32));
        }
        Target::Run { main } => {
            let info = &p.fns[*main];
            let is_result = match &p.asts[info.module].items[info.item].kind {
                ItemKind::Fn(f) => matches!(&f.ret.kind, TypeKind::Named { path, .. } if path.last().map_or(false, |x| x.name == "Result")),
                _ => false,
            };
            main_code.push_str(&format!(
                "int main(int argc, char** argv) {{\n  d_init(argc, argv);\n  d_sandbox({});\n  d_init_lits();\n  return d_run_main(f_{}, {});\n}}\n",
                info.effects, main, is_result as i32
            ));
        }
    }
    g.assemble(main_code)
}

/// Std functions the runtime does not implement yet (they panic when called).
pub fn missing_natives(p: &Program) -> Vec<String> {
    let g = Gen {
        p,
        ctxs: Vec::new(),
        n: 0,
        funcs: String::new(),
        protos: String::new(),
        lits: Vec::new(),
        lit_ids: HashMap::new(),
        natives: BTreeSet::new(),
        fn_wrappers: BTreeSet::new(),
        ctor_wrappers: BTreeSet::new(),
        lines: Vec::new(),
        implemented: implemented_natives(),
        tys: HashMap::new(),
        ty_fwd: String::new(),
        ty_out: String::new(),
    };
    let mut out: Vec<String> = (0..p.fns.len())
        .filter(|&f| p.mods[p.fns[f].module].is_std)
        .map(|f| g.native_name(f))
        .filter(|n| !g.implemented.contains(n))
        .collect();
    out.sort();
    out.dedup();
    out
}

trait StdMod {
    fn is_std_mod(&self) -> bool;
}
impl StdMod for crate::check::ModInfo {
    fn is_std_mod(&self) -> bool {
        self.is_std
    }
}

impl<'p> Gen<'p> {
    // ------------------------------------------------------------ plumbing

    fn cur(&mut self) -> &mut Ctx {
        self.ctxs.last_mut().unwrap()
    }
    fn module(&self) -> usize {
        self.ctxs.last().unwrap().module
    }
    fn emit(&mut self, s: &str) {
        let c = self.cur();
        c.body.push_str("  ");
        c.body.push_str(s);
        c.body.push('\n');
    }
    fn fresh(&mut self, prefix: &str) -> String {
        self.n += 1;
        format!("{}{}", prefix, self.n)
    }
    fn tmp(&mut self, code: impl AsRef<str>) -> String {
        let t = self.fresh("t");
        self.emit(&format!("V {} = {};", t, code.as_ref()));
        t
    }
    fn hoist(&mut self, prefix: &str) -> String {
        let h = self.fresh(prefix);
        self.cur().decls.push(h.clone());
        h
    }
    fn declare(&mut self, name: &str) -> String {
        let c = self.hoist(&format!("l_{}_", name));
        self.cur().scopes.last_mut().unwrap().push((name.to_string(), Loc::Var(c.clone())));
        c
    }
    fn line(&self, m: usize, pos: u32) -> usize {
        match self.lines[m].binary_search(&(pos as usize)) {
            Ok(i) => i + 1,
            Err(i) => i,
        }
    }
    fn at(&mut self, span: crate::diag::Span) {
        let m = self.module();
        let l = self.line(m, span.lo);
        self.cur().at = Some((m, l));
        self.emit(&format!("D_AT(F{}, {});", m, l));
    }
    /// Emit a call that may run other Drift code, then restore this line's location.
    fn call_tmp(&mut self, code: String) -> String {
        let t = self.tmp(code);
        if let Some((m, l)) = self.cur().at {
            self.emit(&format!("D_AT(F{}, {});", m, l));
        }
        t
    }
    fn src_c(&self, span: crate::diag::Span) -> String {
        let src = &self.p.files[self.module()].1;
        c_str(src.get(span.lo as usize..span.hi as usize).unwrap_or("").as_bytes())
    }
    fn note(&self, span: crate::diag::Span, kind: u8) -> Option<Note> {
        self.p.notes.get(&(self.module(), span.lo, span.hi, kind)).cloned()
    }
    fn lit(&mut self, bytes: &[u8], is_bytes: bool) -> String {
        let key = (bytes.to_vec(), is_bytes);
        if let Some(i) = self.lit_ids.get(&key) {
            return format!("K{}", i);
        }
        let i = self.lits.len();
        self.lits.push(key.clone());
        self.lit_ids.insert(key, i);
        format!("K{}", i)
    }

    fn lookup(&mut self, name: &str) -> Option<Loc> {
        let d = self.ctxs.len() - 1;
        self.lookup_at(d, name)
    }
    fn lookup_at(&mut self, depth: usize, name: &str) -> Option<Loc> {
        for scope in self.ctxs[depth].scopes.iter().rev() {
            if let Some((_, l)) = scope.iter().rev().find(|(n, _)| n == name) {
                return Some(l.clone());
            }
        }
        if let Some(i) = self.ctxs[depth].captures.iter().position(|c| c == name) {
            return Some(Loc::Env(i));
        }
        if depth > 0 && self.ctxs[depth].closure && self.lookup_at(depth - 1, name).is_some() {
            self.ctxs[depth].captures.push(name.to_string());
            return Some(Loc::Env(self.ctxs[depth].captures.len() - 1));
        }
        None
    }
    /// Static scalar kind of an expression: 1 Int, 2 Bool, 3 Float, 0 anything else (see `Program::scalars`).
    fn scalar(&self, e: &Expr) -> u8 {
        self.p.scalars.get(&(self.module(), e.span.lo, e.span.hi)).copied().unwrap_or(0)
    }
    /// Read a variable known to hold a scalar: a plain copy, no reference to count.
    fn read_scalar(&mut self, l: &Loc) -> String {
        let code = match l {
            Loc::Var(c) => c.clone(),
            Loc::Ptr(c) => format!("*{}", c),
            Loc::Env(i) => format!("fo->env[{}]", i),
        };
        self.tmp(code)
    }
    fn read(&mut self, l: &Loc) -> String {
        let code = match l {
            Loc::Var(c) => format!("d_retain({})", c),
            Loc::Ptr(c) => format!("d_retain(*{})", c),
            Loc::Env(i) => format!("d_retain(fo->env[{}])", i),
        };
        self.tmp(code)
    }

    /// Close the current context into a C function with the given signature.
    fn finish(&mut self, sig: &str) {
        let ctx = self.ctxs.pop().unwrap();
        let mut f = String::new();
        f.push_str(sig);
        f.push_str(" {\n  V ret = D_UNIT;\n");
        for d in &ctx.decls {
            f.push_str(&format!("  V {} = D_UNIT;\n", d));
        }
        f.push_str(&ctx.body);
        f.push_str("out: ;\n");
        for d in &ctx.decls {
            f.push_str(&format!("  d_release({});\n", d));
        }
        for o in &ctx.owned {
            f.push_str(&format!("  d_release({});\n", o));
        }
        f.push_str("  return ret;\n}\n\n");
        self.funcs.push_str(&f);
        self.protos.push_str(sig);
        self.protos.push_str(";\n");
    }

    // ------------------------------------------------------------ program tables

    fn is_std(&self, module: usize) -> bool {
        self.p.mods[module].is_std
    }
    fn short(&self, module: usize) -> String {
        self.p.mods[module].path.trim_start_matches("std.").replace('.', "_")
    }
    fn td(&self, d: usize) -> String {
        let def = &self.p.defs[d];
        if self.is_std(def.module) { format!("d_td_{}_{}", self.short(def.module), def.name) } else { format!("td_{}", d) }
    }
    fn native_name(&self, fid: usize) -> String {
        let f = &self.p.fns[fid];
        let owner = if f.is_method {
            match &f.params[0].ty {
                Ty::Named(d, _) => format!("{}_", self.p.defs[*d].name),
                _ => String::new(),
            }
        } else if let Some(d) = f.owner {
            format!("{}__", self.p.defs[d].name)
        } else {
            String::new()
        };
        format!("d_n_{}_{}{}", self.short(f.module), owner, f.name)
    }
    fn fn_cname(&mut self, fid: usize) -> String {
        if self.is_std(self.p.fns[fid].module) {
            self.natives.insert(fid);
            self.native_name(fid)
        } else {
            format!("f_{}", fid)
        }
    }
    fn is_reified(&self, fid: usize) -> bool {
        let f = &self.p.fns[fid];
        matches!((self.p.mods[f.module].path.as_str(), f.name.as_str()), ("std.json", "decode") | ("std.db", "query") | ("std.db", "query_one"))
    }
    fn fn_sig_params(&self, fid: usize, named: bool) -> String {
        let mut ps: Vec<String> = self.p.fns[fid]
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let t = if p.mutable { "V*" } else { "V" };
                if named { format!("{} {}{}", t, if p.mutable { "p" } else { "a" }, i) } else { t.to_string() }
            })
            .collect();
        if self.is_reified(fid) {
            ps.push(if named { "const DTy* ty".into() } else { "const DTy*".into() });
        }
        if ps.is_empty() { "void".into() } else { ps.join(", ") }
    }
    /// Field names (or positional indices) of a struct or of variant `vi` of an enum.
    fn field_names(&self, d: usize, vi: Option<usize>) -> Vec<String> {
        match (&self.p.defs[d].kind, vi) {
            (DefKind::Struct(fs), _) => fs.iter().map(|(n, _)| n.clone()).collect(),
            (DefKind::Enum(vs), Some(i)) => match &vs[i].1 {
                VFields::Named(fs) => fs.iter().map(|(n, _)| n.clone()).collect(),
                VFields::Tuple(ts) => (0..ts.len()).map(|i| i.to_string()).collect(),
                VFields::Unit => vec![],
            },
            _ => vec![],
        }
    }
    /// A static `DTy` describing `t` (fully concrete), shared by every use of the same type.
    fn ty_desc(&mut self, t: &Ty) -> String {
        if let Ty::Named(d, args) = t {
            if let DefKind::Alias(x) = &self.p.defs[*d].kind {
                return self.ty_desc(&subst(x, Owner::Def(*d), args));
            }
        }
        let key = format!("{:?}", t);
        if let Some(n) = self.tys.get(&key) {
            return n.clone();
        }
        let name = format!("ty_{}", self.tys.len());
        self.tys.insert(key, name.clone());
        self.ty_fwd.push_str(&format!("static const DTy {};\n", name));
        let c = self.p.core;
        let arr = |g: &mut Self, suffix: &str, ts: &[Ty]| -> String {
            if ts.is_empty() {
                return "NULL".into();
            }
            let names: Vec<String> = ts.iter().map(|x| format!("&{}", g.ty_desc(x))).collect();
            let a = format!("{}_{}", name, suffix);
            g.ty_out.push_str(&format!("static const DTy* const {}[] = {{{}}};\n", a, names.join(", ")));
            a
        };
        let (k, num, n, a, td, vf): (&str, i32, usize, String, String, String) = match t {
            Ty::Tuple(ts) if ts.is_empty() => ("K_UNIT", 0, 0, "NULL".into(), "NULL".into(), "NULL".into()),
            Ty::Tuple(ts) => {
                let a = arr(self, "a", ts);
                ("K_TUPLE", 0, ts.len(), a, "NULL".into(), "NULL".into())
            }
            Ty::Named(d, args) => {
                let d = *d;
                let nums = [(c.int, "K_INT", 0), (c.i32, "K_INT", 1), (c.u8, "K_INT", 2), (c.u32, "K_INT", 3), (c.u64, "K_INT", 4), (c.float, "K_FLOAT", 0), (c.f32, "K_FLOAT", 1), (c.bool, "K_BOOL", 0), (c.str, "K_STR", 0), (c.bytes, "K_BYTES", 0)];
                if let Some((_, k, num)) = nums.iter().find(|(x, _, _)| *x == d) {
                    (*k, *num, 0, "NULL".into(), "NULL".into(), "NULL".into())
                } else if d == c.list || d == c.set || d == c.map {
                    let a = arr(self, "a", args);
                    (if d == c.list { "K_LIST" } else if d == c.set { "K_SET" } else { "K_MAP" }, 0, args.len(), a, "NULL".into(), "NULL".into())
                } else {
                    let o = Owner::Def(d);
                    let variants: Vec<Vec<Ty>> = match &self.p.defs[d].kind {
                        DefKind::Struct(fs) => vec![fs.iter().map(|(_, x)| subst(x, o, args)).collect()],
                        DefKind::Enum(vs) => vs
                            .iter()
                            .map(|(_, v)| match v {
                                VFields::Unit => vec![],
                                VFields::Tuple(ts) => ts.iter().map(|x| subst(x, o, args)).collect(),
                                VFields::Named(fs) => fs.iter().map(|(_, x)| subst(x, o, args)).collect(),
                            })
                            .collect(),
                        _ => vec![],
                    };
                    let vnames: Vec<String> = variants.iter().enumerate().map(|(i, fs)| arr(self, &format!("v{}", i), fs)).collect();
                    let vf = format!("{}_vf", name);
                    self.ty_out.push_str(&format!("static const DTy* const* const {}[] = {{{}}};\n", vf, if vnames.is_empty() { "NULL".into() } else { vnames.join(", ") }));
                    ("K_ADT", 0, variants.len(), "NULL".into(), format!("&{}", self.td(d)), vf)
                }
            }
            _ => ("K_UNIT", 0, 0, "NULL".into(), "NULL".into(), "NULL".into()),
        };
        self.ty_out.push_str(&format!("static const DTy {} = {{{}, {}, {}, {}, {}, {}}};\n", name, k, num, n, a, td, vf));
        name
    }
    fn variant_count(&self, d: usize) -> usize {
        match &self.p.defs[d].kind {
            DefKind::Enum(vs) => vs.len(),
            _ => 1,
        }
    }

    // ------------------------------------------------------------ items

    fn compile_fn(&mut self, fid: usize) {
        let info = &self.p.fns[fid];
        let m = info.module;
        let ItemKind::Fn(f) = &self.p.asts[m].items[info.item].kind else { return };
        let name = c_str(info.name.as_bytes());
        self.ctxs.push(Ctx::new(m, false));
        for (i, prm) in f.params.iter().enumerate() {
            let loc = if prm.mutable {
                Loc::Ptr(format!("p{}", i))
            } else {
                self.cur().owned.push(format!("a{}", i));
                Loc::Var(format!("a{}", i))
            };
            self.cur().scopes[0].push((prm.name.name.clone(), loc));
        }
        for r in &f.requires {
            self.at(r.span);
            let c = self.expr(r);
            let src = self.src_c(r.span);
            self.emit(&format!("if (!{}.i) d_contract_fail(\"requires\", {}, {});", c, name, src));
        }
        let v = self.block(&f.body, true);
        self.emit(&format!("ret = {};", v));
        if !f.ensures.is_empty() {
            // every exit (`return`, `?`) runs the postconditions first
            let lbl = self.fresh("ens");
            let body = std::mem::take(&mut self.cur().body);
            self.cur().body = body.replace("goto out;", &format!("goto {};", lbl));
            self.cur().body.push_str(&format!("{}: ;\n", lbl));
            self.cur().scopes.push(Vec::new());
            let rc = self.declare("result");
            self.emit(&format!("d_release({}); {} = d_retain(ret);", rc, rc));
            for e in &f.ensures {
                self.at(e.span);
                let c = self.expr(e);
                let src = self.src_c(e.span);
                self.emit(&format!("if (!{}.i) d_contract_fail(\"ensures\", {}, {});", c, name, src));
            }
            self.cur().scopes.pop();
        }
        let sig = format!("static V f_{}({})", fid, self.fn_sig_params(fid, true));
        self.finish(&sig);
    }

    fn compile_const(&mut self, c: usize) {
        let ci = &self.p.consts[c];
        let ItemKind::Const { value, .. } = &self.p.asts[ci.module].items[ci.item].kind else { return };
        self.ctxs.push(Ctx::new(ci.module, false));
        self.at(value.span);
        let v = self.expr(value);
        self.emit(&format!("ret = {};", v));
        self.finish(&format!("static V c_{}_init(void)", c));
        self.funcs.push_str(&format!(
            "static V c_{c}_v;\nstatic int c_{c}_s;\nstatic V c_{c}(void) {{\n  if (!__atomic_load_n(&c_{c}_s, __ATOMIC_ACQUIRE)) {{\n    d_const_lock();\n    if (!c_{c}_s) {{ c_{c}_v = c_{c}_init(); d_immortal(c_{c}_v); __atomic_store_n(&c_{c}_s, 1, __ATOMIC_RELEASE); }}\n    d_const_unlock();\n  }}\n  return c_{c}_v;\n}}\n\n",
            c = c
        ));
        self.protos.push_str(&format!("static V c_{}(void);\n", c));
    }

    fn compile_examples(&mut self, m: usize, cases: &[Expr]) -> String {
        let name = self.fresh("ex_");
        self.ctxs.push(Ctx::new(m, false));
        self.emit("ret = d_bool(1);");
        for case in cases {
            self.at(case.span);
            let src = self.src_c(case.span);
            let line = self.line(m, case.span.lo);
            if let ExprKind::Binary { op: BinOp::Eq, lhs, rhs } = &case.kind {
                let l = self.expr(lhs);
                let l = self.tmp(&l);
                let r = self.expr(rhs);
                let r = self.tmp(&r);
                self.emit(&format!("if (!d_eq_b({l}, {r})) {{ d_example_fail(F{m}, {line}, {src}, {l}, {r}); ret = d_bool(0); }}"));
                self.emit(&format!("d_release({}); d_release({});", l, r));
            } else {
                let c = self.expr(case);
                self.emit(&format!("if (!{}.i) {{ d_example_false(F{}, {}, {}); ret = d_bool(0); }}", c, m, line, src));
            }
        }
        self.finish(&format!("static V {}(void)", name));
        name
    }

    fn compile_test(&mut self, m: usize, body: &Block) -> String {
        let name = self.fresh("test_");
        self.ctxs.push(Ctx::new(m, false));
        self.block(body, false);
        self.emit("ret = d_ok(D_UNIT);");
        self.finish(&format!("static V {}(void)", name));
        name
    }

    // ------------------------------------------------------------ statements

    fn block(&mut self, b: &Block, value: bool) -> String {
        self.cur().scopes.push(Vec::new());
        let mut out = "D_UNIT".to_string();
        let n = b.stmts.len();
        for (i, s) in b.stmts.iter().enumerate() {
            if value && i + 1 == n {
                if let StmtKind::Expr(e) = &s.kind {
                    self.at(s.span);
                    out = self.expr(e);
                    break;
                }
            }
            self.stmt(s);
        }
        self.cur().scopes.pop();
        out
    }

    /// Bind an owned value to a pattern (irrefutable).
    fn bind_owned(&mut self, pat: &Pat, v: &str) {
        match &pat.kind {
            PatKind::Bind(n) => {
                let c = self.declare(n);
                self.emit(&format!("d_release({c}); {c} = {v};"));
            }
            PatKind::Wildcard => self.emit(&format!("d_release({});", v)),
            _ => {
                let h = self.hoist("d");
                self.emit(&format!("d_release({h}); {h} = {v};"));
                self.pat_bind(pat, &h);
            }
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        self.at(s.span);
        match &s.kind {
            StmtKind::Let { pat, value, .. } => {
                let v = self.expr(value);
                if let (PatKind::Bind(n), true) = (&pat.kind, self.scalar(value) != 0) {
                    // a scalar never needs releasing, and this variable only ever holds scalars
                    let c = self.declare(n);
                    self.emit(&format!("{c} = {v};"));
                    return;
                }
                self.bind_owned(pat, &v);
            }
            StmtKind::Assign { target, op, value } => {
                let v = self.expr(value);
                let p = self.lvalue(target, *op == AssignOp::Set);
                let int = self.scalar(value) == 1 && self.scalar(target) == 1;
                let f = match op {
                    AssignOp::Set => {
                        self.emit(&format!("d_release(*{p}); *{p} = {v};"));
                        return;
                    }
                    AssignOp::Add if int => "d_addi",
                    AssignOp::Sub if int => "d_subi",
                    AssignOp::Mul if int => "d_muli",
                    AssignOp::Div if int => "d_divi",
                    AssignOp::Rem if int => "d_remi",
                    AssignOp::Add => "d_add",
                    AssignOp::Sub => "d_sub",
                    AssignOp::Mul => "d_mul",
                    AssignOp::Div => "d_div",
                    AssignOp::Rem => "d_rem",
                };
                self.emit(&format!("*{p} = {f}(*{p}, {v});"));
            }
            StmtKind::For { pat, iter, body } => {
                if let (ExprKind::Range { lo, hi, inclusive }, PatKind::Bind(name)) = (&iter.kind, &pat.kind) {
                    if self.scalar(lo) == 1 && self.scalar(hi) == 1 {
                        // `for i in a..b` over Ints: a plain counted loop, no range object
                        let a = self.expr(lo);
                        let b = self.expr(hi);
                        let (i, n) = (self.fresh("i"), self.fresh("n"));
                        self.emit(&format!("int64_t {n} = {b}.i;"));
                        if *inclusive {
                            self.emit(&format!("if ({n} == INT64_MAX) d_panic_c(\"integer overflow in `..=`\"); {n}++;"));
                        }
                        self.emit(&format!("for (int64_t {i} = {a}.i; {i} < {n}; {i}++) {{"));
                        self.cur().scopes.push(Vec::new());
                        let c = self.declare(name);
                        self.emit(&format!("{c} = d_int({i});"));
                        self.block(body, false);
                        self.cur().scopes.pop();
                        self.emit("}");
                        return;
                    }
                }
                let it = self.expr(iter);
                let ic = self.hoist("it");
                self.emit(&format!("d_release({ic}); {ic} = {it};"));
                let (i, n) = (self.fresh("i"), self.fresh("n"));
                self.emit(&format!("for (int64_t {i} = 0, {n} = d_iter_len({ic}); {i} < {n}; {i}++) {{"));
                self.cur().scopes.push(Vec::new());
                self.bind_owned(pat, &format!("d_iter_at({}, {})", ic, i));
                self.block(body, false);
                self.cur().scopes.pop();
                self.emit("}");
            }
            StmtKind::While { cond, body } => {
                self.emit("for (;;) {");
                let c = self.expr(cond);
                self.emit(&format!("if (!{}.i) break;", c));
                self.block(body, false);
                self.emit("}");
            }
            StmtKind::Loop { body } => {
                self.emit("for (;;) {");
                self.block(body, false);
                self.emit("}");
            }
            StmtKind::Assert { cond, message } => {
                let src = self.src_c(cond.span);
                if let ExprKind::Binary { op: BinOp::Eq, lhs, rhs } = &cond.kind {
                    let l = self.expr(lhs);
                    let l = self.tmp(&l);
                    let r = self.expr(rhs);
                    let r = self.tmp(&r);
                    self.emit(&format!("if (!d_eq_b({}, {})) {{", l, r));
                    let m = match message {
                        Some(m) => self.expr(m),
                        None => "D_UNIT".into(),
                    };
                    self.emit(&format!("d_assert_eq_fail({src}, {l}, {r}, {m});"));
                    self.emit("}");
                    self.emit(&format!("d_release({}); d_release({});", l, r));
                } else {
                    let c = self.expr(cond);
                    self.emit(&format!("if (!{}.i) {{", c));
                    let m = match message {
                        Some(m) => self.expr(m),
                        None => "D_UNIT".into(),
                    };
                    self.emit(&format!("d_assert_fail({}, {});", src, m));
                    self.emit("}");
                }
            }
            StmtKind::Expr(e) => {
                let v = self.expr(e);
                if v != "D_UNIT" && self.scalar(e) == 0 {
                    self.emit(&format!("d_release({});", v));
                }
            }
        }
    }

    /// A `V*` to the storage named by `e`, made unique so it can be written.
    fn lvalue(&mut self, e: &Expr, insert: bool) -> String {
        match &e.kind {
            ExprKind::Name(n) => match self.lookup(n) {
                Some(Loc::Var(c)) => format!("&{}", c),
                Some(Loc::Ptr(c)) => c,
                _ => "(V*)0".into(),
            },
            ExprKind::Field { base, .. } => {
                let Some(Note::Field(i)) = self.note(e.span, b'p') else { return "(V*)0".into() };
                let b = self.lvalue(base, false);
                let p = self.fresh("p");
                self.emit(&format!("V* {} = d_mut_field({}, {});", p, b, i));
                p
            }
            ExprKind::Index { base, index } => {
                let k = self.expr(index);
                let b = self.lvalue(base, false);
                let p = self.fresh("p");
                self.emit(&format!("V* {} = d_mut_index({}, {}, {});", p, b, k, insert as i32));
                p
            }
            _ => "(V*)0".into(),
        }
    }

    // ------------------------------------------------------------ patterns

    fn pat_fields(&self, pat: &Pat) -> Vec<String> {
        match self.note(pat.span, b'P') {
            Some(Note::Variant(d, i)) => self.field_names(d, Some(i)),
            Some(Note::Struct(d)) => self.field_names(d, None),
            _ => vec![],
        }
    }

    fn lit_test(&mut self, e: &Expr, v: &str) -> String {
        match &e.kind {
            ExprKind::Int(s) => format!("({v}).i == {}", c_int(int_lit(s))),
            ExprKind::Float(s) => format!("({v}).f == {}", float_lit(s)),
            ExprKind::Bool(b) => format!("({v}).i == {}", *b as i32),
            ExprKind::Unary { op: UnOp::Neg, expr } => match &expr.kind {
                ExprKind::Int(s) => format!("({v}).i == {}", c_int(-int_lit(s))),
                ExprKind::Float(s) => format!("({v}).f == -{}", float_lit(s)),
                _ => "0".into(),
            },
            ExprKind::Str(parts) => {
                let text: String = parts.iter().map(|p| if let StrPart::Lit(s) = p { s.as_str() } else { "" }).collect();
                let k = self.lit(text.as_bytes(), false);
                format!("d_eq_b({}, {})", v, k)
            }
            ExprKind::RawStr(s) => {
                let k = self.lit(s.as_bytes(), false);
                format!("d_eq_b({}, {})", v, k)
            }
            ExprKind::Bytes(b) => {
                let k = self.lit(b, true);
                format!("d_eq_b({}, {})", v, k)
            }
            _ => "0".into(),
        }
    }

    /// C condition testing whether borrowed value `v` matches `pat`.
    fn pat_test(&mut self, pat: &Pat, v: &str) -> String {
        let conj = |parts: Vec<String>| -> String {
            let parts: Vec<String> = parts.into_iter().filter(|p| p != "1").collect();
            if parts.is_empty() { "1".into() } else { format!("({})", parts.join(" && ")) }
        };
        match &pat.kind {
            PatKind::Wildcard | PatKind::Bind(_) => "1".into(),
            PatKind::Lit(e) => self.lit_test(e, v),
            PatKind::Tuple(items) => {
                let parts = items.iter().enumerate().map(|(i, it)| self.pat_test(it, &format!("d_get({}, {})", v, i))).collect();
                conj(parts)
            }
            PatKind::List { items, rest } => {
                let mut parts = vec![format!("d_llen({}) {} {}", v, if rest.is_some() { ">=" } else { "==" }, items.len())];
                for (i, it) in items.iter().enumerate() {
                    parts.push(self.pat_test(it, &format!("d_get({}, {})", v, i)));
                }
                conj(parts)
            }
            PatKind::Unit { .. } | PatKind::TupleVariant { .. } | PatKind::StructVariant { .. } => {
                let mut parts = Vec::new();
                if let Some(Note::Variant(d, i)) = self.note(pat.span, b'P') {
                    if self.variant_count(d) > 1 {
                        parts.push(format!("d_tag({}) == {}", v, i));
                    }
                }
                match &pat.kind {
                    PatKind::TupleVariant { items, .. } => {
                        for (i, it) in items.iter().enumerate() {
                            parts.push(self.pat_test(it, &format!("d_get({}, {})", v, i)));
                        }
                    }
                    PatKind::StructVariant { fields, .. } => {
                        let names = self.pat_fields(pat);
                        for (f, sub) in fields {
                            if let (Some(sp), Some(idx)) = (sub, names.iter().position(|n| *n == f.name)) {
                                parts.push(self.pat_test(sp, &format!("d_get({}, {})", v, idx)));
                            }
                        }
                    }
                    _ => {}
                }
                conj(parts)
            }
            PatKind::Or(alts) => {
                let parts: Vec<String> = alts.iter().map(|a| self.pat_test(a, v)).collect();
                if parts.iter().any(|p| p == "1") { "1".into() } else { format!("({})", parts.join(" || ")) }
            }
        }
    }

    /// Bind the names of `pat` from borrowed value `v` (retaining what is bound).
    fn pat_bind(&mut self, pat: &Pat, v: &str) {
        match &pat.kind {
            PatKind::Wildcard | PatKind::Lit(_) | PatKind::Unit { .. } | PatKind::Or(_) => {}
            PatKind::Bind(n) => {
                let c = self.declare(n);
                self.emit(&format!("d_release({c}); {c} = d_retain({v});"));
            }
            PatKind::Tuple(items) | PatKind::TupleVariant { items, .. } => {
                for (i, it) in items.iter().enumerate() {
                    self.pat_bind(it, &format!("d_get({}, {})", v, i));
                }
            }
            PatKind::List { items, rest } => {
                for (i, it) in items.iter().enumerate() {
                    self.pat_bind(it, &format!("d_get({}, {})", v, i));
                }
                if let Some(Some(id)) = rest {
                    let c = self.declare(&id.name);
                    self.emit(&format!("d_release({c}); {c} = d_list_rest({v}, {});", items.len()));
                }
            }
            PatKind::StructVariant { fields, .. } => {
                let names = self.pat_fields(pat);
                for (f, sub) in fields {
                    let Some(idx) = names.iter().position(|n| *n == f.name) else { continue };
                    let fv = format!("d_get({}, {})", v, idx);
                    match sub {
                        Some(sp) => self.pat_bind(sp, &fv),
                        None => {
                            let c = self.declare(&f.name);
                            self.emit(&format!("d_release({c}); {c} = d_retain({fv});"));
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ expressions

    /// The C lvalue of a local variable named by `e`, read without taking a reference (borrowed).
    fn borrow_local(&mut self, e: &Expr) -> Option<String> {
        let ExprKind::Name(n) = &e.kind else { return None };
        match self.lookup(n) {
            Some(Loc::Var(c)) => Some(c),
            Some(Loc::Ptr(c)) => Some(format!("(*{})", c)),
            _ => None,
        }
    }

    /// Expressions that cannot change any variable while they are evaluated (no calls).
    fn is_plain(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Name(_) | ExprKind::Int(_) | ExprKind::Bool(_) => true,
            ExprKind::Binary { lhs, rhs, .. } => Self::is_plain(lhs) && Self::is_plain(rhs),
            ExprKind::Unary { expr, .. } => Self::is_plain(expr),
            ExprKind::Field { base, .. } => Self::is_plain(base),
            _ => false,
        }
    }

    fn expr(&mut self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(s) => format!("d_int({})", c_int(int_lit(s))),
            ExprKind::Float(s) => format!("d_float({})", float_lit(s)),
            ExprKind::Bool(b) => format!("d_bool({})", *b as i32),
            ExprKind::Str(parts) => self.string(parts),
            ExprKind::RawStr(s) => self.lit(s.as_bytes(), false),
            ExprKind::Bytes(b) => self.lit(b, true),
            ExprKind::Name(_) | ExprKind::Field { .. } => self.path(e),
            ExprKind::Tuple(items) => {
                if items.is_empty() {
                    return "D_UNIT".into();
                }
                let vs: Vec<String> = items.iter().map(|x| self.expr(x)).collect();
                self.tmp(format!("d_tuple({}, (V[]){{{}}})", vs.len(), vs.join(", ")))
            }
            ExprKind::List(items) => {
                if items.is_empty() {
                    return self.tmp("d_list(0, NULL)");
                }
                let vs: Vec<String> = items.iter().map(|x| self.expr(x)).collect();
                self.tmp(format!("d_list({}, (V[]){{{}}})", vs.len(), vs.join(", ")))
            }
            ExprKind::Map(entries) => {
                if entries.is_empty() {
                    return self.tmp("d_map(0, NULL)");
                }
                let mut vs = Vec::new();
                for (k, v) in entries {
                    vs.push(self.expr(k));
                    vs.push(self.expr(v));
                }
                self.tmp(format!("d_map({}, (V[]){{{}}})", entries.len(), vs.join(", ")))
            }
            ExprKind::StructLit { fields, .. } => {
                let Some(Note::StructLit(d, vi)) = self.note(e.span, b's') else { return self.internal("struct literal") };
                let names = self.field_names(d, vi);
                let mut vals: HashMap<String, String> = HashMap::new();
                for (n, v) in fields {
                    let x = match v {
                        Some(v) => self.expr(v),
                        None => self.expr(&Expr { kind: ExprKind::Name(n.name.clone()), span: n.span }),
                    };
                    vals.insert(n.name.clone(), x);
                }
                let ordered: Vec<String> = names.iter().map(|n| vals.remove(n).unwrap_or_else(|| "D_UNIT".into())).collect();
                let td = self.td(d);
                if ordered.is_empty() {
                    self.tmp(format!("d_adt(&{}, {}, 0, NULL)", td, vi.unwrap_or(0)))
                } else {
                    self.tmp(format!("d_adt(&{}, {}, {}, (V[]){{{}}})", td, vi.unwrap_or(0), ordered.len(), ordered.join(", ")))
                }
            }
            ExprKind::With { base, fields } => {
                let Some(Note::With(d)) = self.note(e.span, b's') else { return self.internal("with") };
                let names = self.field_names(d, None);
                let b = self.expr(base);
                let mut idx = Vec::new();
                let mut vals = Vec::new();
                for (n, v) in fields {
                    idx.push(names.iter().position(|x| *x == n.name).unwrap_or(0).to_string());
                    vals.push(self.expr(v));
                }
                self.tmp(format!("d_with({}, {}, (int[]){{{}}}, (V[]){{{}}})", b, idx.len(), idx.join(", "), vals.join(", ")))
            }
            ExprKind::Index { base, index } => {
                if Self::is_plain(index) {
                    if let Some(b) = self.borrow_local(base) {
                        let i = self.expr(index);
                        return self.tmp(format!("d_index_b({}, {})", b, i));
                    }
                }
                let b = self.expr(base);
                let i = self.expr(index);
                self.tmp(format!("d_index({}, {})", b, i))
            }
            ExprKind::Call { .. } => self.call(e),
            ExprKind::Unary { op: UnOp::Neg, expr } if matches!(expr.kind, ExprKind::Int(_)) => {
                let ExprKind::Int(t) = &expr.kind else { unreachable!() };
                format!("d_int({})", c_int(-int_lit(t)))
            }
            ExprKind::Unary { op, expr } => {
                let v = self.expr(expr);
                match op {
                    UnOp::Not => self.tmp(format!("d_bool(!{}.i)", v)),
                    UnOp::Neg => self.tmp(format!("d_neg({})", v)),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs),
            ExprKind::Range { lo, hi, inclusive } => {
                let a = self.expr(lo);
                let b = self.expr(hi);
                self.tmp(format!("d_range({}, {}, {})", a, b, *inclusive as i32))
            }
            ExprKind::Try(inner) => {
                let conv = self.note(e.span, b't');
                let v = self.expr(inner);
                let t = self.tmp(&v);
                self.emit(&format!("if (d_tag({}) == 1) {{", t));
                let r = match conv {
                    Some(Note::TryToError) => format!("d_err(d_to_error(d_take0({})))", t),
                    Some(Note::TryWrap(d, i)) => format!("d_err(d_adt(&{}, {}, 1, (V[]){{d_take0({})}}))", self.td(d), i, t),
                    _ => t.clone(),
                };
                self.emit(&format!("  ret = {}; goto out;", r));
                self.emit("}");
                self.tmp(format!("d_take0({})", t))
            }
            ExprKind::If { cond, then, els } => {
                let c = self.expr(cond);
                let r = self.fresh("r");
                self.emit(&format!("V {} = D_UNIT;", r));
                self.emit(&format!("if ({}.i) {{", c));
                let a = self.block(then, els.is_some());
                if els.is_some() {
                    self.emit(&format!("{} = {};", r, a));
                }
                self.emit("} else {");
                if let Some(x) = els {
                    let b = self.expr(x);
                    self.emit(&format!("{} = {};", r, b));
                }
                self.emit("}");
                r
            }
            ExprKind::Match { scrutinee, arms } => {
                let s = self.expr(scrutinee);
                let sv = self.hoist("m");
                self.emit(&format!("d_release({sv}); {sv} = {s};"));
                let r = self.fresh("r");
                let end = self.fresh("mend");
                self.emit(&format!("V {} = D_UNIT;", r));
                for arm in arms {
                    let cond = self.pat_test(&arm.pat, &sv);
                    self.emit(&format!("if ({}) {{", cond));
                    self.cur().scopes.push(Vec::new());
                    self.pat_bind(&arm.pat, &sv);
                    if let Some(g) = &arm.guard {
                        let gv = self.expr(g);
                        self.emit(&format!("if ({}.i) {{", gv));
                    }
                    let v = self.expr(&arm.body);
                    self.emit(&format!("{} = {}; goto {};", r, v, end));
                    if arm.guard.is_some() {
                        self.emit("}");
                    }
                    self.cur().scopes.pop();
                    self.emit("}");
                }
                self.emit("d_panic_c(\"internal: no match arm matched\");");
                self.cur().body.push_str(&format!("{}: ;\n", end));
                r
            }
            ExprKind::Closure { params, body, .. } => {
                let id = self.fresh("lam_");
                let m = self.module();
                self.ctxs.push(Ctx::new(m, true));
                for (i, (pat, _)) in params.iter().enumerate() {
                    self.bind_owned(pat, &format!("args[{}]", i));
                }
                let v = self.expr(body);
                self.emit(&format!("ret = {};", v));
                let caps = self.ctxs.last().unwrap().captures.clone();
                self.finish(&format!("static V {}(DFn* fo, V* args)", id));
                let mut env = Vec::new();
                for name in &caps {
                    let l = self.lookup(name).expect("captured name resolves in the enclosing function");
                    env.push(self.read(&l));
                }
                if env.is_empty() {
                    self.tmp(format!("d_fn_new({}, {}, 0, NULL)", id, params.len()))
                } else {
                    self.tmp(format!("d_fn_new({}, {}, {}, (V[]){{{}}})", id, params.len(), env.len(), env.join(", ")))
                }
            }
            ExprKind::Block(b) => self.block(b, true),
            ExprKind::Spawn(x) => {
                // the expression runs in the new task, as the body of a closure without parameters
                let id = self.fresh("task_");
                let m = self.module();
                self.ctxs.push(Ctx::new(m, true));
                let v = self.expr(x);
                self.emit(&format!("ret = {};", v));
                let caps = self.ctxs.last().unwrap().captures.clone();
                self.finish(&format!("static V {}(DFn* fo, V* args)", id));
                let mut env = Vec::new();
                for name in &caps {
                    let l = self.lookup(name).expect("captured name resolves in the enclosing function");
                    env.push(self.read(&l));
                }
                let f = if env.is_empty() {
                    format!("d_fn_new({}, 0, 0, NULL)", id)
                } else {
                    format!("d_fn_new({}, 0, {}, (V[]){{{}}})", id, env.len(), env.join(", "))
                };
                self.tmp(format!("d_spawn({})", f))
            }
            ExprKind::Return(v) => {
                let r = match v {
                    Some(x) => self.expr(x),
                    None => "D_UNIT".into(),
                };
                self.emit(&format!("ret = {}; goto out;", r));
                "D_UNIT".into()
            }
            ExprKind::Break => {
                self.emit("break;");
                "D_UNIT".into()
            }
            ExprKind::Continue => {
                self.emit("continue;");
                "D_UNIT".into()
            }
            ExprKind::Todo => {
                self.at(e.span);
                self.emit("d_panic_c(\"reached a `todo` hole\");");
                "D_UNIT".into()
            }
        }
    }

    fn internal(&mut self, what: &str) -> String {
        self.emit(&format!("d_panic_c({});", c_str(format!("internal: unresolved {}", what).as_bytes())));
        "D_UNIT".into()
    }

    fn string(&mut self, parts: &[StrPart]) -> String {
        match parts {
            [] => self.lit(b"", false),
            [StrPart::Lit(s)] => self.lit(s.as_bytes(), false),
            _ => {
                let b = self.fresh("b");
                self.emit(&format!("DBuf {}; d_buf_init(&{});", b, b));
                for p in parts {
                    match p {
                        StrPart::Lit(s) => {
                            if !s.is_empty() {
                                self.emit(&format!("d_buf_add(&{}, {}, {});", b, c_str(s.as_bytes()), s.len()));
                            }
                        }
                        StrPart::Expr(x) => {
                            let v = self.expr(x);
                            self.emit(&format!("d_buf_show(&{}, {}, 1);", b, v));
                        }
                    }
                }
                self.tmp(format!("d_buf_done(&{})", b))
            }
        }
    }

    fn path(&mut self, e: &Expr) -> String {
        match self.note(e.span, b'p') {
            Some(Note::Fn(fid)) => self.fn_value(fid),
            Some(Note::Const(c)) => self.tmp(format!("c_{}()", c)),
            Some(Note::Variant(d, i)) => {
                let n = match &self.p.defs[d].kind {
                    DefKind::Enum(vs) => match &vs[i].1 {
                        VFields::Tuple(ts) => ts.len(),
                        _ => 0,
                    },
                    _ => 0,
                };
                if n == 0 {
                    let td = self.td(d);
                    self.tmp(format!("d_adt(&{}, {}, 0, NULL)", td, i))
                } else {
                    self.ctor_wrappers.insert((d, i));
                    self.tmp(format!("d_fn_new(ctw_{}_{}, {}, 0, NULL)", d, i, n))
                }
            }
            Some(Note::Field(i)) => {
                let ExprKind::Field { base, .. } = &e.kind else { return self.internal("field") };
                let b = self.expr(base);
                self.tmp(format!("d_field({}, {})", b, i))
            }
            _ => match &e.kind {
                ExprKind::Name(n) => match self.lookup(n) {
                    Some(l) if self.scalar(e) != 0 => self.read_scalar(&l),
                    Some(l) => self.read(&l),
                    None => self.internal(&format!("name `{}`", n)),
                },
                _ => self.internal("path"),
            },
        }
    }

    fn fn_value(&mut self, fid: usize) -> String {
        self.fn_wrappers.insert(fid);
        let n = self.p.fns[fid].params.len();
        self.tmp(format!("d_fn_new(fnw_{}, {}, 0, NULL)", fid, n))
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> String {
        if matches!(op, BinOp::And | BinOp::Or) {
            let a = self.expr(lhs);
            let r = self.fresh("r");
            self.emit(&format!("V {} = {};", r, a));
            self.emit(&format!("if ({}{}.i) {{", if op == BinOp::Or { "!" } else { "" }, r));
            let b = self.expr(rhs);
            self.emit(&format!("{} = {};", r, b));
            self.emit("}");
            return r;
        }
        let a = self.expr(lhs);
        let b = self.expr(rhs);
        let k = self.scalar(lhs);
        if k != 0 && k == self.scalar(rhs) {
            // both operands are statically Int, Bool or Float: no kind dispatch, nothing to release
            let fast = match (k, op) {
                (1, BinOp::Add) => Some(format!("d_addi({}, {})", a, b)),
                (1, BinOp::Sub) => Some(format!("d_subi({}, {})", a, b)),
                (1, BinOp::Mul) => Some(format!("d_muli({}, {})", a, b)),
                (1, BinOp::Div) => Some(format!("d_divi({}, {})", a, b)),
                (1, BinOp::Rem) => Some(format!("d_remi({}, {})", a, b)),
                (1 | 2, BinOp::Eq) => Some(format!("d_bool({}.i == {}.i)", a, b)),
                (1 | 2, BinOp::Ne) => Some(format!("d_bool({}.i != {}.i)", a, b)),
                (1, BinOp::Lt) => Some(format!("d_bool({}.i < {}.i)", a, b)),
                (1, BinOp::Le) => Some(format!("d_bool({}.i <= {}.i)", a, b)),
                (1, BinOp::Gt) => Some(format!("d_bool({}.i > {}.i)", a, b)),
                (1, BinOp::Ge) => Some(format!("d_bool({}.i >= {}.i)", a, b)),
                (3, BinOp::Add) => Some(format!("d_float({}.f + {}.f)", a, b)),
                (3, BinOp::Sub) => Some(format!("d_float({}.f - {}.f)", a, b)),
                (3, BinOp::Mul) => Some(format!("d_float({}.f * {}.f)", a, b)),
                (3, BinOp::Div) => Some(format!("d_float({}.f / {}.f)", a, b)),
                _ => None,
            };
            if let Some(code) = fast {
                return self.tmp(code);
            }
        }
        let f = match op {
            BinOp::Add => "d_add",
            BinOp::Sub => "d_sub",
            BinOp::Mul => "d_mul",
            BinOp::Div => "d_div",
            BinOp::Rem => "d_rem",
            BinOp::BitAnd => "d_band",
            BinOp::BitOr => "d_bor",
            BinOp::BitXor => "d_bxor",
            BinOp::Shl => "d_shl",
            BinOp::Shr => "d_shr",
            BinOp::Eq => "d_op_eq",
            BinOp::Ne => "d_op_ne",
            BinOp::Lt => "d_op_lt",
            BinOp::Le => "d_op_le",
            BinOp::Gt => "d_op_gt",
            BinOp::Ge => "d_op_ge",
            BinOp::And | BinOp::Or => unreachable!(),
        };
        self.tmp(format!("{}({}, {})", f, a, b))
    }

    fn call(&mut self, e: &Expr) -> String {
        let ExprKind::Call { callee, args } = &e.kind else { unreachable!() };
        match self.note(e.span, b'c') {
            Some(Note::Method(fid)) => {
                let field = match &callee.kind {
                    ExprKind::Index { base, .. } => base,
                    _ => callee,
                };
                let ExprKind::Field { base, .. } = &field.kind else { return self.internal("method") };
                let var_self = self.p.fns[fid].params.first().map_or(false, |p| p.mutable);
                // `xs.len()` / `xs.is_empty()` on a local: no reference taken, no call
                let fname = self.p.fns[fid].name.clone();
                if args.is_empty() && !var_self && self.is_std(self.p.fns[fid].module) && (fname == "len" || fname == "is_empty") {
                    let recv = matches!(self.p.fns[fid].params.first().map(|p| &p.ty), Some(Ty::Named(d, _)) if matches!(self.p.defs[*d].name.as_str(), "List" | "Bytes" | "Map" | "Set" | "Str"));
                    if recv {
                        if let Some(b) = self.borrow_local(base) {
                            return if fname == "len" { format!("d_int(d_len_b({}))", b) } else { format!("d_bool(d_len_b({}) == 0)", b) };
                        }
                    }
                }
                let mut vs = Vec::new();
                if var_self {
                    let rest: Vec<String> = args.iter().map(|a| self.expr(a)).collect();
                    vs.push(self.lvalue(base, false));
                    vs.extend(rest);
                } else {
                    vs.push(self.expr(base));
                    vs.extend(args.iter().map(|a| self.expr(a)));
                }
                if let Some(Note::TypeArg(t)) = self.note(e.span, b'g') {
                    vs.push(format!("&{}", self.ty_desc(&t)));
                }
                let name = self.fn_cname(fid);
                return self.call_tmp(format!("{}({})", name, vs.join(", ")));
            }
            Some(Note::FieldFn(i)) => {
                let ExprKind::Field { base, .. } = &callee.kind else { return self.internal("field call") };
                let b = self.expr(base);
                let f = self.tmp(format!("d_field({}, {})", b, i));
                return self.call_value(&f, args);
            }
            _ => {}
        }
        let target: &Expr = match &callee.kind {
            ExprKind::Index { base, .. } if matches!(self.note(base.span, b'p'), Some(Note::Fn(_))) => base,
            _ => callee,
        };
        match self.note(target.span, b'p') {
            Some(Note::Fn(fid)) => {
                let mut vs: Vec<String> = args.iter().map(|a| self.expr(a)).collect();
                if let Some(Note::TypeArg(t)) = self.note(e.span, b'g') {
                    vs.push(format!("&{}", self.ty_desc(&t)));
                }
                let name = self.fn_cname(fid);
                self.call_tmp(format!("{}({})", name, vs.join(", ")))
            }
            Some(Note::Variant(d, i)) => {
                let vs: Vec<String> = args.iter().map(|a| self.expr(a)).collect();
                let td = self.td(d);
                self.tmp(format!("d_adt(&{}, {}, {}, (V[]){{{}}})", td, i, vs.len(), vs.join(", ")))
            }
            _ => {
                let f = self.expr(callee);
                self.call_value(&f, args)
            }
        }
    }

    fn call_value(&mut self, f: &str, args: &[Expr]) -> String {
        let vs: Vec<String> = args.iter().map(|a| self.expr(a)).collect();
        if vs.is_empty() {
            self.call_tmp(format!("d_call({}, 0, NULL)", f))
        } else {
            self.call_tmp(format!("d_call({}, {}, (V[]){{{}}})", f, vs.len(), vs.join(", ")))
        }
    }

    // ------------------------------------------------------------ output

    fn assemble(mut self, main_code: String) -> String {
        let mut wrappers = String::new();
        for fid in self.fn_wrappers.clone() {
            let name = self.fn_cname(fid);
            let n = self.p.fns[fid].params.len();
            let args: Vec<String> = (0..n).map(|i| format!("args[{}]", i)).collect();
            wrappers.push_str(&format!("static V fnw_{}(DFn* fo, V* args) {{ (void)fo; (void)args; return {}({}); }}\n", fid, name, args.join(", ")));
            self.protos.push_str(&format!("static V fnw_{}(DFn* fo, V* args);\n", fid));
        }
        for (d, i) in self.ctor_wrappers.clone() {
            let n = match &self.p.defs[d].kind {
                DefKind::Enum(vs) => match &vs[i].1 {
                    VFields::Tuple(ts) => ts.len(),
                    _ => 0,
                },
                _ => 0,
            };
            wrappers.push_str(&format!("static V ctw_{d}_{i}(DFn* fo, V* args) {{ (void)fo; return d_adt(&{}, {i}, {n}, args); }}\n", self.td(d)));
            self.protos.push_str(&format!("static V ctw_{}_{}(DFn* fo, V* args);\n", d, i));
        }
        let mut out = String::new();
        out.push_str("/* generated by drift; do not edit */\n#include \"drift_rt.h\"\n\n");
        for (m, (name, _, is_std)) in self.p.files.iter().enumerate() {
            if !is_std {
                out.push_str(&format!("static const char* const F{} = {};\n", m, c_str(name.as_bytes())));
            }
        }
        out.push('\n');
        // user type descriptors
        for (d, def) in self.p.defs.iter().enumerate() {
            if self.p.mods[def.module].is_std {
                continue;
            }
            let (variants, is_enum): (Vec<(Option<String>, Vec<String>, bool)>, bool) = match &def.kind {
                DefKind::Struct(fs) => (vec![(None, fs.iter().map(|(n, _)| n.clone()).collect(), true)], false),
                DefKind::Enum(vs) => (
                    vs.iter()
                        .map(|(n, f)| match f {
                            VFields::Unit => (Some(n.clone()), vec![], false),
                            VFields::Tuple(ts) => (Some(n.clone()), (0..ts.len()).map(|i| i.to_string()).collect(), false),
                            VFields::Named(fs) => (Some(n.clone()), fs.iter().map(|(x, _)| x.clone()).collect(), true),
                        })
                        .collect(),
                    true,
                ),
                _ => continue,
            };
            let mut vtext = Vec::new();
            for (vi, (vname, fields, named)) in variants.iter().enumerate() {
                let farr = if *named {
                    let arr = format!("td{}_f{}", d, vi);
                    let names: Vec<String> = fields.iter().map(|f| c_str(f.as_bytes())).collect();
                    out.push_str(&format!("static const char* const {}[] = {{{}}};\n", arr, if names.is_empty() { "0".into() } else { names.join(", ") }));
                    arr
                } else {
                    "NULL".into()
                };
                let vn = vname.as_ref().map(|n| c_str(n.as_bytes())).unwrap_or_else(|| "NULL".into());
                vtext.push(format!("{{{}, {}, {}}}", vn, fields.len(), farr));
            }
            out.push_str(&format!("static const DVariant td{}_v[] = {{{}}};\n", d, vtext.join(", ")));
            out.push_str(&format!(
                "static DType td_{} = {{{}, {}, td{}_v, {}, 0, NULL}};\n",
                d,
                c_str(def.name.as_bytes()),
                variants.len(),
                d,
                is_enum as i32
            ));
        }
        out.push('\n');
        out.push_str(&self.ty_fwd);
        out.push_str(&self.ty_out);
        out.push('\n');
        for i in 0..self.lits.len() {
            out.push_str(&format!("V K{};\n", i));
        }
        // natives: prototypes for the runtime's, stubs for the missing ones
        for fid in self.natives.clone() {
            let name = self.native_name(fid);
            if self.implemented.contains(&name) {
                out.push_str(&format!("V {}({});\n", name, self.fn_sig_params(fid, false)));
            } else {
                let f = &self.p.fns[fid];
                let what = format!("{}.{}", self.p.mods[f.module].path.trim_start_matches("std."), f.name);
                let params = self.fn_sig_params(fid, true);
                let mut uses: String = (0..f.params.len()).map(|i| format!("(void){}{}; ", if f.params[i].mutable { "p" } else { "a" }, i)).collect();
                if self.is_reified(fid) {
                    uses.push_str("(void)ty; ");
                }
                out.push_str(&format!("static V {}({}) {{ {}d_unimpl({}); }}\n", name, params, uses, c_str(what.as_bytes())));
            }
        }
        out.push('\n');
        out.push_str(&self.protos);
        out.push('\n');
        out.push_str(&self.funcs);
        out.push_str(&wrappers);
        out.push_str("\nstatic void d_init_lits(void) {\n");
        for (i, (bytes, is_bytes)) in self.lits.iter().enumerate() {
            out.push_str(&format!(
                "  K{} = {}({}, {});\n",
                i,
                if *is_bytes { "d_bytes_const" } else { "d_str_const" },
                c_str(bytes),
                bytes.len()
            ));
        }
        out.push_str("}\n\n");
        out.push_str(&main_code);
        out
    }
}
