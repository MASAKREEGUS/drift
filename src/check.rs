//! Semantic analysis: name resolution, types with local inference, effects, exhaustiveness,
//! mutability, typed holes. Diagnostics carry machine-applicable fixes whenever the repair is certain.

use crate::ast::*;
use crate::diag::{Diag, Severity, Span};
use std::collections::{HashMap, HashSet};

pub const STD_MODULES: &[(&str, &str)] = &[
    ("core", include_str!("std/core.dr")),
    ("io", include_str!("std/io.dr")),
    ("fs", include_str!("std/fs.dr")),
    ("env", include_str!("std/env.dr")),
    ("bytes", include_str!("std/bytes.dr")),
    ("http", include_str!("std/http.dr")),
    ("html", include_str!("std/html.dr")),
    ("json", include_str!("std/json.dr")),
    ("time", include_str!("std/time.dr")),
    ("rand", include_str!("std/rand.dr")),
];

pub const EFFECTS: [&str; 9] = ["io", "fs", "net", "db", "env", "time", "rand", "proc", "spawn"];

pub struct SourceFile {
    pub name: String,
    pub src: String,
    /// Dotted module path: `std.http`, `shop.offers`, `main`.
    pub module: String,
    pub is_std: bool,
}

pub fn std_sources() -> Vec<SourceFile> {
    STD_MODULES
        .iter()
        .map(|(n, s)| SourceFile { name: format!("<std>/{}.dr", n), src: s.to_string(), module: format!("std.{}", n), is_std: true })
        .collect()
}

/// Type-check `asts[i]` (parsed from `files[i]`). Std modules must be included.
pub fn check(files: &[SourceFile], asts: Vec<Module>) -> Vec<(usize, Diag)> {
    let mut c = Checker::new(files, asts);
    c.run();
    let mut out = c.diags;
    out.sort_by_key(|(f, d)| (*f, d.span.lo, d.span.hi));
    out
}

/// What the checker learned about an expression or pattern, for the code generator.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Note {
    Fn(usize),
    Const(usize),
    Variant(usize, usize),
    Struct(usize),
    Field(usize),
    Method(usize),
    FieldFn(usize),
    StructLit(usize, Option<usize>),
    With(usize),
    TrySame,
    TryToError,
    TryWrap(usize, usize),
    /// Concrete type a reified generic (`json.decode[T]`) is called at; kind b'g' on the call.
    TypeArg(Ty),
}

/// Note keys: (module, span.lo, span.hi, kind) with kind b'p' path/field, b'c' call, b't' `?`, b's' literal, b'P' pattern.
pub(crate) type NoteKey = (usize, u32, u32, u8);

/// A fully checked program: everything the back end needs.
pub struct Program {
    pub(crate) files: Vec<(String, String, bool)>,
    pub(crate) asts: Vec<Module>,
    pub(crate) mods: Vec<ModInfo>,
    pub(crate) defs: Vec<TypeDef>,
    pub(crate) fns: Vec<FnInfo>,
    pub(crate) consts: Vec<ConstInfo>,
    pub(crate) notes: HashMap<NoteKey, Note>,
    pub(crate) core: Core,
}

/// Type-check and keep the tables for code generation.
pub fn check_program(files: &[SourceFile], asts: Vec<Module>) -> (Vec<(usize, Diag)>, Program) {
    let mut c = Checker::new(files, asts);
    c.run();
    let mut out = std::mem::take(&mut c.diags);
    out.sort_by_key(|(f, d)| (*f, d.span.lo, d.span.hi));
    let prog = Program {
        files: files.iter().map(|f| (f.name.clone(), f.src.clone(), f.is_std)).collect(),
        asts: std::mem::take(&mut c.asts),
        mods: std::mem::take(&mut c.mods),
        defs: std::mem::take(&mut c.defs),
        fns: std::mem::take(&mut c.fns),
        consts: std::mem::take(&mut c.consts),
        notes: std::mem::take(&mut c.notes),
        core: c.core,
    };
    (out, prog)
}

// ---------------------------------------------------------------- types

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Owner {
    Def(usize),
    Fn(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ty {
    Named(usize, Vec<Ty>),
    Tuple(Vec<Ty>),
    /// params, return, effects, effect-polymorphic (declared without `uses`)
    Fn(Vec<Ty>, Box<Ty>, u16, bool),
    Gen(Owner, usize),
    Var(usize),
    Never,
    Err,
}

fn unit() -> Ty {
    Ty::Tuple(vec![])
}

pub(crate) fn subst(t: &Ty, owner: Owner, args: &[Ty]) -> Ty {
    match t {
        Ty::Gen(o, i) if *o == owner => args.get(*i).cloned().unwrap_or(Ty::Err),
        Ty::Named(d, a) => Ty::Named(*d, a.iter().map(|x| subst(x, owner, args)).collect()),
        Ty::Tuple(v) => Ty::Tuple(v.iter().map(|x| subst(x, owner, args)).collect()),
        Ty::Fn(p, r, e, poly) => Ty::Fn(p.iter().map(|x| subst(x, owner, args)).collect(), Box::new(subst(r, owner, args)), *e, *poly),
        other => other.clone(),
    }
}

#[derive(Clone, Debug)]
pub(crate) enum VFields {
    Unit,
    Tuple(Vec<Ty>),
    Named(Vec<(String, Ty)>),
}

#[derive(Clone, Debug)]
pub(crate) enum DefKind {
    Struct(Vec<(String, Ty)>),
    Enum(Vec<(String, VFields)>),
    Alias(Ty),
    Opaque,
    Pending,
}

#[derive(Clone, Debug)]
pub(crate) struct TypeDef {
    pub(crate) name: String,
    pub(crate) module: usize,
    pub(crate) generics: Vec<String>,
    pub(crate) kind: DefKind,
    pub(crate) public: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Param {
    pub(crate) name: String,
    pub(crate) ty: Ty,
    pub(crate) mutable: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct FnInfo {
    pub(crate) name: String,
    pub(crate) module: usize,
    pub(crate) item: usize,
    pub(crate) generics: Vec<String>,
    pub(crate) params: Vec<Param>,
    pub(crate) ret: Ty,
    pub(crate) effects: u16,
    pub(crate) is_method: bool,
    pub(crate) owner: Option<usize>,
    pub(crate) public: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ConstInfo {
    pub(crate) module: usize,
    pub(crate) item: usize,
    pub(crate) ty: Ty,
    pub(crate) public: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Decl {
    Type(usize),
    Fn(usize),
    Const(usize),
    Module(usize),
    Variant(usize, usize),
}

pub(crate) struct ModInfo {
    pub(crate) path: String,
    pub(crate) file: usize,
    pub(crate) is_std: bool,
    pub(crate) names: HashMap<String, Decl>,
    pub(crate) imports: HashMap<String, Decl>,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Core {
    pub(crate) int: usize,
    pub(crate) i32: usize,
    pub(crate) u8: usize,
    pub(crate) u32: usize,
    pub(crate) u64: usize,
    pub(crate) float: usize,
    pub(crate) f32: usize,
    pub(crate) bool: usize,
    pub(crate) str: usize,
    pub(crate) bytes: usize,
    pub(crate) never: usize,
    pub(crate) list: usize,
    pub(crate) map: usize,
    pub(crate) set: usize,
    pub(crate) range: usize,
    pub(crate) task: usize,
    pub(crate) option: usize,
    pub(crate) result: usize,
    pub(crate) error: usize,
}

#[derive(Clone, Debug)]
enum LocalKind {
    Let { kw: Span },
    Param { is_self: bool },
    Pattern,
}

#[derive(Clone, Debug)]
struct Local {
    name: String,
    ty: Ty,
    mutable: bool,
    span: Span,
    kind: LocalKind,
    mutated: bool,
    depth: u32,
}

enum PathRes {
    Local(usize),
    Fn(usize),
    Const(usize),
    Type(usize),
    Module(usize),
    Variant(usize, usize),
    Error,
}

// ------------------------------------------------- exhaustiveness patterns

#[derive(Clone, Debug, PartialEq)]
enum Ctor {
    Tuple(usize),
    Struct(usize),
    Variant(usize, usize),
    True,
    False,
    Nil,
    Cons,
}

#[derive(Clone, Debug)]
enum P {
    Wild,
    Lit(String),
    Ctor(Ctor, Vec<P>),
    Or(Vec<P>),
}

const METHOD_ALIASES: &[(&str, &str)] = &[
    ("length", "len"),
    ("size", "len"),
    ("push_back", "push"),
    ("append", "push"),
    ("lower", "to_lower"),
    ("lowercase", "to_lower"),
    ("to_lowercase", "to_lower"),
    ("toLowerCase", "to_lower"),
    ("upper", "to_upper"),
    ("uppercase", "to_upper"),
    ("to_uppercase", "to_upper"),
    ("toUpperCase", "to_upper"),
    ("strip", "trim"),
    ("startswith", "starts_with"),
    ("startsWith", "starts_with"),
    ("endswith", "ends_with"),
    ("endsWith", "ends_with"),
    ("includes", "contains"),
    ("has", "contains"),
    ("indexOf", "index_of"),
    ("index", "index_of"),
    ("reduce", "fold"),
    ("sort", "sorted"),
    ("sort_by", "sorted_by"),
    ("sort_by_key", "sorted_by"),
    ("reverse", "reversed"),
    ("items", "entries"),
    ("iter", "(not needed: List methods work directly)"),
    ("into_iter", "(not needed: List methods work directly)"),
    ("collect", "(not needed: map/filter already return a List)"),
    ("to_string", "(use interpolation: \"{x}\")"),
    ("toString", "(use interpolation: \"{x}\")"),
    ("splitlines", "lines"),
    ("isEmpty", "is_empty"),
    ("is_digit", "is_numeric"),
    ("unwrap_or_default", "unwrap_or"),
];

const TYPE_ALIASES: &[(&str, &str)] = &[
    ("String", "Str"),
    ("str", "Str"),
    ("string", "Str"),
    ("Vec", "List"),
    ("Array", "List"),
    ("list", "List"),
    ("HashMap", "Map"),
    ("BTreeMap", "Map"),
    ("Dict", "Map"),
    ("dict", "Map"),
    ("HashSet", "Set"),
    ("bool", "Bool"),
    ("boolean", "Bool"),
    ("int", "Int"),
    ("i64", "Int"),
    ("isize", "Int"),
    ("usize", "Int"),
    ("Integer", "Int"),
    ("number", "Int"),
    ("i32", "I32"),
    ("u8", "U8"),
    ("u32", "U32"),
    ("u64", "U64"),
    ("f64", "Float"),
    ("float", "Float"),
    ("double", "Float"),
    ("f32", "F32"),
    ("Optional", "Option"),
    ("Maybe", "Option"),
    ("Exception", "Error"),
];

const VALUE_ALIASES: &[(&str, &str)] = &[
    ("null", "None"),
    ("nil", "None"),
    ("undefined", "None"),
    ("True", "true"),
    ("False", "false"),
    ("print", "io.println"),
    ("println", "io.println"),
    ("eprintln", "io.eprintln"),
];

/// Edit distance where swapping two adjacent characters costs 1 (optimal string alignment).
fn lev(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m]
}

/// Closest candidate within an edit distance proportional to the name length.
/// `certain` (safe to apply automatically) for distance <= 1, case-only differences and unique substrings.
fn suggest<'a>(name: &str, cands: impl IntoIterator<Item = &'a str>) -> Option<(String, bool)> {
    let limit = (name.chars().count() / 3).max(1);
    let lower = name.to_lowercase();
    let mut best: Option<(usize, &str)> = None;
    let mut cands_vec: Vec<&str> = cands.into_iter().collect();
    cands_vec.sort();
    cands_vec.dedup();
    for c in cands_vec.iter().copied() {
        if c == name {
            continue;
        }
        let d = if c.to_lowercase() == lower { 0 } else { lev(name, c) };
        if d <= limit && best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    if let Some((d, c)) = best {
        return Some((c.to_string(), d <= 1));
    }
    // `u32_le` → `read_u32_le`: a unique candidate containing the name (or contained in it)
    if name.len() >= 3 {
        let hits: Vec<&str> = cands_vec.iter().copied().filter(|c| c.contains(name) || (c.len() >= 3 && name.contains(c))).collect();
        if hits.len() == 1 {
            return Some((hits[0].to_string(), true));
        }
    }
    None
}

/// Attach a suggestion: as a fix when certain, otherwise as a hint in the message.
fn with_suggestion(mut d: Diag, span: Span, s: Option<(String, bool)>) -> Diag {
    match s {
        Some((name, true)) => d.with_fix(span, name),
        Some((name, false)) => {
            d.message.push_str(&format!("; did you mean `{}`?", name));
            d
        }
        None => d,
    }
}

fn is_snake(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
fn is_pascal(s: &str) -> bool {
    s.chars().next().map_or(false, |c| c.is_ascii_uppercase()) && !s.contains('_')
}
fn is_upper(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}
fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
fn to_pascal(s: &str) -> String {
    s.split('_').filter(|p| !p.is_empty()).map(|p| {
        let mut c = p.chars();
        match c.next() {
            Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
            None => String::new(),
        }
    }).collect()
}

fn effect_list(bits: u16) -> Vec<&'static str> {
    EFFECTS.iter().enumerate().filter(|(i, _)| bits & (1 << i) != 0).map(|(_, n)| *n).collect()
}

// ---------------------------------------------------------------- checker

struct Checker<'f> {
    files: &'f [SourceFile],
    asts: Vec<Module>,
    mods: Vec<ModInfo>,
    defs: Vec<TypeDef>,
    fns: Vec<FnInfo>,
    consts: Vec<ConstInfo>,
    methods: HashMap<(usize, String), usize>,
    assoc: HashMap<(usize, String), usize>,
    method_names: HashSet<String>,
    std_by_name: HashMap<String, usize>,
    core_mod: usize,
    core: Core,
    diags: Vec<(usize, Diag)>,
    // ---- per-body state
    cur: usize,
    vars: Vec<Option<Ty>>,
    locals: Vec<Local>,
    scopes: Vec<usize>,
    rets: Vec<Ty>,
    effects: u16,
    reported_effects: u16,
    effect_insert: Option<(u32, bool)>,
    effect_owner: String,
    closure_depth: u32,
    loop_depth: u32,
    gens: Vec<(String, Ty)>,
    holes: Vec<(Span, Ty, Vec<(String, Ty)>)>,
    infer_lets: Vec<(String, Span, Ty)>,
    reified: Vec<(Span, Ty)>,
    notes: HashMap<NoteKey, Note>,
}

impl<'f> Checker<'f> {
    fn new(files: &'f [SourceFile], asts: Vec<Module>) -> Checker<'f> {
        let mods = files
            .iter()
            .enumerate()
            .map(|(i, f)| ModInfo { path: f.module.clone(), file: i, is_std: f.is_std, names: HashMap::new(), imports: HashMap::new() })
            .collect();
        Checker {
            files,
            asts,
            mods,
            defs: Vec::new(),
            fns: Vec::new(),
            consts: Vec::new(),
            methods: HashMap::new(),
            assoc: HashMap::new(),
            method_names: HashSet::new(),
            std_by_name: HashMap::new(),
            core_mod: 0,
            core: Core::default(),
            diags: Vec::new(),
            cur: 0,
            vars: Vec::new(),
            locals: Vec::new(),
            scopes: Vec::new(),
            rets: Vec::new(),
            effects: 0,
            reported_effects: 0,
            effect_insert: None,
            effect_owner: String::new(),
            closure_depth: 0,
            loop_depth: 0,
            gens: Vec::new(),
            holes: Vec::new(),
            infer_lets: Vec::new(),
            reified: Vec::new(),
            notes: HashMap::new(),
        }
    }

    fn note(&mut self, span: Span, kind: u8, n: Note) {
        self.notes.insert((self.cur, span.lo, span.hi, kind), n);
    }

    fn emit(&mut self, d: Diag) {
        let file = self.mods[self.cur].file;
        self.diags.push((file, d));
    }

    fn text(&self, span: Span) -> String {
        let src = &self.files[self.mods[self.cur].file].src;
        src.get(span.lo as usize..span.hi as usize).unwrap_or("").to_string()
    }

    // ============================================================ collection

    fn run(&mut self) {
        for (i, m) in self.mods.iter().enumerate() {
            if m.is_std {
                let short = m.path.trim_start_matches("std.").to_string();
                if short == "core" {
                    self.core_mod = i;
                }
                self.std_by_name.insert(short, i);
            }
        }
        for m in 0..self.mods.len() {
            self.collect_names(m);
        }
        self.fill_core();
        for m in 0..self.mods.len() {
            self.resolve_imports(m);
        }
        // aliases first, then structs/enums, then signatures and consts
        for pass in 0..2 {
            for d in 0..self.defs.len() {
                self.lower_def(d, pass == 0);
            }
        }
        for f in 0..self.fns.len() {
            self.lower_sig(f);
        }
        for c in 0..self.consts.len() {
            let (m, i) = (self.consts[c].module, self.consts[c].item);
            self.cur = m;
            if let ItemKind::Const { ty, .. } = self.asts[m].items[i].kind.clone() {
                self.gens.clear();
                self.consts[c].ty = self.lower_ty(&ty);
            }
        }
        for m in 0..self.mods.len() {
            if !self.mods[m].is_std {
                self.check_module(m);
            }
        }
    }

    fn naming(&mut self, name: &Ident, what: &str, ok: fn(&str) -> bool, fix: fn(&str) -> String, style: &str) {
        if !ok(&name.name) {
            let good = fix(&name.name);
            self.emit(Diag::error("E0701", name.span, format!("{} `{}` must be {}: `{}`", what, name.name, style, good)).expected(good));
        }
    }

    fn declare_item(&mut self, m: usize, name: &Ident, decl: Decl) {
        if self.mods[m].names.contains_key(&name.name) {
            self.cur = m;
            self.emit(Diag::error("E0901", name.span, format!("`{}` is defined twice in this module", name.name)));
            return;
        }
        self.mods[m].names.insert(name.name.clone(), decl);
    }

    fn collect_names(&mut self, m: usize) {
        self.cur = m;
        let is_std = self.mods[m].is_std;
        let items = self.asts[m].items.clone();
        for (i, it) in items.iter().enumerate() {
            match &it.kind {
                ItemKind::Struct { name, generics, fields } => {
                    self.naming(name, "type", is_pascal, to_pascal, "PascalCase");
                    for f in fields {
                        self.naming(&f.name, "field", is_snake, to_snake, "snake_case");
                    }
                    let kind = if is_std && fields.is_empty() { DefKind::Opaque } else { DefKind::Pending };
                    let id = self.defs.len();
                    self.defs.push(TypeDef { name: name.name.clone(), module: m, generics: generics.iter().map(|g| g.name.clone()).collect(), kind, public: it.public });
                    self.declare_item(m, name, Decl::Type(id));
                }
                ItemKind::Enum { name, generics, variants } => {
                    self.naming(name, "type", is_pascal, to_pascal, "PascalCase");
                    for v in variants {
                        self.naming(&v.name, "variant", is_pascal, to_pascal, "PascalCase");
                    }
                    let id = self.defs.len();
                    self.defs.push(TypeDef { name: name.name.clone(), module: m, generics: generics.iter().map(|g| g.name.clone()).collect(), kind: DefKind::Pending, public: it.public });
                    self.declare_item(m, name, Decl::Type(id));
                }
                ItemKind::TypeAlias { name, generics, .. } => {
                    self.naming(name, "type", is_pascal, to_pascal, "PascalCase");
                    let id = self.defs.len();
                    self.defs.push(TypeDef { name: name.name.clone(), module: m, generics: generics.iter().map(|g| g.name.clone()).collect(), kind: DefKind::Pending, public: it.public });
                    self.declare_item(m, name, Decl::Type(id));
                }
                ItemKind::Const { name, .. } => {
                    self.naming(name, "constant", is_upper, |s| to_snake(s).to_uppercase(), "UPPER_CASE");
                    let id = self.consts.len();
                    self.consts.push(ConstInfo { module: m, item: i, ty: Ty::Err, public: it.public });
                    self.declare_item(m, name, Decl::Const(id));
                }
                ItemKind::Fn(f) => {
                    self.naming(&f.name, "function", is_snake, to_snake, "snake_case");
                    let is_method = f.params.first().map_or(false, |p| p.name.name == "self");
                    let id = self.fns.len();
                    self.fns.push(FnInfo {
                        name: f.name.name.clone(),
                        module: m,
                        item: i,
                        generics: f.generics.iter().map(|g| g.name.clone()).collect(),
                        params: Vec::new(),
                        ret: Ty::Err,
                        effects: 0,
                        is_method,
                        owner: None,
                        public: it.public,
                    });
                    if is_method {
                        self.method_names.insert(f.name.name.clone());
                        if f.owner.is_some() {
                            self.emit(Diag::error("E0902", f.name.span, "an associated function `Type.name` cannot take `self`"));
                        }
                    } else if f.owner.is_none() {
                        self.declare_item(m, &f.name, Decl::Fn(id));
                    }
                }
                ItemKind::Import { .. } | ItemKind::Examples { .. } | ItemKind::Test { .. } => {}
            }
        }
    }

    fn fill_core(&mut self) {
        let cm = self.core_mod;
        let get = |c: &Checker, n: &str| match c.mods[cm].names.get(n) {
            Some(Decl::Type(d)) => *d,
            _ => usize::MAX,
        };
        self.core = Core {
            int: get(self, "Int"),
            i32: get(self, "I32"),
            u8: get(self, "U8"),
            u32: get(self, "U32"),
            u64: get(self, "U64"),
            float: get(self, "Float"),
            f32: get(self, "F32"),
            bool: get(self, "Bool"),
            str: get(self, "Str"),
            bytes: get(self, "Bytes"),
            never: get(self, "Never"),
            list: get(self, "List"),
            map: get(self, "Map"),
            set: get(self, "Set"),
            range: get(self, "Range"),
            task: get(self, "Task"),
            option: get(self, "Option"),
            result: get(self, "Result"),
            error: get(self, "Error"),
        };
        let (o, r) = (self.core.option, self.core.result);
        for (n, d) in [("Some", Decl::Variant(o, 0)), ("None", Decl::Variant(o, 1)), ("Ok", Decl::Variant(r, 0)), ("Err", Decl::Variant(r, 1))] {
            self.mods[cm].names.insert(n.to_string(), d);
        }
    }

    fn resolve_imports(&mut self, m: usize) {
        self.cur = m;
        let items = self.asts[m].items.clone();
        let src = self.files[self.mods[m].file].src.clone();
        for it in &items {
            let ItemKind::Import { path, names } = &it.kind else { continue };
            let full: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
            if full[0] == "std" {
                let short = full.get(1).copied().unwrap_or("");
                if full.len() == 2 && self.std_by_name.contains_key(short) && names.is_none() {
                    let mut end = it.span.hi as usize;
                    if src.as_bytes().get(end) == Some(&b'\n') {
                        end += 1;
                    }
                    self.emit(
                        Diag::warning("W0001", it.span, format!("`std.{}` is always in scope; this import does nothing", short))
                            .with_fix(Span::new(it.span.lo as usize, end), ""),
                    );
                } else if !self.std_by_name.contains_key(short) {
                    let cands: Vec<String> = self.std_by_name.keys().cloned().collect();
                    let mut d = Diag::error("E0150", it.span, format!("there is no std module `{}`", short));
                    d = with_suggestion(d, path[1].span, suggest(short, cands.iter().map(|s| s.as_str())));
                    self.emit(d);
                } else {
                    self.emit(Diag::error("E0150", it.span, "std modules are always in scope: use `module.name` directly"));
                }
                continue;
            }
            let target_path = full.join(".");
            let Some(target) = self.mods.iter().position(|x| !x.is_std && x.path == target_path) else {
                let cands: Vec<String> = self.mods.iter().filter(|x| !x.is_std).map(|x| x.path.clone()).collect();
                let mut d = Diag::error("E0150", it.span, format!("module `{}` not found", target_path));
                if let Some((s, _)) = suggest(&target_path, cands.iter().map(|s| s.as_str())) {
                    d.message.push_str(&format!("; did you mean `{}`?", s));
                }
                self.emit(d);
                continue;
            };
            match names {
                None => {
                    let alias = path.last().unwrap().name.clone();
                    self.mods[m].imports.insert(alias, Decl::Module(target));
                }
                Some(list) => {
                    for n in list {
                        match self.mods[target].names.get(&n.name).copied() {
                            Some(decl) => {
                                if !self.decl_public(decl) {
                                    self.emit(Diag::error("E0903", n.span, format!("`{}` is private to `{}`; mark it `pub`", n.name, target_path)));
                                }
                                if self.mods[m].names.contains_key(&n.name) || self.mods[m].imports.contains_key(&n.name) {
                                    self.emit(Diag::error("E0901", n.span, format!("`{}` is already defined in this module", n.name)));
                                }
                                self.mods[m].imports.insert(n.name.clone(), decl);
                            }
                            None => {
                                let cands: Vec<String> = self.mods[target].names.keys().cloned().collect();
                                let mut d = Diag::error("E0151", n.span, format!("`{}` has no item `{}`", target_path, n.name));
                                d = with_suggestion(d, n.span, suggest(&n.name, cands.iter().map(|s| s.as_str())));
                                self.emit(d);
                            }
                        }
                    }
                }
            }
        }
    }

    fn decl_public(&self, d: Decl) -> bool {
        match d {
            Decl::Type(x) => self.defs[x].public,
            Decl::Fn(x) => self.fns[x].public,
            Decl::Const(x) => self.consts[x].public,
            Decl::Variant(x, _) => self.defs[x].public,
            Decl::Module(_) => true,
        }
    }

    fn lookup_in(&self, m: usize, name: &str) -> Option<Decl> {
        if let Some(d) = self.mods[m].names.get(name) {
            return Some(*d);
        }
        if let Some(d) = self.mods[m].imports.get(name) {
            return Some(*d);
        }
        if m != self.core_mod {
            if let Some(d) = self.mods[self.core_mod].names.get(name) {
                return Some(*d);
            }
        }
        self.std_by_name.get(name).map(|x| Decl::Module(*x))
    }

    /// All names visible in the current module (for suggestions).
    fn visible_names(&self) -> Vec<String> {
        let m = self.cur;
        let mut v: Vec<String> = self.mods[m].names.keys().cloned().collect();
        v.extend(self.mods[m].imports.keys().cloned());
        v.extend(self.mods[self.core_mod].names.keys().cloned());
        v.extend(self.std_by_name.keys().cloned());
        v.extend(self.locals.iter().map(|l| l.name.clone()));
        v
    }

    fn lower_def(&mut self, d: usize, aliases_only: bool) {
        let m = self.defs[d].module;
        self.cur = m;
        let item = self.asts[m].items.iter().find(|it| match &it.kind {
            ItemKind::Struct { name, .. } | ItemKind::Enum { name, .. } | ItemKind::TypeAlias { name, .. } => name.name == self.defs[d].name,
            _ => false,
        });
        let Some(item) = item.cloned() else { return };
        self.gens = self.defs[d].generics.iter().enumerate().map(|(i, g)| (g.clone(), Ty::Gen(Owner::Def(d), i))).collect();
        match (&item.kind, aliases_only) {
            (ItemKind::TypeAlias { ty, .. }, true) => {
                let t = self.lower_ty(ty);
                self.defs[d].kind = DefKind::Alias(t);
            }
            (ItemKind::Struct { fields, .. }, false) => {
                if matches!(self.defs[d].kind, DefKind::Opaque) {
                    return;
                }
                let fs = fields.iter().map(|f| (f.name.name.clone(), self.lower_ty(&f.ty))).collect();
                self.defs[d].kind = DefKind::Struct(fs);
            }
            (ItemKind::Enum { variants, .. }, false) => {
                let vs = variants
                    .iter()
                    .map(|v| {
                        let f = match &v.fields {
                            VariantFields::Unit => VFields::Unit,
                            VariantFields::Tuple(ts) => VFields::Tuple(ts.iter().map(|t| self.lower_ty(t)).collect()),
                            VariantFields::Named(fs) => VFields::Named(fs.iter().map(|f| (f.name.name.clone(), self.lower_ty(&f.ty))).collect()),
                        };
                        (v.name.name.clone(), f)
                    })
                    .collect();
                self.defs[d].kind = DefKind::Enum(vs);
            }
            _ => {}
        }
        self.gens.clear();
    }

    fn effect_bits(&mut self, uses: &[Ident]) -> u16 {
        let mut bits = 0;
        for u in uses {
            match EFFECTS.iter().position(|e| *e == u.name) {
                Some(i) => bits |= 1 << i,
                None => {
                    let mut d = Diag::error("E0120", u.span, format!("unknown effect `{}`", u.name)).expected(EFFECTS.join(", "));
                    d = with_suggestion(d, u.span, suggest(&u.name, EFFECTS.iter().copied()));
                    self.emit(d);
                }
            }
        }
        bits
    }

    fn lower_sig(&mut self, fid: usize) {
        let (m, i) = (self.fns[fid].module, self.fns[fid].item);
        self.cur = m;
        let ItemKind::Fn(f) = self.asts[m].items[i].kind.clone() else { return };
        self.gens = f.generics.iter().enumerate().map(|(i, g)| (g.name.clone(), Ty::Gen(Owner::Fn(fid), i))).collect();
        for g in &f.generics {
            self.naming(g, "generic parameter", is_pascal, to_pascal, "PascalCase");
        }
        let params: Vec<Param> = f.params.iter().map(|p| Param { name: p.name.name.clone(), ty: self.lower_ty(&p.ty), mutable: p.mutable }).collect();
        let ret = self.lower_ty(&f.ret);
        let effects = self.effect_bits(&f.uses);
        self.fns[fid].params = params.clone();
        self.fns[fid].ret = ret;
        self.fns[fid].effects = effects;
        self.gens.clear();
        if self.fns[fid].is_method {
            let head = match &params[0].ty {
                Ty::Named(d, _) => Some(*d),
                _ => None,
            };
            match head {
                Some(d) if self.defs[d].module == m => {
                    if self.methods.insert((d, f.name.name.clone()), fid).is_some() {
                        self.emit(Diag::error("E0901", f.name.span, format!("method `{}` is defined twice for `{}`", f.name.name, self.defs[d].name)));
                    }
                }
                Some(d) => self.emit(Diag::error(
                    "E0902",
                    f.params[0].ty.span,
                    format!("methods of `{}` must be defined in the module that defines it", self.defs[d].name),
                )),
                None => self.emit(Diag::error("E0902", f.params[0].ty.span, "`self` must be a named struct or enum type")),
            }
        }
        if let Some(owner) = &f.owner {
            match self.lookup_in(m, &owner.name) {
                Some(Decl::Type(d)) if self.defs[d].module == m => {
                    self.fns[fid].owner = Some(d);
                    if self.assoc.insert((d, f.name.name.clone()), fid).is_some() {
                        self.emit(Diag::error("E0901", f.name.span, format!("`{}.{}` is defined twice", owner.name, f.name.name)));
                    }
                }
                _ => self.emit(Diag::error("E0902", owner.span, format!("`{}` is not a type defined in this module", owner.name))),
            }
        }
    }

    // ============================================================ type lowering

    fn lower_ty(&mut self, ty: &Type) -> Ty {
        match &ty.kind {
            TypeKind::Tuple(items) => Ty::Tuple(items.iter().map(|t| self.lower_ty(t)).collect()),
            TypeKind::Fn { params, ret, uses } => {
                let ps = params.iter().map(|t| self.lower_ty(t)).collect();
                let r = self.lower_ty(ret);
                let e = self.effect_bits(uses);
                Ty::Fn(ps, Box::new(r), e, uses.is_empty())
            }
            TypeKind::Named { path, args } => {
                let args_t: Vec<Ty> = args.iter().map(|a| self.lower_ty(a)).collect();
                if path.len() == 1 {
                    if let Some((_, g)) = self.gens.iter().find(|(n, _)| *n == path[0].name) {
                        return g.clone();
                    }
                }
                let Some(def) = self.resolve_type_path(path) else { return Ty::Err };
                self.instantiate_def(def, args_t, ty.span)
            }
        }
    }

    fn resolve_type_path(&mut self, path: &[Ident]) -> Option<usize> {
        let mut decl = match self.lookup_in(self.cur, &path[0].name) {
            Some(d) => d,
            None => {
                self.unknown_type(&path[0]);
                return None;
            }
        };
        for seg in &path[1..] {
            decl = match decl {
                Decl::Module(m) => match self.mods[m].names.get(&seg.name).copied() {
                    Some(d) => d,
                    None => {
                        let cands: Vec<String> = self.mods[m].names.keys().cloned().collect();
                        let mut d = Diag::error("E0102", seg.span, format!("`{}` has no type `{}`", self.mods[m].path, seg.name));
                        d = with_suggestion(d, seg.span, suggest(&seg.name, cands.iter().map(|s| s.as_str())));
                        self.emit(d);
                        return None;
                    }
                },
                _ => {
                    self.emit(Diag::error("E0102", seg.span, "only modules contain types"));
                    return None;
                }
            };
        }
        match decl {
            Decl::Type(d) => Some(d),
            _ => {
                let span = path.last().unwrap().span;
                self.emit(Diag::error("E0102", span, format!("`{}` is not a type", path.last().unwrap().name)));
                None
            }
        }
    }

    /// `HttpError` → `http.HttpError`: the unique std module that defines `name`.
    fn qualify_std(&self, name: &str, want_type: bool) -> Option<String> {
        let hits: Vec<String> = self
            .mods
            .iter()
            .enumerate()
            .filter(|(i, m)| m.is_std && *i != self.core_mod)
            .filter(|(_, m)| match m.names.get(name) {
                Some(Decl::Type(_)) => want_type,
                Some(Decl::Fn(_)) | Some(Decl::Const(_)) => !want_type,
                _ => false,
            })
            .map(|(_, m)| format!("{}.{}", m.path.trim_start_matches("std."), name))
            .collect();
        if hits.len() == 1 { hits.into_iter().next() } else { None }
    }

    fn unknown_type(&mut self, id: &Ident) {
        let mut d = Diag::error("E0102", id.span, format!("unknown type `{}`", id.name));
        if let Some(q) = self.qualify_std(&id.name, true) {
            d.message = format!("unknown type `{}`: it lives in a std module, write `{}`", id.name, q);
            d = d.with_fix(id.span, q);
        } else if let Some((_, to)) = TYPE_ALIASES.iter().find(|(from, _)| *from == id.name) {
            d.message = format!("unknown type `{}`: Drift calls it `{}`", id.name, to);
            d = d.with_fix(id.span, *to);
        } else {
            let mut cands: Vec<String> = self.visible_names().into_iter().filter(|n| is_pascal(n)).collect();
            cands.extend(self.gens.iter().map(|(n, _)| n.clone()));
            d = with_suggestion(d, id.span, suggest(&id.name, cands.iter().map(|s| s.as_str())));
        }
        self.emit(d);
    }

    fn instantiate_def(&mut self, def: usize, mut args: Vec<Ty>, span: Span) -> Ty {
        if def == self.core.never {
            return Ty::Never;
        }
        let n = self.defs[def].generics.len();
        if args.len() != n {
            let name = self.defs[def].name.clone();
            let mut d = Diag::error("E0110", span, format!("`{}` takes {} type argument(s), found {}", name, n, args.len()));
            if n == 0 && !args.is_empty() {
                d = d.with_fix(span, name);
            }
            self.emit(d);
            args.resize(n, Ty::Err);
        }
        if let DefKind::Alias(t) = &self.defs[def].kind {
            return subst(&t.clone(), Owner::Def(def), &args);
        }
        Ty::Named(def, args)
    }

    // ============================================================ inference core

    fn fresh(&mut self) -> Ty {
        self.vars.push(None);
        Ty::Var(self.vars.len() - 1)
    }

    fn resolve(&self, t: &Ty) -> Ty {
        let mut t = t.clone();
        while let Ty::Var(v) = t {
            match &self.vars[v] {
                Some(x) => t = x.clone(),
                None => break,
            }
        }
        t
    }

    fn zonk(&self, t: &Ty) -> Ty {
        match self.resolve(t) {
            Ty::Named(d, a) => Ty::Named(d, a.iter().map(|x| self.zonk(x)).collect()),
            Ty::Tuple(v) => Ty::Tuple(v.iter().map(|x| self.zonk(x)).collect()),
            Ty::Fn(p, r, e, poly) => Ty::Fn(p.iter().map(|x| self.zonk(x)).collect(), Box::new(self.zonk(&r)), e, poly),
            other => other,
        }
    }

    fn occurs(&self, v: usize, t: &Ty) -> bool {
        match self.resolve(t) {
            Ty::Var(x) => x == v,
            Ty::Named(_, a) => a.iter().any(|x| self.occurs(v, x)),
            Ty::Tuple(a) => a.iter().any(|x| self.occurs(v, x)),
            Ty::Fn(p, r, _, _) => p.iter().any(|x| self.occurs(v, x)) || self.occurs(v, &r),
            _ => false,
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            // an unknown type absorbs inference variables so one mistake reports once
            (Ty::Var(x), Ty::Err) | (Ty::Err, Ty::Var(x)) => {
                self.vars[*x] = Some(Ty::Err);
                true
            }
            (Ty::Err, _) | (_, Ty::Err) | (Ty::Never, _) | (_, Ty::Never) => true,
            (Ty::Var(x), Ty::Var(y)) if x == y => true,
            (Ty::Var(x), _) => {
                if self.occurs(*x, &b) {
                    return false;
                }
                self.vars[*x] = Some(b.clone());
                true
            }
            (_, Ty::Var(y)) => {
                if self.occurs(*y, &a) {
                    return false;
                }
                self.vars[*y] = Some(a.clone());
                true
            }
            (Ty::Named(d1, a1), Ty::Named(d2, a2)) => {
                if d1 != d2 || a1.len() != a2.len() {
                    return false;
                }
                let mut ok = true;
                for (x, y) in a1.iter().zip(a2.iter()) {
                    ok &= self.unify(x, y);
                }
                ok
            }
            (Ty::Tuple(x), Ty::Tuple(y)) => {
                if x.len() != y.len() {
                    return false;
                }
                let mut ok = true;
                for (p, q) in x.iter().zip(y.iter()) {
                    ok &= self.unify(p, q);
                }
                ok
            }
            (Ty::Fn(p1, r1, _, _), Ty::Fn(p2, r2, _, _)) => {
                if p1.len() != p2.len() {
                    return false;
                }
                let mut ok = true;
                for (p, q) in p1.iter().zip(p2.iter()) {
                    ok &= self.unify(p, q);
                }
                ok & self.unify(r1, r2)
            }
            (Ty::Gen(o1, i1), Ty::Gen(o2, i2)) => o1 == o2 && i1 == i2,
            _ => false,
        }
    }

    /// Would `a` and `b` unify? Leaves no trace.
    fn probe(&mut self, a: &Ty, b: &Ty) -> bool {
        let saved = self.vars.clone();
        let ok = self.unify(a, b);
        self.vars = saved;
        ok
    }

    fn show(&self, t: &Ty) -> String {
        match self.resolve(t) {
            Ty::Named(d, args) => {
                let def = &self.defs[d];
                let mut s = String::new();
                if def.module != self.core_mod && def.module != self.cur {
                    s.push_str(self.mods[def.module].path.rsplit('.').next().unwrap_or(""));
                    s.push('.');
                }
                s.push_str(&def.name);
                if !args.is_empty() {
                    s.push('[');
                    s.push_str(&args.iter().map(|a| self.show(a)).collect::<Vec<_>>().join(", "));
                    s.push(']');
                }
                s
            }
            Ty::Tuple(v) => {
                if v.len() == 1 {
                    format!("({},)", self.show(&v[0]))
                } else {
                    format!("({})", v.iter().map(|a| self.show(a)).collect::<Vec<_>>().join(", "))
                }
            }
            Ty::Fn(p, r, e, _) => {
                let mut s = format!("fn({}) -> {}", p.iter().map(|a| self.show(a)).collect::<Vec<_>>().join(", "), self.show(&r));
                if e != 0 {
                    s.push_str(&format!(" uses {}", effect_list(e).join(", ")));
                }
                s
            }
            Ty::Gen(o, i) => match o {
                Owner::Def(d) => self.defs[d].generics.get(i).cloned().unwrap_or_default(),
                Owner::Fn(f) => self.fns[f].generics.get(i).cloned().unwrap_or_default(),
            },
            Ty::Var(_) => "_".into(),
            Ty::Never => "Never".into(),
            Ty::Err => "{unknown}".into(),
        }
    }

    fn t_int(&self) -> Ty {
        Ty::Named(self.core.int, vec![])
    }
    fn t_bool(&self) -> Ty {
        Ty::Named(self.core.bool, vec![])
    }
    fn t_str(&self) -> Ty {
        Ty::Named(self.core.str, vec![])
    }
    fn head(&self, t: &Ty) -> Option<usize> {
        match self.resolve(t) {
            Ty::Named(d, _) => Some(d),
            _ => None,
        }
    }
    fn is_int(&self, t: &Ty) -> bool {
        let c = self.core;
        self.head(t).map_or(false, |d| [c.int, c.i32, c.u8, c.u32, c.u64].contains(&d))
    }
    fn is_float(&self, t: &Ty) -> bool {
        self.head(t).map_or(false, |d| d == self.core.float || d == self.core.f32)
    }
    fn is_numeric(&self, t: &Ty) -> bool {
        self.is_int(t) || self.is_float(t)
    }
    fn is_fn(&self, t: &Ty) -> bool {
        matches!(self.resolve(t), Ty::Fn(..))
    }
    fn args_of(&self, t: &Ty) -> Vec<Ty> {
        match self.resolve(t) {
            Ty::Named(_, a) => a,
            _ => vec![],
        }
    }

    fn struct_fields(&self, d: usize, args: &[Ty]) -> Option<Vec<(String, Ty)>> {
        match &self.defs[d].kind {
            DefKind::Struct(fs) => Some(fs.iter().map(|(n, t)| (n.clone(), subst(t, Owner::Def(d), args))).collect()),
            _ => None,
        }
    }

    fn variants(&self, d: usize, args: &[Ty]) -> Option<Vec<(String, VFields)>> {
        match &self.defs[d].kind {
            DefKind::Enum(vs) => Some(
                vs.iter()
                    .map(|(n, f)| {
                        let f = match f {
                            VFields::Unit => VFields::Unit,
                            VFields::Tuple(ts) => VFields::Tuple(ts.iter().map(|t| subst(t, Owner::Def(d), args)).collect()),
                            VFields::Named(fs) => VFields::Named(fs.iter().map(|(n, t)| (n.clone(), subst(t, Owner::Def(d), args))).collect()),
                        };
                        (n.clone(), f)
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    fn fresh_args(&mut self, d: usize) -> Vec<Ty> {
        (0..self.defs[d].generics.len()).map(|_| self.fresh()).collect()
    }

    /// Fresh instance of `d`, pre-unified with `exp` when it names the same type.
    fn instance_for(&mut self, d: usize, exp: Option<&Ty>) -> Vec<Ty> {
        let args = self.fresh_args(d);
        if let Some(x) = exp {
            let t = Ty::Named(d, args.clone());
            if self.probe(&t, x) {
                self.unify(&t, x);
            }
        }
        args.iter().map(|a| self.resolve(a)).collect()
    }

    // ============================================================ modules

    fn check_module(&mut self, m: usize) {
        self.cur = m;
        let items = self.asts[m].items.clone();
        for (idx, it) in items.iter().enumerate() {
            match &it.kind {
                ItemKind::Fn(f) => {
                    if let Some(fid) = (0..self.fns.len()).find(|x| self.fns[*x].module == m && self.fns[*x].item == idx) {
                        self.check_fn(fid, f);
                    }
                }
                ItemKind::Const { ty: _, value, name } => {
                    let Some(Decl::Const(c)) = self.mods[m].names.get(&name.name).copied() else { continue };
                    let t = self.consts[c].ty.clone();
                    self.begin_body(0, None, String::from("a constant"));
                    self.expr(value, Some(&t));
                    self.end_body();
                }
                ItemKind::Examples { target, cases } => {
                    let exists = matches!(self.mods[m].names.get(&target.name), Some(Decl::Fn(_)))
                        || self.fns.iter().any(|f| f.module == m && f.name == target.name);
                    if !exists {
                        let cands: Vec<String> = self.fns.iter().filter(|f| f.module == m).map(|f| f.name.clone()).collect();
                        let mut d = Diag::error("E0140", target.span, format!("`examples` names `{}`, which is not a function in this module", target.name));
                        d = with_suggestion(d, target.span, suggest(&target.name, cands.iter().map(|s| s.as_str())));
                        self.emit(d);
                    }
                    self.begin_body(0, None, String::from("`examples` (must be pure)"));
                    self.rets.push(Ty::Err);
                    let b = self.t_bool();
                    for c in cases {
                        self.expr(c, Some(&b));
                    }
                    self.end_body();
                }
                ItemKind::Test { uses, body, .. } => {
                    let e = self.effect_bits(uses);
                    let ins = if uses.is_empty() { (body.span.lo, false) } else { (uses.last().unwrap().span.hi, true) };
                    self.begin_body(e, Some(ins), String::from("this test"));
                    let r = Ty::Named(self.core.result, vec![unit(), Ty::Named(self.core.error, vec![])]);
                    self.rets.push(r);
                    self.block(body, None, true);
                    self.end_body();
                }
                _ => {}
            }
        }
    }

    fn begin_body(&mut self, effects: u16, insert: Option<(u32, bool)>, owner: String) {
        self.vars.clear();
        self.locals.clear();
        self.scopes.clear();
        self.rets.clear();
        self.holes.clear();
        self.infer_lets.clear();
        self.reified.clear();
        self.effects = effects;
        self.reported_effects = 0;
        self.effect_insert = insert;
        self.effect_owner = owner;
        self.closure_depth = 0;
        self.loop_depth = 0;
        self.push_scope();
    }

    fn end_body(&mut self) {
        self.pop_scope();
        for (span, ty, scope) in std::mem::take(&mut self.holes) {
            let ts = self.show(&self.zonk(&ty));
            let mut d = Diag::error("H0001", span, format!("hole: expected `{}`", ts)).expected(ts);
            d.severity = Severity::Hole;
            d.scope = scope.iter().map(|(n, t)| (n.clone(), self.show(&self.zonk(t)))).collect();
            self.emit(d);
        }
        for (span, ty) in std::mem::take(&mut self.reified) {
            let z = self.zonk(&ty);
            match self.decodable(&z, &mut Vec::new()) {
                Ok(()) => self.note(span, b'g', Note::TypeArg(z)),
                Err(why) => {
                    self.emit(Diag::error("E0803", span, format!("`json.decode` cannot decode this type: {}", why)));
                    self.poison(&z);
                }
            }
        }
        for (name, span, ty) in std::mem::take(&mut self.infer_lets) {
            let z = self.zonk(&ty);
            if self.has_var(&z) {
                let shown = self.show(&z);
                self.emit(Diag::error("E0802", span, format!("cannot infer the type of `{}` (`{}`); add an annotation: `let {}: Type = ...`", name, shown, name)));
            }
        }
        self.gens.clear();
    }

    /// Bind every unknown in `t` to the error type (its cause was already reported).
    fn poison(&mut self, t: &Ty) {
        match t {
            Ty::Var(v) => self.vars[*v] = Some(Ty::Err),
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().for_each(|x| self.poison(x)),
            Ty::Fn(p, r, _, _) => {
                p.iter().for_each(|x| self.poison(x));
                self.poison(r);
            }
            _ => {}
        }
    }

    /// Whether values of `t` can be rebuilt from JSON (a concrete type made of data, no functions or handles).
    fn decodable(&self, t: &Ty, seen: &mut Vec<usize>) -> Result<(), String> {
        let c = self.core;
        match t {
            Ty::Err => Ok(()),
            Ty::Var(_) => Err("the target type is not known; annotate it: `let x: T = json.decode(text)?` or `json.decode[T](text)`".into()),
            Ty::Gen(..) => Err(format!("`{}` is a type parameter here; decode at a call site where the type is concrete", self.show(t))),
            Ty::Fn(..) => Err("functions are not data".into()),
            Ty::Never => Err("`Never` has no values".into()),
            Ty::Tuple(ts) => ts.iter().try_for_each(|x| self.decodable(x, seen)),
            Ty::Named(d, args) => {
                let d = *d;
                if [c.int, c.i32, c.u8, c.u32, c.u64, c.float, c.f32, c.bool, c.str, c.bytes].contains(&d) {
                    return Ok(());
                }
                if [c.list, c.set, c.map, c.option, c.result].contains(&d) {
                    return args.iter().try_for_each(|x| self.decodable(x, seen));
                }
                if seen.contains(&d) {
                    return Ok(());
                }
                seen.push(d);
                let o = Owner::Def(d);
                let r = match &self.defs[d].kind {
                    DefKind::Struct(fs) if !(fs.is_empty() && self.mods[self.defs[d].module].is_std) => {
                        fs.iter().try_for_each(|(_, ft)| self.decodable(&subst(ft, o, args), seen))
                    }
                    DefKind::Enum(vs) => vs.iter().try_for_each(|(_, v)| match v {
                        VFields::Unit => Ok(()),
                        VFields::Tuple(ts) => ts.iter().try_for_each(|x| self.decodable(&subst(x, o, args), seen)),
                        VFields::Named(fs) => fs.iter().try_for_each(|(_, x)| self.decodable(&subst(x, o, args), seen)),
                    }),
                    DefKind::Alias(a) => self.decodable(&subst(a, o, args), seen),
                    _ => Err(format!("`{}` is an opaque runtime value, not data", self.show(t))),
                };
                seen.pop();
                r
            }
        }
    }

    fn has_var(&self, t: &Ty) -> bool {
        match t {
            Ty::Var(_) => true,
            Ty::Named(_, a) | Ty::Tuple(a) => a.iter().any(|x| self.has_var(x)),
            Ty::Fn(p, r, _, _) => p.iter().any(|x| self.has_var(x)) || self.has_var(r),
            _ => false,
        }
    }

    fn check_fn(&mut self, fid: usize, f: &FnDecl) {
        let info = self.fns[fid].clone();
        let ins = if f.uses.is_empty() { (f.ret.span.hi, false) } else { (f.uses.last().unwrap().span.hi, true) };
        self.begin_body(info.effects, Some(ins), format!("`{}`", info.name));
        self.gens = info.generics.iter().enumerate().map(|(i, g)| (g.clone(), Ty::Gen(Owner::Fn(fid), i))).collect();
        for (p, ast) in info.params.iter().zip(f.params.iter()) {
            self.declare(&p.name, p.ty.clone(), ast.name.span, p.mutable, LocalKind::Param { is_self: p.name == "self" });
        }
        let b = self.t_bool();
        let saved = (self.effects, self.effect_insert.take());
        self.effects = 0;
        self.effect_owner = String::from("`requires` (must be pure)");
        for r in &f.requires {
            self.expr(r, Some(&b));
        }
        self.effects = saved.0;
        self.effect_insert = saved.1;
        self.effect_owner = format!("`{}`", info.name);
        self.rets.push(info.ret.clone());
        self.block(&f.body, Some(&info.ret), false);
        if !f.ensures.is_empty() {
            self.push_scope();
            self.declare("result", info.ret.clone(), f.ret.span, false, LocalKind::Pattern);
            let saved = (self.effects, self.effect_insert.take());
            self.effects = 0;
            for e in &f.ensures {
                self.expr(e, Some(&b));
            }
            self.effects = saved.0;
            self.effect_insert = saved.1;
            self.pop_scope();
        }
        self.end_body();
    }

    // ============================================================ scopes

    fn push_scope(&mut self) {
        self.scopes.push(self.locals.len());
    }

    fn pop_scope(&mut self) {
        let mark = self.scopes.pop().unwrap_or(0);
        let gone: Vec<Local> = self.locals.drain(mark..).collect();
        for l in gone {
            if let LocalKind::Let { kw } = l.kind {
                if l.mutable && !l.mutated {
                    self.emit(Diag::warning("W0201", l.span, format!("`{}` is never changed; declare it with `let`", l.name)).with_fix(kw, "let"));
                }
            }
        }
    }

    fn declare(&mut self, name: &str, ty: Ty, span: Span, mutable: bool, kind: LocalKind) {
        if name == "_" {
            return;
        }
        if self.locals.iter().any(|l| l.name == name) {
            self.emit(Diag::error("E0601", span, format!("`{}` is already defined in this function; Drift has no shadowing, choose a new name", name)));
        } else if !is_snake(name) {
            self.emit(Diag::error("E0701", span, format!("variable `{}` must be snake_case: `{}`", name, to_snake(name))).expected(to_snake(name)));
        }
        if let LocalKind::Let { .. } = kind {
            if self.has_var(&self.zonk(&ty)) {
                self.infer_lets.push((name.to_string(), span, ty.clone()));
            }
        }
        self.locals.push(Local { name: name.to_string(), ty, mutable, span, kind, mutated: false, depth: self.closure_depth });
    }

    fn find_local(&self, name: &str) -> Option<usize> {
        self.locals.iter().rposition(|l| l.name == name)
    }

    // ============================================================ effects

    fn require_effects(&mut self, bits: u16, span: Span, what: &str) {
        let missing = bits & !self.effects & !self.reported_effects;
        if missing == 0 {
            return;
        }
        self.reported_effects |= missing;
        let names = effect_list(missing);
        let mut d = Diag::error(
            "E0301",
            span,
            format!("{} uses `{}`, but {} does not declare it", what, names.join(", "), self.effect_owner),
        )
        .expected(format!("uses {}", effect_list(self.effects | missing).join(", ")));
        if let Some((pos, has_uses)) = self.effect_insert {
            let text = if has_uses { format!(", {}", names.join(", ")) } else if self.effect_owner == "this test" { format!("uses {} ", names.join(", ")) } else { format!(" uses {}", names.join(", ")) };
            d = d.with_fix(Span::empty_at(pos), text);
            // later calls in the same body see the fixed signature
            self.effects |= missing;
            if !has_uses {
                self.effect_insert = Some((pos, true));
            }
        }
        self.emit(d);
    }

    // ============================================================ statements

    fn block(&mut self, b: &Block, exp: Option<&Ty>, discard: bool) -> Ty {
        self.push_scope();
        let n = b.stmts.len();
        let mut last_ty = unit();
        for (i, s) in b.stmts.iter().enumerate() {
            let last = i + 1 == n;
            if last && !discard {
                if let StmtKind::Expr(e) = &s.kind {
                    let t = self.expr(e, exp);
                    self.pop_scope();
                    return t;
                }
            }
            last_ty = self.stmt(s);
        }
        self.pop_scope();
        if matches!(last_ty, Ty::Never) {
            return Ty::Never;
        }
        if discard {
            return unit();
        }
        if let Some(x) = exp {
            if !self.unify(&unit(), x) {
                let shown = self.show(x);
                let at = Span::empty_at(b.span.hi.saturating_sub(1));
                self.emit(Diag::error("E0206", at, format!("block ends without a value; expected `{}`", shown)).expected(shown).found("()"));
                return x.clone();
            }
        }
        unit()
    }

    fn stmt(&mut self, s: &Stmt) -> Ty {
        match &s.kind {
            StmtKind::Let { mutable, pat, ty, value } => {
                let declared = ty.as_ref().map(|t| self.lower_ty(t));
                let vt = self.expr(value, declared.as_ref());
                let t = declared.unwrap_or(vt);
                let kw = Span::new(s.span.lo as usize, s.span.lo as usize + 3);
                match &pat.kind {
                    PatKind::Bind(n) => self.declare(n, t, pat.span, *mutable, LocalKind::Let { kw }),
                    _ => {
                        let p = self.bind_pat(pat, &t, *mutable);
                        self.require_irrefutable(&p, &t, pat.span, "let");
                    }
                }
                unit()
            }
            StmtKind::Assign { target, op, value } => {
                self.mark_mutation(target);
                let tt = self.expr(target, None);
                self.expr(value, Some(&tt));
                if *op != AssignOp::Set {
                    let ok = self.is_numeric(&tt)
                        || (*op == AssignOp::Add && matches!(self.head(&tt), Some(d) if d == self.core.str || d == self.core.list || d == self.core.bytes))
                        || matches!(self.resolve(&tt), Ty::Err | Ty::Var(_));
                    if !ok {
                        let shown = self.show(&tt);
                        self.emit(Diag::error("E0211", s.span, format!("compound assignment is not defined for `{}`", shown)));
                    }
                }
                unit()
            }
            StmtKind::For { pat, iter, body } => {
                let it = self.expr(iter, None);
                let elem = self.elem_type(&it, iter.span);
                self.push_scope();
                let p = self.bind_pat(pat, &elem, false);
                self.require_irrefutable(&p, &elem, pat.span, "for");
                self.loop_depth += 1;
                self.block(body, None, true);
                self.loop_depth -= 1;
                self.pop_scope();
                unit()
            }
            StmtKind::While { cond, body } => {
                let b = self.t_bool();
                self.expr(cond, Some(&b));
                self.loop_depth += 1;
                self.block(body, None, true);
                self.loop_depth -= 1;
                unit()
            }
            StmtKind::Loop { body } => {
                self.loop_depth += 1;
                self.block(body, None, true);
                self.loop_depth -= 1;
                if contains_break(&body.stmts) { unit() } else { Ty::Never }
            }
            StmtKind::Assert { cond, message } => {
                let b = self.t_bool();
                self.expr(cond, Some(&b));
                if let Some(m) = message {
                    let st = self.t_str();
                    self.expr(m, Some(&st));
                }
                unit()
            }
            StmtKind::Expr(e) => {
                let t = self.expr(e, None);
                if self.head(&t) == Some(self.core.result) {
                    let mut d = Diag::error("E0250", e.span, "this `Result` is ignored; handle the error (usually with `?`)");
                    let ret_ok = self.rets.last().map_or(false, |r| matches!(self.head(r), Some(h) if h == self.core.result));
                    if ret_ok {
                        d = d.with_fix(Span::empty_at(e.span.hi), "?");
                    }
                    self.emit(d);
                }
                t
            }
        }
    }

    fn elem_type(&mut self, it: &Ty, span: Span) -> Ty {
        let c = self.core;
        let args = self.args_of(it);
        match self.head(it) {
            Some(d) if d == c.list || d == c.set => args[0].clone(),
            Some(d) if d == c.map => Ty::Tuple(vec![args[0].clone(), args[1].clone()]),
            Some(d) if d == c.range => self.t_int(),
            _ => {
                if matches!(self.resolve(it), Ty::Err) {
                    return Ty::Err;
                }
                let shown = self.show(it);
                let mut d = Diag::error("E0221", span, format!("cannot iterate over `{}`", shown)).found(shown.clone());
                if self.head(it) == Some(c.str) {
                    d.message.push_str("; use `.chars()` or `.split(...)`");
                    d = d.with_fix(Span::empty_at(span.hi), ".chars()");
                }
                self.emit(d);
                Ty::Err
            }
        }
    }

    /// Mutating `target` requires a `var` root that is not captured by a closure.
    fn mark_mutation(&mut self, target: &Expr) {
        let mut root = target;
        loop {
            match &root.kind {
                ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => root = base,
                _ => break,
            }
        }
        let ExprKind::Name(n) = &root.kind else {
            self.emit(Diag::error("E0505", target.span, "only variables (or their fields and elements) can be changed"));
            return;
        };
        let Some(i) = self.find_local(n) else { return };
        let l = self.locals[i].clone();
        if l.depth < self.closure_depth {
            self.emit(Diag::error("E0503", target.span, format!("closures capture by value: `{}` cannot be changed inside this closure; return the new value instead", n)));
            return;
        }
        if !l.mutable {
            let mut d = Diag::error("E0501", target.span, format!("`{}` is immutable", n));
            match l.kind {
                LocalKind::Let { kw } => {
                    d.message.push_str("; declare it with `var`");
                    d = d.with_fix(kw, "var");
                }
                LocalKind::Param { is_self: true } => {
                    d.message.push_str("; declare the parameter as `var self` to change the caller's value");
                    d = d.with_fix(Span::empty_at(l.span.lo), "var ");
                }
                LocalKind::Param { is_self: false } => d.message.push_str("; parameters cannot change: return a new value instead"),
                LocalKind::Pattern => d.message.push_str("; pattern bindings cannot change"),
            }
            self.emit(d);
            return;
        }
        self.locals[i].mutated = true;
    }

    // ============================================================ patterns

    fn require_irrefutable(&mut self, p: &P, ty: &Ty, span: Span, ctx: &str) {
        let rows: Vec<Vec<P>> = vec![vec![p.clone()]];
        if let Some(w) = self.useful(&rows, &[P::Wild], &[ty.clone()]) {
            let missing = self.show_pat(&w[0], ty);
            self.emit(Diag::error("E0260", span, format!("this `{}` pattern can fail (e.g. `{}` is not matched); use `match`", ctx, missing)));
        }
    }

    fn resolve_variant_path(&mut self, path: &[Ident]) -> Option<(usize, Option<usize>)> {
        // returns (def, Some(variant)) for a variant, (def, None) for a struct
        let mut decl = match self.lookup_in(self.cur, &path[0].name) {
            Some(d) => d,
            None => {
                self.unknown_name(&path[0]);
                return None;
            }
        };
        for seg in &path[1..] {
            decl = match decl {
                Decl::Module(m) => match self.mods[m].names.get(&seg.name).copied() {
                    Some(d) => d,
                    None => {
                        self.emit(Diag::error("E0100", seg.span, format!("`{}` has no item `{}`", self.mods[m].path, seg.name)));
                        return None;
                    }
                },
                Decl::Type(d) => match self.variant_index(d, &seg.name) {
                    Some(i) => Decl::Variant(d, i),
                    None => {
                        self.unknown_variant(d, seg);
                        return None;
                    }
                },
                _ => {
                    self.emit(Diag::error("E0100", seg.span, "not a variant path"));
                    return None;
                }
            };
        }
        match decl {
            Decl::Variant(d, i) => Some((d, Some(i))),
            Decl::Type(d) => Some((d, None)),
            _ => {
                let span = path.last().unwrap().span;
                self.emit(Diag::error("E0132", span, "patterns must name an enum variant or a struct"));
                None
            }
        }
    }

    fn variant_index(&self, d: usize, name: &str) -> Option<usize> {
        match &self.defs[d].kind {
            DefKind::Enum(vs) => vs.iter().position(|(n, _)| n == name),
            _ => None,
        }
    }

    fn unknown_variant(&mut self, d: usize, seg: &Ident) {
        let def = self.defs[d].clone();
        let mut cands: Vec<String> = match &def.kind {
            DefKind::Enum(vs) => vs.iter().map(|(n, _)| n.clone()).collect(),
            _ => vec![],
        };
        cands.extend(self.assoc.keys().filter(|(x, _)| *x == d).map(|(_, n)| n.clone()));
        let mut diag = Diag::error("E0104", seg.span, format!("`{}` has no variant or associated function `{}`", def.name, seg.name));
        let sg = suggest(&seg.name, cands.iter().map(|s| s.as_str()));
        if sg.is_none() && !cands.is_empty() {
            diag.message.push_str(&format!(" (has: {})", cands.join(", ")));
        }
        diag = with_suggestion(diag, seg.span, sg);
        self.emit(diag);
    }

    fn bind_pat(&mut self, p: &Pat, ty: &Ty, mutable: bool) -> P {
        match &p.kind {
            PatKind::Wildcard => P::Wild,
            PatKind::Bind(n) => {
                self.declare(n, ty.clone(), p.span, mutable, if mutable { LocalKind::Let { kw: Span::empty_at(p.span.lo) } } else { LocalKind::Pattern });
                if mutable {
                    // mark as mutated to avoid a misleading fix pointing at the pattern
                    if let Some(l) = self.locals.last_mut() {
                        l.mutated = true;
                    }
                }
                P::Wild
            }
            PatKind::Lit(e) => {
                self.expr(e, Some(ty));
                match &e.kind {
                    ExprKind::Bool(true) => P::Ctor(Ctor::True, vec![]),
                    ExprKind::Bool(false) => P::Ctor(Ctor::False, vec![]),
                    _ => P::Lit(self.text(e.span)),
                }
            }
            PatKind::Tuple(items) => {
                let elems: Vec<Ty> = match self.resolve(ty) {
                    Ty::Tuple(ts) if ts.len() == items.len() => ts,
                    Ty::Err => vec![Ty::Err; items.len()],
                    other => {
                        let fresh: Vec<Ty> = items.iter().map(|_| self.fresh()).collect();
                        if !self.unify(&other, &Ty::Tuple(fresh.clone())) {
                            let shown = self.show(&other);
                            self.emit(Diag::error("E0201", p.span, format!("tuple pattern of {} elements does not match `{}`", items.len(), shown)).found(shown));
                        }
                        fresh
                    }
                };
                let ps = items.iter().zip(elems.iter()).map(|(i, t)| self.bind_pat(i, t, mutable)).collect();
                P::Ctor(Ctor::Tuple(items.len()), ps)
            }
            PatKind::List { items, rest } => {
                let elem = self.fresh();
                let lt = Ty::Named(self.core.list, vec![elem.clone()]);
                if !self.unify(ty, &lt) {
                    let shown = self.show(ty);
                    self.emit(Diag::error("E0201", p.span, format!("list pattern does not match `{}`", shown)).found(shown));
                }
                let ps: Vec<P> = items.iter().map(|i| self.bind_pat(i, &elem, mutable)).collect();
                let mut tail = match rest {
                    Some(r) => {
                        if let Some(id) = r {
                            self.declare(&id.name, lt.clone(), id.span, false, LocalKind::Pattern);
                        }
                        P::Wild
                    }
                    None => P::Ctor(Ctor::Nil, vec![]),
                };
                for x in ps.into_iter().rev() {
                    tail = P::Ctor(Ctor::Cons, vec![x, tail]);
                }
                tail
            }
            PatKind::Unit { path } | PatKind::TupleVariant { path, .. } | PatKind::StructVariant { path, .. } => {
                let Some((d, vi)) = self.resolve_variant_path(path) else { return P::Wild };
                let args = self.instance_for(d, Some(ty));
                let inst = Ty::Named(d, args.clone());
                if !self.unify(&inst, ty) {
                    let (a, b) = (self.show(ty), self.show(&inst));
                    self.emit(Diag::error("E0201", p.span, format!("pattern of type `{}` cannot match a value of type `{}`", b, a)).expected(a).found(b));
                    return P::Wild;
                }
                let (fields, ctor) = match vi {
                    Some(i) => (self.variants(d, &args).unwrap()[i].1.clone(), Ctor::Variant(d, i)),
                    None => match self.struct_fields(d, &args) {
                        Some(fs) => (VFields::Named(fs), Ctor::Struct(d)),
                        None => {
                            self.emit(Diag::error("E0132", p.span, "this type cannot be destructured"));
                            return P::Wild;
                        }
                    },
                };
                self.note(p.span, b'P', match vi {
                    Some(i) => Note::Variant(d, i),
                    None => Note::Struct(d),
                });
                let vname = path.last().unwrap().name.clone();
                match (&p.kind, fields) {
                    (PatKind::Unit { .. }, VFields::Unit) => P::Ctor(ctor, vec![]),
                    (PatKind::TupleVariant { items, .. }, VFields::Tuple(ts)) => {
                        if items.len() != ts.len() {
                            self.emit(Diag::error("E0203", p.span, format!("`{}` has {} field(s), pattern has {}", vname, ts.len(), items.len())));
                        }
                        let ps = ts.iter().enumerate().map(|(i, t)| match items.get(i) {
                            Some(sp) => self.bind_pat(sp, t, mutable),
                            None => P::Wild,
                        }).collect();
                        P::Ctor(ctor, ps)
                    }
                    (PatKind::StructVariant { fields: pf, rest, .. }, VFields::Named(fs)) => {
                        let mut ps = vec![P::Wild; fs.len()];
                        for (fname, sub) in pf {
                            match fs.iter().position(|(n, _)| *n == fname.name) {
                                Some(i) => {
                                    let ft = fs[i].1.clone();
                                    ps[i] = match sub {
                                        Some(sp) => self.bind_pat(sp, &ft, mutable),
                                        None => {
                                            self.declare(&fname.name, ft, fname.span, mutable, LocalKind::Pattern);
                                            P::Wild
                                        }
                                    };
                                }
                                None => {
                                    let cands: Vec<&str> = fs.iter().map(|(n, _)| n.as_str()).collect();
                                    let mut d = Diag::error("E0311", fname.span, format!("`{}` has no field `{}`", vname, fname.name));
                                    d = with_suggestion(d, fname.span, suggest(&fname.name, cands));
                                    self.emit(d);
                                }
                            }
                        }
                        let missing: Vec<String> = fs.iter().filter(|(n, _)| !pf.iter().any(|(f, _)| f.name == *n)).map(|(n, _)| n.clone()).collect();
                        if !rest && !missing.is_empty() {
                            let at = Span::empty_at(p.span.hi - 1);
                            let text = if pf.is_empty() { ".. ".to_string() } else { ", .. ".to_string() };
                            self.emit(
                                Diag::error("E0270", p.span, format!("pattern does not mention {}; add `..` to ignore them", missing.iter().map(|m| format!("`{}`", m)).collect::<Vec<_>>().join(", ")))
                                    .with_fix(at, text),
                            );
                        }
                        P::Ctor(ctor, ps)
                    }
                    (_, f) => {
                        let shape = match f {
                            VFields::Unit => format!("`{}`", vname),
                            VFields::Tuple(_) => format!("`{}(...)`", vname),
                            VFields::Named(_) => format!("`{} {{ ... }}`", vname),
                        };
                        self.emit(Diag::error("E0132", p.span, format!("wrong pattern shape: write {}", shape)));
                        P::Wild
                    }
                }
            }
            PatKind::Or(alts) => {
                let before = self.locals.len();
                let ps: Vec<P> = alts.iter().map(|a| self.bind_pat(a, ty, mutable)).collect();
                if self.locals.len() != before {
                    self.emit(Diag::error("E0271", p.span, "or-patterns cannot bind names; use separate arms"));
                }
                P::Or(ps)
            }
        }
    }

    // ------------------------------------------------------------ exhaustiveness

    fn ctors_of(&self, t: &Ty) -> Option<Vec<(Ctor, Vec<Ty>)>> {
        let c = self.core;
        match self.resolve(t) {
            Ty::Tuple(ts) => Some(vec![(Ctor::Tuple(ts.len()), ts)]),
            Ty::Named(d, args) => {
                if d == c.bool {
                    return Some(vec![(Ctor::True, vec![]), (Ctor::False, vec![])]);
                }
                if d == c.list {
                    let l = Ty::Named(d, args.clone());
                    return Some(vec![(Ctor::Nil, vec![]), (Ctor::Cons, vec![args[0].clone(), l])]);
                }
                if let Some(vs) = self.variants(d, &args) {
                    return Some(
                        vs.into_iter()
                            .enumerate()
                            .map(|(i, (_, f))| {
                                let tys = match f {
                                    VFields::Unit => vec![],
                                    VFields::Tuple(ts) => ts,
                                    VFields::Named(fs) => fs.into_iter().map(|(_, t)| t).collect(),
                                };
                                (Ctor::Variant(d, i), tys)
                            })
                            .collect(),
                    );
                }
                if let Some(fs) = self.struct_fields(d, &args) {
                    return Some(vec![(Ctor::Struct(d), fs.into_iter().map(|(_, t)| t).collect())]);
                }
                None
            }
            _ => None,
        }
    }

    fn useful(&self, rows: &[Vec<P>], q: &[P], tys: &[Ty]) -> Option<Vec<P>> {
        if q.is_empty() {
            return if rows.is_empty() { Some(vec![]) } else { None };
        }
        if let P::Or(alts) = &q[0] {
            for a in alts {
                let mut q2 = q.to_vec();
                q2[0] = a.clone();
                if let Some(w) = self.useful(rows, &q2, tys) {
                    return Some(w);
                }
            }
            return None;
        }
        let rows = expand_or(rows);
        match &q[0] {
            P::Ctor(c, args) => {
                let ftys = self.ctors_of(&tys[0]).and_then(|cs| cs.into_iter().find(|(x, _)| x == c).map(|(_, t)| t)).unwrap_or_else(|| vec![Ty::Err; args.len()]);
                let spec = specialize(&rows, c, args.len());
                let mut q2 = args.clone();
                q2.extend_from_slice(&q[1..]);
                let mut t2 = ftys;
                t2.extend_from_slice(&tys[1..]);
                self.useful(&spec, &q2, &t2).map(|w| rebuild(c, w, args.len()))
            }
            P::Lit(l) => {
                let spec: Vec<Vec<P>> = rows
                    .iter()
                    .filter(|r| matches!(&r[0], P::Wild) || matches!(&r[0], P::Lit(x) if x == l))
                    .map(|r| r[1..].to_vec())
                    .collect();
                self.useful(&spec, &q[1..], &tys[1..]).map(|mut w| {
                    w.insert(0, P::Lit(l.clone()));
                    w
                })
            }
            P::Wild => {
                let used: Vec<Ctor> = rows.iter().filter_map(|r| if let P::Ctor(c, _) = &r[0] { Some(c.clone()) } else { None }).collect();
                let all = self.ctors_of(&tys[0]);
                if let Some(all) = &all {
                    if !all.is_empty() && all.iter().all(|(c, _)| used.contains(c)) {
                        for (c, ftys) in all {
                            let n = ftys.len();
                            let spec = specialize(&rows, c, n);
                            let mut q2 = vec![P::Wild; n];
                            q2.extend_from_slice(&q[1..]);
                            let mut t2 = ftys.clone();
                            t2.extend_from_slice(&tys[1..]);
                            if let Some(w) = self.useful(&spec, &q2, &t2) {
                                return Some(rebuild(c, w, n));
                            }
                        }
                        return None;
                    }
                }
                let def: Vec<Vec<P>> = rows.iter().filter(|r| matches!(r[0], P::Wild)).map(|r| r[1..].to_vec()).collect();
                self.useful(&def, &q[1..], &tys[1..]).map(|mut w| {
                    let head = match &all {
                        Some(all) if !used.is_empty() => all
                            .iter()
                            .find(|(c, _)| !used.contains(c))
                            .map(|(c, ft)| P::Ctor(c.clone(), vec![P::Wild; ft.len()]))
                            .unwrap_or(P::Wild),
                        _ => P::Wild,
                    };
                    w.insert(0, head);
                    w
                })
            }
            P::Or(_) => unreachable!(),
        }
    }

    fn show_pat(&self, p: &P, ty: &Ty) -> String {
        match p {
            P::Wild => "_".into(),
            P::Lit(s) => s.clone(),
            P::Or(ps) => ps.iter().map(|x| self.show_pat(x, ty)).collect::<Vec<_>>().join(" | "),
            P::Ctor(c, args) => {
                let ftys = self.ctors_of(ty).and_then(|cs| cs.into_iter().find(|(x, _)| x == c).map(|(_, t)| t)).unwrap_or_default();
                let sub = |i: usize| self.show_pat(&args[i], ftys.get(i).unwrap_or(&Ty::Err));
                match c {
                    Ctor::True => "true".into(),
                    Ctor::False => "false".into(),
                    Ctor::Nil => "[]".into(),
                    Ctor::Cons => {
                        let mut items = vec![sub(0)];
                        let mut tail = &args[1];
                        loop {
                            match tail {
                                P::Ctor(Ctor::Cons, a) => {
                                    items.push(self.show_pat(&a[0], ftys.first().unwrap_or(&Ty::Err)));
                                    tail = &a[1];
                                }
                                P::Ctor(Ctor::Nil, _) => break,
                                _ => {
                                    items.push("..".into());
                                    break;
                                }
                            }
                        }
                        format!("[{}]", items.join(", "))
                    }
                    Ctor::Tuple(n) => format!("({})", (0..*n).map(sub).collect::<Vec<_>>().join(", ")),
                    Ctor::Struct(d) => format!("{} {{ .. }}", self.defs[*d].name),
                    Ctor::Variant(d, i) => {
                        let def = &self.defs[*d];
                        let DefKind::Enum(vs) = &def.kind else { return "_".into() };
                        let (vn, f) = &vs[*i];
                        let qual = if *d == self.core.option || *d == self.core.result {
                            vn.clone()
                        } else if def.module != self.cur && def.module != self.core_mod {
                            format!("{}.{}.{}", self.mods[def.module].path.rsplit('.').next().unwrap_or(""), def.name, vn)
                        } else {
                            format!("{}.{}", def.name, vn)
                        };
                        match f {
                            VFields::Unit => qual,
                            VFields::Tuple(_) => format!("{}({})", qual, (0..args.len()).map(sub).collect::<Vec<_>>().join(", ")),
                            VFields::Named(_) => format!("{} {{ .. }}", qual),
                        }
                    }
                }
            }
        }
    }

    // ============================================================ expressions

    fn expr(&mut self, e: &Expr, exp: Option<&Ty>) -> Ty {
        let t = self.expr_inner(e, exp);
        match exp {
            None => t,
            Some(x) => {
                if self.unify(&t, x) {
                    t
                } else {
                    self.mismatch(e, &t, x);
                    x.clone()
                }
            }
        }
    }

    fn mismatch(&mut self, e: &Expr, found: &Ty, expected: &Ty) {
        let (fs, es) = (self.show(found), self.show(expected));
        let mut d = Diag::error("E0201", e.span, format!("expected `{}`, found `{}`", es, fs)).expected(es).found(fs);
        let src = self.text(e.span);
        let needs_parens = matches!(e.kind, ExprKind::Binary { .. } | ExprKind::Unary { .. } | ExprKind::Range { .. } | ExprKind::Closure { .. } | ExprKind::If { .. } | ExprKind::Match { .. } | ExprKind::Spawn(_));
        let c = self.core;
        let eh = self.head(expected);
        let eargs = self.args_of(expected);
        if eh == Some(c.result) && self.probe(found, &eargs[0]) {
            d = d.with_fix(e.span, format!("Ok({})", src));
        } else if eh == Some(c.option) && self.probe(found, &eargs[0]) {
            d = d.with_fix(e.span, format!("Some({})", src));
        } else if self.is_float(expected) && self.is_int(found) {
            if matches!(e.kind, ExprKind::Int(_)) && !src.starts_with("0x") && !src.starts_with("0b") {
                d = d.with_fix(e.span, format!("{}.0", src));
            } else if needs_parens {
                d = d.with_fix(e.span, format!("({}).to_float()", src));
            } else {
                d = d.with_fix(e.span, format!("{}.to_float()", src));
            }
        } else if eh == Some(c.str) && !self.is_fn(found) && !matches!(self.resolve(found), Ty::Err | Ty::Var(_)) && !src.contains('"') && !src.contains('\n') {
            d.message.push_str("; to turn a value into text, interpolate it");
            d = d.with_fix(e.span, format!("\"{{{}}}\"", src));
        } else if self.head(found) == Some(c.result) && self.probe(&self.args_of(found)[0], expected) {
            d.message.push_str("; propagate the error with `?`");
            let ret_ok = self.rets.last().map_or(false, |r| self.head(r) == Some(c.result));
            if ret_ok {
                d = d.with_fix(Span::empty_at(e.span.hi), "?");
            }
        } else if self.head(found) == Some(c.option) && self.probe(&self.args_of(found)[0], expected) {
            d.message.push_str("; handle `None` with `?` or `.unwrap_or(default)`");
            let ret_ok = self.rets.last().map_or(false, |r| self.head(r) == Some(c.option));
            if ret_ok {
                d = d.with_fix(Span::empty_at(e.span.hi), "?");
            }
        }
        self.emit(d);
    }

    /// Flatten `a.b.c` into segments if it is a pure path; otherwise None.
    fn path_segments(e: &Expr) -> Option<Vec<Ident>> {
        match &e.kind {
            ExprKind::Name(n) => Some(vec![Ident { name: n.clone(), span: e.span }]),
            ExprKind::Field { base, name } => {
                let mut v = Self::path_segments(base)?;
                v.push(name.clone());
                Some(v)
            }
            _ => None,
        }
    }

    /// Resolve a path expression that names an item (not a field access on a value).
    fn resolve_value_path(&mut self, e: &Expr) -> Option<PathRes> {
        let segs = Self::path_segments(e)?;
        if let Some(i) = self.find_local(&segs[0].name) {
            return if segs.len() == 1 { Some(PathRes::Local(i)) } else { None };
        }
        let mut decl = match self.lookup_in(self.cur, &segs[0].name) {
            Some(d) => d,
            None => {
                self.unknown_name(&segs[0]);
                return Some(PathRes::Error);
            }
        };
        for (k, seg) in segs.iter().enumerate().skip(1) {
            decl = match decl {
                Decl::Module(m) => match self.mods[m].names.get(&seg.name).copied() {
                    Some(d) => {
                        if !self.mods[m].is_std && !self.decl_public(d) {
                            self.emit(Diag::error("E0903", seg.span, format!("`{}` is private to `{}`; mark it `pub`", seg.name, self.mods[m].path)));
                        }
                        d
                    }
                    None => {
                        let cands: Vec<String> = self.mods[m].names.keys().cloned().collect();
                        let mut d = Diag::error("E0103", seg.span, format!("`{}` has no item `{}`", self.mods[m].path, seg.name));
                        if let Some((_, to)) = VALUE_ALIASES.iter().find(|(f, _)| *f == seg.name) {
                            if cands.iter().any(|c| c == to) {
                                d = d.with_fix(seg.span, *to);
                            }
                        } else {
            d = with_suggestion(d, seg.span, suggest(&seg.name, cands.iter().map(|s| s.as_str())));
        }
                        self.emit(d);
                        return Some(PathRes::Error);
                    }
                },
                Decl::Type(d) => {
                    if let Some(i) = self.variant_index(d, &seg.name) {
                        Decl::Variant(d, i)
                    } else if let Some(f) = self.assoc.get(&(d, seg.name.clone())).copied() {
                        if k + 1 != segs.len() {
                            return None;
                        }
                        self.note(e.span, b'p', Note::Fn(f));
                        return Some(PathRes::Fn(f));
                    } else {
                        self.unknown_variant(d, seg);
                        return Some(PathRes::Error);
                    }
                }
                // a value followed by more segments: field access, handled by the caller
                _ => return None,
            };
        }
        match decl {
            Decl::Fn(f) => self.note(e.span, b'p', Note::Fn(f)),
            Decl::Const(c) => self.note(e.span, b'p', Note::Const(c)),
            Decl::Variant(d, i) => self.note(e.span, b'p', Note::Variant(d, i)),
            _ => {}
        }
        Some(match decl {
            Decl::Type(d) => PathRes::Type(d),
            Decl::Fn(f) => PathRes::Fn(f),
            Decl::Const(c) => PathRes::Const(c),
            Decl::Module(m) => PathRes::Module(m),
            Decl::Variant(d, i) => PathRes::Variant(d, i),
        })
    }

    fn unknown_name(&mut self, id: &Ident) {
        let mut d = Diag::error("E0100", id.span, format!("unknown name `{}`", id.name));
        let qualified = self.qualify_std(&id.name, false).or_else(|| if is_pascal(&id.name) { self.qualify_std(&id.name, true) } else { None });
        if let Some((_, to)) = VALUE_ALIASES.iter().find(|(f, _)| *f == id.name) {
            d.message = format!("unknown name `{}`: Drift writes `{}`", id.name, to);
            d = d.with_fix(id.span, *to);
        } else if let Some(q) = qualified {
            d.message = format!("unknown name `{}`: it lives in a std module, write `{}`", id.name, q);
            d = d.with_fix(id.span, q);
        } else if let Some((_, to)) = TYPE_ALIASES.iter().find(|(f, _)| *f == id.name) {
            d.message = format!("unknown name `{}`: Drift calls it `{}`", id.name, to);
            d = d.with_fix(id.span, *to);
        } else {
            let cands = self.visible_names();
            d = with_suggestion(d, id.span, suggest(&id.name, cands.iter().map(|s| s.as_str())));
        }
        self.emit(d);
    }

    fn instantiate_fn(&mut self, fid: usize, targs: Option<Vec<Ty>>) -> (Vec<Ty>, Vec<Param>, Ty) {
        let info = self.fns[fid].clone();
        let fresh: Vec<Ty> = match targs {
            Some(t) => t,
            None => (0..info.generics.len()).map(|_| self.fresh()).collect(),
        };
        let o = Owner::Fn(fid);
        let params = info.params.iter().map(|p| Param { name: p.name.clone(), ty: subst(&p.ty, o, &fresh), mutable: p.mutable }).collect();
        (fresh.clone(), params, subst(&info.ret, o, &fresh))
    }

    fn fn_value(&mut self, fid: usize, span: Span) -> Ty {
        let targs = if self.is_reified(fid) { Some(vec![Ty::Err]) } else { None };
        let (_, params, ret) = self.instantiate_fn(fid, targs);
        let eff = self.fns[fid].effects;
        let name = format!("`{}`", self.fns[fid].name);
        self.require_effects(eff, span, &name);
        if self.fns[fid].is_method {
            self.emit(Diag::error("E0105", span, format!("{} is a method; call it as `value.{}(...)`", name, self.fns[fid].name)));
        }
        if self.is_reified(fid) {
            self.emit(Diag::error("E0803", span, format!("{} needs its target type at the call; call it directly: `|s| json.decode[T](s)`", name)));
        }
        Ty::Fn(params.into_iter().map(|p| p.ty).collect(), Box::new(ret), eff, false)
    }

    fn path_value(&mut self, res: PathRes, e: &Expr, exp: Option<&Ty>) -> Ty {
        match res {
            PathRes::Local(i) => self.locals[i].ty.clone(),
            PathRes::Fn(f) => self.fn_value(f, e.span),
            PathRes::Const(c) => self.consts[c].ty.clone(),
            PathRes::Variant(d, i) => {
                let args = self.instance_for(d, exp);
                let vs = self.variants(d, &args).unwrap();
                let (vn, f) = vs[i].clone();
                match f {
                    VFields::Unit => Ty::Named(d, args),
                    VFields::Tuple(ts) => Ty::Fn(ts, Box::new(Ty::Named(d, args)), 0, false),
                    VFields::Named(fs) => {
                        let fields = fs.iter().map(|(n, _)| format!("{}: todo", n)).collect::<Vec<_>>().join(", ");
                        let src = self.text(e.span);
                        self.emit(
                            Diag::error("E0130", e.span, format!("`{}` has named fields; construct it with `{} {{ ... }}`", vn, src))
                                .with_fix(Span::empty_at(e.span.hi), format!(" {{ {} }}", fields)),
                        );
                        Ty::Named(d, args)
                    }
                }
            }
            PathRes::Type(d) => {
                let name = self.defs[d].name.clone();
                self.emit(Diag::error("E0101", e.span, format!("`{}` is a type, not a value", name)));
                Ty::Err
            }
            PathRes::Module(m) => {
                let p = self.mods[m].path.clone();
                self.emit(Diag::error("E0101", e.span, format!("`{}` is a module, not a value; write `{}.name`", p, self.text(e.span))));
                Ty::Err
            }
            PathRes::Error => Ty::Err,
        }
    }

    fn expr_inner(&mut self, e: &Expr, exp: Option<&Ty>) -> Ty {
        let c = self.core;
        match &e.kind {
            ExprKind::Int(_) => match exp {
                Some(x) if self.is_int(x) => self.resolve(x),
                _ => self.t_int(),
            },
            ExprKind::Float(_) => match exp {
                Some(x) if self.head(x) == Some(c.f32) => self.resolve(x),
                _ => Ty::Named(c.float, vec![]),
            },
            ExprKind::Bool(_) => self.t_bool(),
            ExprKind::Str(parts) => {
                for p in parts {
                    if let StrPart::Expr(x) = p {
                        let t = self.expr(x, None);
                        if self.is_fn(&t) {
                            self.emit(Diag::error("E0207", x.span, "functions cannot be interpolated into text"));
                        }
                    }
                }
                self.t_str()
            }
            ExprKind::RawStr(_) => self.t_str(),
            ExprKind::Bytes(_) => Ty::Named(c.bytes, vec![]),
            ExprKind::Name(_) | ExprKind::Field { .. } => {
                if let Some(res) = self.resolve_value_path(e) {
                    return self.path_value(res, e, exp);
                }
                let ExprKind::Field { base, name } = &e.kind else { return Ty::Err };
                let bt = self.expr(base, None);
                self.field_access(&bt, name, e)
            }
            ExprKind::Tuple(items) => {
                let hints: Vec<Option<Ty>> = match exp.map(|x| self.resolve(x)) {
                    Some(Ty::Tuple(ts)) if ts.len() == items.len() => ts.into_iter().map(Some).collect(),
                    _ => vec![None; items.len()],
                };
                Ty::Tuple(items.iter().zip(hints.iter()).map(|(x, h)| self.expr(x, h.as_ref())).collect())
            }
            ExprKind::List(items) => {
                let elem = match exp.map(|x| self.resolve(x)) {
                    Some(Ty::Named(d, a)) if d == c.list => a[0].clone(),
                    _ => self.fresh(),
                };
                for x in items {
                    self.expr(x, Some(&elem));
                }
                Ty::Named(c.list, vec![elem])
            }
            ExprKind::Map(entries) => {
                let (k, v) = match exp.map(|x| self.resolve(x)) {
                    Some(Ty::Named(d, a)) if d == c.map => (a[0].clone(), a[1].clone()),
                    _ => (self.fresh(), self.fresh()),
                };
                for (a, b) in entries {
                    self.expr(a, Some(&k));
                    self.expr(b, Some(&v));
                }
                Ty::Named(c.map, vec![k, v])
            }
            ExprKind::StructLit { path, fields } => self.struct_lit(e, path, fields, exp),
            ExprKind::With { base, fields } => {
                let bt = self.expr(base, exp);
                let Some(d) = self.head(&bt) else { return bt };
                let args = self.args_of(&bt);
                let Some(fs) = self.struct_fields(d, &args) else {
                    let shown = self.show(&bt);
                    self.emit(Diag::error("E0131", base.span, format!("`with` updates a struct; `{}` is not one", shown)));
                    return bt;
                };
                self.note(e.span, b's', Note::With(d));
                for (n, v) in fields {
                    match fs.iter().find(|(f, _)| *f == n.name) {
                        Some((_, ft)) => {
                            let ft = ft.clone();
                            self.expr(v, Some(&ft));
                        }
                        None => {
                            self.unknown_field(d, &fs, n);
                            self.expr(v, None);
                        }
                    }
                }
                bt
            }
            ExprKind::Index { base, index } => {
                let bt = self.expr(base, None);
                let args = self.args_of(&bt);
                match self.head(&bt) {
                    Some(d) if d == c.list => {
                        let i = self.t_int();
                        self.expr(index, Some(&i));
                        args[0].clone()
                    }
                    Some(d) if d == c.map => {
                        self.expr(index, Some(&args[0]));
                        args[1].clone()
                    }
                    Some(d) if d == c.bytes => {
                        let i = self.t_int();
                        self.expr(index, Some(&i));
                        Ty::Named(c.u8, vec![])
                    }
                    _ => {
                        if !matches!(self.resolve(&bt), Ty::Err) {
                            let shown = self.show(&bt);
                            let mut d = Diag::error("E0212", e.span, format!("`{}` cannot be indexed", shown));
                            if self.head(&bt) == Some(c.str) {
                                d.message.push_str("; use `.slice(a, b)` or `.chars()`");
                            }
                            self.emit(d);
                        }
                        self.expr(index, None);
                        Ty::Err
                    }
                }
            }
            ExprKind::Call { callee, args } => self.call(e, callee, args, exp),
            ExprKind::Unary { op, expr } => match op {
                UnOp::Not => {
                    let b = self.t_bool();
                    self.expr(expr, Some(&b));
                    b
                }
                UnOp::Neg => {
                    let t = self.expr(expr, exp);
                    let signed = self.head(&t).map_or(false, |d| d == c.int || d == c.i32 || d == c.float || d == c.f32);
                    if !signed && !matches!(self.resolve(&t), Ty::Err | Ty::Var(_)) {
                        let shown = self.show(&t);
                        self.emit(Diag::error("E0211", e.span, format!("`-` is not defined for `{}`", shown)));
                    }
                    t
                }
            },
            ExprKind::Binary { op, lhs, rhs } => self.binary(e, *op, lhs, rhs),
            ExprKind::Range { lo, hi, .. } => {
                let i = self.t_int();
                self.expr(lo, Some(&i));
                self.expr(hi, Some(&i));
                Ty::Named(c.range, vec![])
            }
            ExprKind::Try(inner) => self.try_expr(e, inner),
            ExprKind::If { cond, then, els } => {
                let b = self.t_bool();
                self.expr(cond, Some(&b));
                match els {
                    None => {
                        self.block(then, None, true);
                        if let Some(x) = exp {
                            if !self.probe(&unit(), x) {
                                let shown = self.show(x);
                                self.emit(
                                    Diag::error("E0230", e.span, format!("`if` without `else` has no value, but `{}` is expected here", shown))
                                        .with_fix(Span::empty_at(then.span.hi), " else { todo }"),
                                );
                                return x.clone();
                            }
                        }
                        unit()
                    }
                    Some(x) => {
                        let target = match exp {
                            Some(t) => t.clone(),
                            None => self.fresh(),
                        };
                        let t1 = self.block(then, Some(&target), false);
                        let t2 = self.expr(x, Some(&target));
                        if matches!(t1, Ty::Never) && matches!(t2, Ty::Never) {
                            return Ty::Never;
                        }
                        target
                    }
                }
            }
            ExprKind::Match { scrutinee, arms } => self.match_expr(e, scrutinee, arms, exp),
            ExprKind::Closure { params, ret, body } => {
                let expected_fn = exp.and_then(|x| match self.resolve(x) {
                    Ty::Fn(p, r, _, _) => Some((p, *r)),
                    _ => None,
                });
                if let Some((ps, _)) = &expected_fn {
                    if ps.len() != params.len() {
                        self.emit(Diag::error("E0203", e.span, format!("this closure takes {} parameter(s), but {} are expected here", params.len(), ps.len())));
                    }
                }
                self.push_scope();
                self.closure_depth += 1;
                let saved_loop = self.loop_depth;
                self.loop_depth = 0;
                let mut ptys = Vec::new();
                for (i, (pat, ann)) in params.iter().enumerate() {
                    let hinted = expected_fn.as_ref().and_then(|(ps, _)| ps.get(i).cloned());
                    let t = match (ann, hinted) {
                        (Some(a), h) => {
                            let t = self.lower_ty(a);
                            if let Some(h) = h {
                                if !self.unify(&t, &h) {
                                    let (a1, b1) = (self.show(&h), self.show(&t));
                                    self.emit(Diag::error("E0201", a.span, format!("expected parameter of type `{}`, found `{}`", a1, b1)).expected(a1).found(b1));
                                }
                            }
                            t
                        }
                        (None, Some(h)) => h,
                        (None, None) => self.fresh(),
                    };
                    let p = self.bind_pat(pat, &t, false);
                    self.require_irrefutable(&p, &t, pat.span, "closure parameter");
                    ptys.push(t);
                }
                let r = match (ret, &expected_fn) {
                    (Some(a), _) => self.lower_ty(a),
                    (None, Some((_, r))) => r.clone(),
                    (None, None) => self.fresh(),
                };
                self.rets.push(r.clone());
                self.expr(body, Some(&r));
                self.rets.pop();
                self.loop_depth = saved_loop;
                self.closure_depth -= 1;
                self.pop_scope();
                Ty::Fn(ptys, Box::new(r), 0, true)
            }
            ExprKind::Block(b) => self.block(b, exp, false),
            ExprKind::Spawn(x) => {
                self.require_effects(1 << 8, e.span, "`spawn`");
                let t = self.expr(x, None);
                Ty::Named(c.task, vec![t])
            }
            ExprKind::Return(v) => {
                let r = self.rets.last().cloned().unwrap_or(Ty::Err);
                match v {
                    Some(x) => {
                        self.expr(x, Some(&r));
                    }
                    None => {
                        if !self.unify(&unit(), &r) {
                            let shown = self.show(&r);
                            self.emit(Diag::error("E0201", e.span, format!("`return` needs a value of type `{}`", shown)).expected(shown));
                        }
                    }
                }
                Ty::Never
            }
            ExprKind::Break | ExprKind::Continue => {
                if self.loop_depth == 0 {
                    self.emit(Diag::error("E0240", e.span, "`break`/`continue` outside of a loop"));
                }
                Ty::Never
            }
            ExprKind::Todo => {
                let v = self.fresh();
                let scope = self.locals.iter().map(|l| (l.name.clone(), l.ty.clone())).collect();
                self.holes.push((e.span, v.clone(), scope));
                v
            }
        }
    }

    fn unknown_field(&mut self, d: usize, fs: &[(String, Ty)], n: &Ident) {
        let name = self.defs[d].name.clone();
        let mut diag = Diag::error("E0311", n.span, format!("`{}` has no field `{}`", name, n.name));
        let mut cands: Vec<String> = fs.iter().map(|(x, _)| x.clone()).collect();
        cands.extend(self.methods.keys().filter(|(x, _)| *x == d).map(|(_, m)| m.clone()));
        let sg = suggest(&n.name, cands.iter().map(|s| s.as_str()));
        if sg.is_none() && !fs.is_empty() {
            diag.message.push_str(&format!(" (fields: {})", fs.iter().map(|(x, _)| x.as_str()).collect::<Vec<_>>().join(", ")));
        }
        diag = with_suggestion(diag, n.span, sg);
        self.emit(diag);
    }

    fn field_access(&mut self, bt: &Ty, name: &Ident, e: &Expr) -> Ty {
        let rt = self.resolve(bt);
        match &rt {
            Ty::Err => return Ty::Err,
            Ty::Var(_) => {
                self.emit(Diag::error("E0802", e.span, format!("the type of this value must be known before reading `.{}`; annotate the binding", name.name)));
                return Ty::Err;
            }
            _ => {}
        }
        let Some(d) = self.head(&rt) else {
            let shown = self.show(&rt);
            self.emit(Diag::error("E0311", name.span, format!("`{}` has no fields; destructure tuples with `let (a, b) = value`", shown)));
            return Ty::Err;
        };
        let args = self.args_of(&rt);
        if let Some(fs) = self.struct_fields(d, &args) {
            if let Some(i) = fs.iter().position(|(f, _)| *f == name.name) {
                self.note(e.span, b'p', Note::Field(i));
                return fs[i].1.clone();
            }
            if self.methods.contains_key(&(d, name.name.clone())) {
                let mut diag = Diag::error("E0313", name.span, format!("`{}` is a method; call it", name.name));
                let fid = self.methods[&(d, name.name.clone())];
                if self.fns[fid].params.len() == 1 {
                    diag = diag.with_fix(Span::empty_at(name.span.hi), "()");
                }
                self.emit(diag);
                return Ty::Err;
            }
            self.unknown_field(d, &fs, name);
            return Ty::Err;
        }
        if self.methods.contains_key(&(d, name.name.clone())) {
            let fid = self.methods[&(d, name.name.clone())];
            let mut diag = Diag::error("E0313", name.span, format!("`{}` is a method; call it", name.name));
            if self.fns[fid].params.len() == 1 {
                diag = diag.with_fix(Span::empty_at(name.span.hi), "()");
            }
            self.emit(diag);
            return Ty::Err;
        }
        let shown = self.show(&rt);
        let mut diag = Diag::error("E0311", name.span, format!("`{}` has no field `{}`", shown, name.name));
        if matches!(self.defs[d].kind, DefKind::Enum(_)) {
            diag.message.push_str("; enums are read with `match`");
        }
        self.emit(diag);
        Ty::Err
    }

    fn struct_lit(&mut self, e: &Expr, path: &Expr, fields: &[(Ident, Option<Expr>)], exp: Option<&Ty>) -> Ty {
        let Some(segs) = Self::path_segments(path) else { return Ty::Err };
        let Some((d, vi)) = self.resolve_variant_path(&segs) else {
            for (_, v) in fields {
                if let Some(v) = v {
                    self.expr(v, None);
                }
            }
            return Ty::Err;
        };
        let args = self.instance_for(d, exp);
        let fs: Vec<(String, Ty)> = match vi {
            Some(i) => match self.variants(d, &args).unwrap()[i].1.clone() {
                VFields::Named(fs) => fs,
                VFields::Unit => {
                    let src = self.text(path.span);
                    self.emit(Diag::error("E0131", e.span, format!("`{}` has no fields; write `{}`", src, src)).with_fix(e.span, src));
                    return Ty::Named(d, args);
                }
                VFields::Tuple(_) => {
                    self.emit(Diag::error("E0131", e.span, format!("`{}` takes positional values: `{}(...)`", self.text(path.span), self.text(path.span))));
                    return Ty::Named(d, args);
                }
            },
            None => match self.struct_fields(d, &args) {
                Some(fs) => fs,
                None => {
                    let name = self.defs[d].name.clone();
                    self.emit(Diag::error("E0131", e.span, format!("`{}` cannot be built with a struct literal", name)));
                    return Ty::Err;
                }
            },
        };
        let mut seen: Vec<String> = Vec::new();
        for (n, v) in fields {
            if seen.contains(&n.name) {
                self.emit(Diag::error("E0314", n.span, format!("field `{}` is given twice", n.name)));
            }
            seen.push(n.name.clone());
            let ft = fs.iter().find(|(f, _)| *f == n.name).map(|(_, t)| t.clone());
            match ft {
                Some(ft) => match v {
                    Some(v) => {
                        self.expr(v, Some(&ft));
                    }
                    None => {
                        let synthetic = Expr { kind: ExprKind::Name(n.name.clone()), span: n.span };
                        self.expr(&synthetic, Some(&ft));
                    }
                },
                None => {
                    self.unknown_field(d, &fs, n);
                    if let Some(v) = v {
                        self.expr(v, None);
                    }
                }
            }
        }
        let missing: Vec<&String> = fs.iter().map(|(n, _)| n).filter(|n| !seen.contains(n)).collect();
        if !missing.is_empty() {
            let list = missing.iter().map(|m| format!("{}: todo", m)).collect::<Vec<_>>().join(", ");
            let (at, text) = match fields.last() {
                Some((n, v)) => (v.as_ref().map_or(n.span.hi, |v| v.span.hi), format!(", {}", list)),
                None => (e.span.hi - 1, format!("{} ", list)),
            };
            self.emit(
                Diag::error("E0310", e.span, format!("missing field(s) {}", missing.iter().map(|m| format!("`{}`", m)).collect::<Vec<_>>().join(", ")))
                    .with_fix(Span::empty_at(at), text),
            );
        }
        self.note(e.span, b's', Note::StructLit(d, vi));
        Ty::Named(d, args)
    }

    fn binary(&mut self, e: &Expr, op: BinOp, lhs: &Expr, rhs: &Expr) -> Ty {
        let c = self.core;
        if matches!(op, BinOp::And | BinOp::Or) {
            let b = self.t_bool();
            self.expr(lhs, Some(&b));
            self.expr(rhs, Some(&b));
            return b;
        }
        let lit = |x: &Expr| match &x.kind {
            ExprKind::Int(_) | ExprKind::Float(_) => true,
            ExprKind::Unary { op: UnOp::Neg, expr } => matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_)),
            _ => false,
        };
        let lt = if lit(lhs) && !lit(rhs) {
            let rt = self.expr(rhs, None);
            self.expr(lhs, Some(&rt))
        } else {
            let lt = self.expr(lhs, None);
            self.expr(rhs, Some(&lt));
            lt
        };
        let rl = self.resolve(&lt);
        if matches!(rl, Ty::Err | Ty::Never) {
            return if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) { self.t_bool() } else { Ty::Err };
        }
        let sym = match op {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            _ => "",
        };
        match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if self.is_fn(&rl) {
                    self.emit(Diag::error("E0211", e.span, "functions cannot be compared"));
                }
                self.t_bool()
            }
            _ => {
                let ok = self.is_numeric(&rl)
                    || matches!(rl, Ty::Var(_))
                    || (op == BinOp::Add && matches!(self.head(&rl), Some(d) if d == c.str || d == c.bytes || d == c.list));
                if !ok {
                    let shown = self.show(&rl);
                    self.emit(Diag::error("E0211", e.span, format!("`{}` is not defined for `{}`", sym, shown)));
                }
                lt
            }
        }
    }

    fn try_expr(&mut self, e: &Expr, inner: &Expr) -> Ty {
        let c = self.core;
        let t = self.expr(inner, None);
        let rt = self.resolve(&t);
        let ret = self.rets.last().cloned().unwrap_or(Ty::Err);
        let rr = self.resolve(&ret);
        if matches!(rt, Ty::Err) || matches!(rr, Ty::Err) {
            return match self.head(&rt) {
                Some(h) if h == c.result || h == c.option => self.args_of(&rt)[0].clone(),
                _ => Ty::Err,
            };
        }
        let qspan = Span::new(e.span.hi as usize - 1, e.span.hi as usize);
        self.note(e.span, b't', Note::TrySame);
        match self.head(&rt) {
            Some(h) if h == c.result => {
                let a = self.args_of(&rt);
                let (ok, err) = (a[0].clone(), a[1].clone());
                match &rr {
                    Ty::Var(_) => {
                        let f = self.fresh();
                        self.unify(&rr, &Ty::Named(c.result, vec![f, err]));
                    }
                    _ if self.head(&rr) == Some(c.result) => {
                        let target = self.args_of(&rr)[1].clone();
                        let conv = self.conversion(&err, &target);
                        if let Some(cv) = &conv {
                            self.note(e.span, b't', cv.clone());
                        }
                        if conv.is_none() {
                            let (fe, te) = (self.show(&err), self.show(&target));
                            self.emit(
                                Diag::error("E0402", qspan, format!("`?` cannot turn `{}` into `{}`; add a variant `{}({})` to the error enum, or use `.map_err(...)`", fe, te, self.defs[self.head(&err).unwrap_or(c.error)].name, fe))
                                    .expected(te)
                                    .found(fe),
                            );
                        }
                    }
                    _ => {
                        let shown = self.show(&rr);
                        self.emit(Diag::error("E0401", qspan, format!("`?` needs the enclosing function to return `Result`, but it returns `{}`", shown)));
                    }
                }
                ok
            }
            Some(h) if h == c.option => {
                let ok = self.args_of(&rt)[0].clone();
                match &rr {
                    Ty::Var(_) => {
                        let f = self.fresh();
                        self.unify(&rr, &Ty::Named(c.option, vec![f]));
                    }
                    _ if self.head(&rr) == Some(c.option) => {}
                    _ if self.head(&rr) == Some(c.result) => {
                        self.emit(
                            Diag::error("E0403", qspan, "`?` on an `Option` inside a function returning `Result`: say which error `None` becomes")
                                .with_fix(Span::empty_at(qspan.lo), ".ok_or(todo)"),
                        );
                    }
                    _ => {
                        let shown = self.show(&rr);
                        self.emit(Diag::error("E0401", qspan, format!("`?` needs the enclosing function to return `Option`, but it returns `{}`", shown)));
                    }
                }
                ok
            }
            _ => {
                let shown = self.show(&rt);
                self.emit(Diag::error("E0404", qspan, format!("`?` applies to `Result` or `Option`, not `{}`", shown)).with_fix(qspan, ""));
                rt
            }
        }
    }

    /// How `?` turns an error of type `from` into `to`, if it can.
    fn conversion(&mut self, from: &Ty, to: &Ty) -> Option<Note> {
        if self.probe(from, to) {
            self.unify(from, to);
            return Some(Note::TrySame);
        }
        let d = self.head(to)?;
        if d == self.core.error {
            return Some(Note::TryToError);
        }
        let args = self.args_of(to);
        let vs = self.variants(d, &args)?;
        let mut hits = Vec::new();
        for (i, (_, f)) in vs.iter().enumerate() {
            if let VFields::Tuple(ts) = f {
                if ts.len() == 1 && self.probe(from, &ts[0]) {
                    hits.push(i);
                }
            }
        }
        if hits.len() == 1 { Some(Note::TryWrap(d, hits[0])) } else { None }
    }

    #[allow(dead_code)]
    fn error_converts(&mut self, from: &Ty, to: &Ty) -> bool {
        if self.probe(from, to) {
            self.unify(from, to);
            return true;
        }
        let Some(d) = self.head(to) else { return false };
        if d == self.core.error {
            return true;
        }
        let args = self.args_of(to);
        let Some(vs) = self.variants(d, &args) else { return false };
        let wrapping: Vec<Ty> = vs
            .into_iter()
            .filter_map(|(_, f)| match f {
                VFields::Tuple(ts) if ts.len() == 1 => Some(ts[0].clone()),
                _ => None,
            })
            .filter(|t| self.probe(from, t))
            .collect();
        wrapping.len() == 1
    }

    fn match_expr(&mut self, e: &Expr, scrutinee: &Expr, arms: &[Arm], exp: Option<&Ty>) -> Ty {
        let st = self.expr(scrutinee, None);
        let target = match exp {
            Some(t) => t.clone(),
            None => self.fresh(),
        };
        let mut rows: Vec<Vec<P>> = Vec::new();
        let mut all_never = !arms.is_empty();
        for a in arms {
            self.push_scope();
            let p = self.bind_pat(&a.pat, &st, false);
            if let Some(g) = &a.guard {
                let b = self.t_bool();
                self.expr(g, Some(&b));
            }
            let bt = self.expr(&a.body, Some(&target));
            if !matches!(bt, Ty::Never) {
                all_never = false;
            }
            self.pop_scope();
            let stz = self.zonk(&st);
            if self.useful(&rows, &[p.clone()], &[stz]).is_none() {
                self.emit(Diag::warning("W0101", a.pat.span, "this arm is unreachable: earlier arms already match everything it matches"));
            }
            if a.guard.is_none() {
                rows.push(vec![p]);
            }
        }
        let stz = self.zonk(&st);
        if !matches!(stz, Ty::Err) {
            let mut missing = Vec::new();
            let mut probe_rows = rows.clone();
            while missing.len() < 8 {
                match self.useful(&probe_rows, &[P::Wild], &[stz.clone()]) {
                    Some(w) => {
                        missing.push(self.show_pat(&w[0], &stz));
                        if matches!(w[0], P::Wild) {
                            break;
                        }
                        probe_rows.push(w);
                    }
                    None => break,
                }
            }
            if !missing.is_empty() {
                let at = e.span.hi - 1;
                let text: String = missing.iter().map(|m| format!("{} => todo\n", m)).collect();
                self.emit(
                    Diag::error("E0280", scrutinee.span, format!("`match` does not cover {}", missing.iter().map(|m| format!("`{}`", m)).collect::<Vec<_>>().join(", ")))
                        .with_fix(Span::empty_at(at), text),
                );
            }
        }
        if all_never {
            return Ty::Never;
        }
        target
    }

    // ------------------------------------------------------------ calls

    fn call(&mut self, e: &Expr, callee: &Expr, args: &[Expr], exp: Option<&Ty>) -> Ty {
        // explicit type arguments: f[T](...)
        if let ExprKind::Index { base, index } = &callee.kind {
            if let Some(PathRes::Fn(f)) = self.peek_path(base) {
                if let Some(ty_ast) = expr_as_type(index) {
                    let t = self.lower_ty(&ty_ast);
                    let n = self.fns[f].generics.len();
                    let targs = if n == 1 { Some(vec![t]) } else { None };
                    return self.call_fn(f, None, args, e, targs, exp);
                }
            }
        }
        // free call of a method name: `len(xs)` → `xs.len()`
        if let ExprKind::Name(n) = &callee.kind {
            if self.find_local(n).is_none() && self.lookup_in(self.cur, n).is_none() && self.method_names.contains(n) && !args.is_empty() {
                let recv = self.text(args[0].span);
                let rest: Vec<String> = args[1..].iter().map(|a| self.text(a.span)).collect();
                let recv = if matches!(args[0].kind, ExprKind::Binary { .. } | ExprKind::Unary { .. } | ExprKind::Range { .. }) { format!("({})", recv) } else { recv };
                self.emit(
                    Diag::error("E0105", callee.span, format!("`{}` is a method: call it as `x.{}(...)`", n, n))
                        .with_fix(e.span, format!("{}.{}({})", recv, n, rest.join(", "))),
                );
                for a in args {
                    self.expr(a, None);
                }
                return Ty::Err;
            }
        }
        if let Some(res) = self.resolve_value_path(callee) {
            return match res {
                PathRes::Fn(f) => {
                    if self.fns[f].is_method {
                        self.fn_value(f, callee.span);
                        return Ty::Err;
                    }
                    self.call_fn(f, None, args, e, None, exp)
                }
                PathRes::Variant(d, i) => self.construct(e, callee, d, i, args, exp),
                other => {
                    let ft = self.path_value(other, callee, None);
                    self.call_value(e, &ft, args)
                }
            };
        }
        if let ExprKind::Field { base, name } = &callee.kind {
            let bt = self.expr(base, None);
            return self.method_call(e, base, &bt, name, args, exp);
        }
        let ft = self.expr(callee, None);
        self.call_value(e, &ft, args)
    }

    /// Resolve a path without reporting errors.
    fn peek_path(&mut self, e: &Expr) -> Option<PathRes> {
        let n = self.diags.len();
        let r = self.resolve_value_path(e);
        self.diags.truncate(n);
        r
    }

    fn call_value(&mut self, e: &Expr, ft: &Ty, args: &[Expr]) -> Ty {
        match self.resolve(ft) {
            Ty::Fn(ps, r, eff, _) => {
                if ps.len() != args.len() {
                    self.emit(Diag::error("E0203", e.span, format!("expected {} argument(s), found {}", ps.len(), args.len())));
                }
                for (a, p) in args.iter().zip(ps.iter()) {
                    self.expr(a, Some(p));
                }
                for a in args.iter().skip(ps.len()) {
                    self.expr(a, None);
                }
                self.require_effects(eff, e.span, "this call");
                *r
            }
            Ty::Err => {
                for a in args {
                    self.expr(a, None);
                }
                Ty::Err
            }
            Ty::Var(_) => {
                let ps: Vec<Ty> = args.iter().map(|a| self.expr(a, None)).collect();
                let r = self.fresh();
                self.unify(ft, &Ty::Fn(ps, Box::new(r.clone()), 0, true));
                r
            }
            other => {
                let shown = self.show(&other);
                self.emit(Diag::error("E0204", e.span, format!("`{}` is not a function", shown)));
                Ty::Err
            }
        }
    }

    fn construct(&mut self, e: &Expr, callee: &Expr, d: usize, i: usize, args: &[Expr], exp: Option<&Ty>) -> Ty {
        let targs = self.instance_for(d, exp);
        let (vn, f) = self.variants(d, &targs).unwrap()[i].clone();
        match f {
            VFields::Tuple(ts) => {
                if ts.len() != args.len() {
                    self.emit(Diag::error("E0203", e.span, format!("`{}` takes {} value(s), found {}", vn, ts.len(), args.len())));
                }
                for (a, t) in args.iter().zip(ts.iter()) {
                    self.expr(a, Some(t));
                }
            }
            VFields::Unit => {
                let src = self.text(callee.span);
                self.emit(Diag::error("E0203", e.span, format!("`{}` takes no values; write `{}`", vn, src)).with_fix(e.span, src));
            }
            VFields::Named(_) => {
                self.emit(Diag::error("E0130", e.span, format!("`{}` has named fields; use `{} {{ field: value }}`", vn, self.text(callee.span))));
            }
        }
        Ty::Named(d, targs)
    }

    fn is_reified(&self, fid: usize) -> bool {
        let f = &self.fns[fid];
        f.name == "decode" && self.mods[f.module].is_std && self.mods[f.module].path == "std.json"
    }

    fn call_fn(&mut self, fid: usize, recv: Option<(&Expr, Ty)>, args: &[Expr], e: &Expr, targs: Option<Vec<Ty>>, exp: Option<&Ty>) -> Ty {
        let (targs, mut params, ret) = self.instantiate_fn(fid, targs);
        if self.is_reified(fid) && !self.mods[self.cur].is_std {
            self.reified.push((e.span, targs[0].clone()));
        }
        let name = self.fns[fid].name.clone();
        if let Some((rexpr, rty)) = recv {
            let p0 = params.remove(0);
            if !self.unify(&rty, &p0.ty) {
                let (a, b) = (self.show(&p0.ty), self.show(&rty));
                self.emit(Diag::error("E0201", rexpr.span, format!("`.{}` expects a receiver of type `{}`, found `{}`", name, a, b)).expected(a).found(b));
            }
            if p0.mutable {
                self.mark_mutation(rexpr);
            }
        }
        if let Some(x) = exp {
            if self.probe(&ret, x) {
                self.unify(&ret, x);
            }
        }
        if params.len() != args.len() {
            let sig = params.iter().map(|p| format!("{}: {}", p.name, self.show(&p.ty))).collect::<Vec<_>>().join(", ");
            self.emit(Diag::error("E0203", e.span, format!("`{}` takes {} argument(s), found {}: `{}({})`", name, params.len(), args.len(), name, sig)));
        }
        for (a, p) in args.iter().zip(params.iter()) {
            self.expr(a, Some(&p.ty));
        }
        for a in args.iter().skip(params.len()) {
            self.expr(a, None);
        }
        let eff = self.fns[fid].effects;
        self.require_effects(eff, e.span, &format!("`{}`", name));
        ret
    }

    fn method_call(&mut self, e: &Expr, base: &Expr, bt: &Ty, name: &Ident, args: &[Expr], exp: Option<&Ty>) -> Ty {
        let rt = self.resolve(bt);
        match &rt {
            Ty::Err => {
                for a in args {
                    self.expr(a, None);
                }
                return Ty::Err;
            }
            Ty::Var(_) => {
                self.emit(Diag::error("E0802", base.span, format!("the type of this value must be known before calling `.{}`; annotate the binding", name.name)));
                for a in args {
                    self.expr(a, None);
                }
                return Ty::Err;
            }
            _ => {}
        }
        let Some(d) = self.head(&rt) else {
            let shown = self.show(&rt);
            self.emit(Diag::error("E0312", name.span, format!("`{}` has no methods", shown)));
            return Ty::Err;
        };
        if let Some(fid) = self.methods.get(&(d, name.name.clone())).copied() {
            self.note(e.span, b'c', Note::Method(fid));
            return self.call_fn(fid, Some((base, rt.clone())), args, e, None, exp);
        }
        // a struct field holding a function
        let targs = self.args_of(&rt);
        if let Some(fs) = self.struct_fields(d, &targs) {
            if let Some(i) = fs.iter().position(|(f, _)| *f == name.name) {
                let ft = fs[i].1.clone();
                self.note(e.span, b'c', Note::FieldFn(i));
                return self.call_value(e, &ft, args);
            }
        }
        let shown = self.show(&rt);
        let mut diag = Diag::error("E0312", name.span, format!("`{}` has no method `{}`", shown, name.name));
        let cands: Vec<String> = self.methods.keys().filter(|(x, _)| *x == d).map(|(_, m)| m.clone()).collect();
        let c = self.core;
        if (name.name == "unwrap" || name.name == "expect") && (d == c.option || d == c.result) {
            diag.message = format!("Drift has no `{}`: propagate with `?` or give a default with `.unwrap_or(value)`", name.name);
            let ret_head = self.rets.last().and_then(|r| self.head(r));
            let compatible = ret_head == Some(d) || (d == c.result && ret_head == Some(c.result));
            let call = Span::new(name.span.lo as usize - 1, e.span.hi as usize);
            if compatible {
                diag = diag.with_fix(call, "?");
            } else if d == c.option && ret_head == Some(c.result) {
                // the missing decision (which error?) becomes a typed hole
                diag = diag.with_fix(call, ".ok_or(todo)?");
            }
        } else if let Some((_, to)) = METHOD_ALIASES.iter().find(|(f, _)| *f == name.name) {
            if cands.iter().any(|x| x == to) {
                diag.message.push_str(&format!("; Drift calls it `{}`", to));
                diag = diag.with_fix(name.span, *to);
            } else if to.starts_with('(') {
                diag.message.push_str(&format!(" {}", to));
            }
        } else {
            diag = with_suggestion(diag, name.span, suggest(&name.name, cands.iter().map(|s| s.as_str())));
        }
        self.emit(diag);
        for a in args {
            self.expr(a, None);
        }
        Ty::Err
    }
}

fn expand_or(rows: &[Vec<P>]) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    for r in rows {
        match &r[0] {
            P::Or(alts) => {
                for a in alts {
                    let mut x = r.clone();
                    x[0] = a.clone();
                    out.extend(expand_or(&[x]));
                }
            }
            _ => out.push(r.clone()),
        }
    }
    out
}

fn specialize(rows: &[Vec<P>], c: &Ctor, n: usize) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    for r in rows {
        match &r[0] {
            P::Ctor(c2, args) if c2 == c => {
                let mut x = args.clone();
                x.extend_from_slice(&r[1..]);
                out.push(x);
            }
            P::Wild => {
                let mut x = vec![P::Wild; n];
                x.extend_from_slice(&r[1..]);
                out.push(x);
            }
            _ => {}
        }
    }
    out
}

fn rebuild(c: &Ctor, mut w: Vec<P>, n: usize) -> Vec<P> {
    let rest = w.split_off(n.min(w.len()));
    let mut out = vec![P::Ctor(c.clone(), w)];
    out.extend(rest);
    out
}

fn contains_break(stmts: &[Stmt]) -> bool {
    fn e(x: &Expr) -> bool {
        match &x.kind {
            ExprKind::Break => true,
            ExprKind::If { cond, then, els } => e(cond) || b(&then.stmts) || els.as_ref().map_or(false, |x| e(x)),
            ExprKind::Match { scrutinee, arms } => e(scrutinee) || arms.iter().any(|a| e(&a.body)),
            ExprKind::Block(bl) => b(&bl.stmts),
            _ => false,
        }
    }
    fn b(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| match &s.kind {
            StmtKind::Expr(x) => e(x),
            StmtKind::Let { value, .. } => e(value),
            StmtKind::Assert { .. } | StmtKind::Assign { .. } => false,
            // nested loops own their breaks
            StmtKind::For { .. } | StmtKind::While { .. } | StmtKind::Loop { .. } => false,
        })
    }
    b(stmts)
}

/// `Int`, `List[Int]`, `http.Request` written in expression position (explicit type arguments).
fn expr_as_type(e: &Expr) -> Option<Type> {
    match &e.kind {
        ExprKind::Name(n) => Some(Type { kind: TypeKind::Named { path: vec![Ident { name: n.clone(), span: e.span }], args: vec![] }, span: e.span }),
        ExprKind::Field { .. } => {
            let segs = Checker::path_segments(e)?;
            Some(Type { kind: TypeKind::Named { path: segs, args: vec![] }, span: e.span })
        }
        ExprKind::Index { base, index } => {
            let mut t = expr_as_type(base)?;
            let a = expr_as_type(index)?;
            if let TypeKind::Named { args, .. } = &mut t.kind {
                args.push(a);
            }
            t.span = e.span;
            Some(t)
        }
        _ => None,
    }
}
