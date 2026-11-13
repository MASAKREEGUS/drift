//! Canonical formatter. Layout depends only on the AST and a fixed width, never on the original
//! layout, except that comments are kept and a single blank line between statements is preserved.
//! Invariants (tested): output re-parses to the same AST, and fmt(fmt(x)) == fmt(x).

use crate::ast::*;
use crate::diag::{Diag, Edit, Span};
use crate::lexer::Comment;
use crate::parser::parse;

pub const WIDTH: usize = 100;

// ---------------- document algebra ----------------

#[derive(Clone, Debug)]
enum Doc {
    Text(String),
    /// space when flat, newline when broken
    Line,
    /// nothing when flat, newline when broken
    SoftLine,
    HardLine,
    Nest(Box<Doc>),
    Group(Box<Doc>),
    Cat(Vec<Doc>),
    /// text only when the enclosing group is broken
    IfBreak(&'static str),
    /// text only when the enclosing group is flat
    IfFlat(&'static str),
}

fn t(s: impl Into<String>) -> Doc {
    Doc::Text(s.into())
}
fn cat(v: Vec<Doc>) -> Doc {
    Doc::Cat(v)
}
fn group(d: Doc) -> Doc {
    Doc::Group(Box::new(d))
}
fn nest(d: Doc) -> Doc {
    Doc::Nest(Box::new(d))
}
fn join(items: Vec<Doc>, sep: Doc) -> Doc {
    let mut out = Vec::new();
    for (i, d) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(sep.clone());
        }
        out.push(d);
    }
    cat(out)
}
/// `open item, item close` that breaks one item per line with a trailing comma.
fn list(open: &str, items: Vec<Doc>, close: &str, padded: bool) -> Doc {
    if items.is_empty() {
        return t(format!("{}{}", open, close));
    }
    let edge = if padded { Doc::Line } else { Doc::SoftLine };
    group(cat(vec![
        t(open),
        nest(cat(vec![edge.clone(), join(items, cat(vec![t(","), Doc::Line])), Doc::IfBreak(",")])),
        edge,
        t(close),
    ]))
}

fn render(doc: &Doc, width: usize) -> String {
    let mut out = String::new();
    let mut col: usize = 0;
    let mut pending: Option<usize> = None;
    let mut stack: Vec<(usize, bool, &Doc)> = vec![(0, false, doc)];
    while let Some((ind, flat, d)) = stack.pop() {
        match d {
            Doc::Text(s) => {
                if s.is_empty() {
                    continue;
                }
                if let Some(i) = pending.take() {
                    out.push_str(&" ".repeat(i));
                    col = i;
                }
                out.push_str(s);
                col += s.chars().count();
            }
            Doc::Line | Doc::SoftLine | Doc::HardLine => {
                if flat && !matches!(d, Doc::HardLine) {
                    if matches!(d, Doc::Line) {
                        if let Some(i) = pending.take() {
                            out.push_str(&" ".repeat(i));
                            col = i;
                        }
                        out.push(' ');
                        col += 1;
                    }
                } else {
                    out.push('\n');
                    pending = Some(ind);
                    col = ind;
                }
            }
            Doc::Nest(inner) => stack.push((ind + 2, flat, inner)),
            Doc::Cat(v) => {
                for x in v.iter().rev() {
                    stack.push((ind, flat, x));
                }
            }
            Doc::Group(inner) => {
                let f = flat || fits(width as isize - col as isize, inner, &stack);
                stack.push((ind, f, inner));
            }
            Doc::IfBreak(s) => {
                if !flat {
                    out.push_str(s);
                    col += s.len();
                }
            }
            Doc::IfFlat(s) => {
                if flat {
                    out.push_str(s);
                    col += s.len();
                }
            }
        }
    }
    out
}

/// Does `first` fit flat in `rem` columns, counting what follows it up to the next possible break?
fn fits(mut rem: isize, first: &Doc, rest: &[(usize, bool, &Doc)]) -> bool {
    let mut stack: Vec<(bool, &Doc)> = vec![(true, first)];
    let mut rest_i = rest.len();
    loop {
        let (flat, d) = match stack.pop() {
            Some(x) => x,
            None => {
                if rest_i == 0 {
                    return true;
                }
                rest_i -= 1;
                (rest[rest_i].1, rest[rest_i].2)
            }
        };
        if rem < 0 {
            return false;
        }
        match d {
            Doc::Text(s) => rem -= s.chars().count() as isize,
            Doc::Line => {
                if flat {
                    rem -= 1
                } else {
                    return true;
                }
            }
            Doc::SoftLine => {
                if !flat {
                    return true;
                }
            }
            Doc::HardLine => return !flat,
            Doc::Nest(inner) => stack.push((flat, inner)),
            Doc::Group(inner) => stack.push((flat, inner)),
            Doc::Cat(v) => {
                for x in v.iter().rev() {
                    stack.push((flat, x));
                }
            }
            Doc::IfBreak(s) => {
                if !flat {
                    rem -= s.len() as isize
                }
            }
            Doc::IfFlat(s) => {
                if flat {
                    rem -= s.len() as isize
                }
            }
        }
        if rem < 0 {
            return false;
        }
    }
}

// ---------------- printer ----------------

pub enum FmtError {
    /// The source does not parse; formatting refuses to touch it.
    Syntax(Vec<Diag>),
}

pub fn format_source(src: &str) -> Result<String, FmtError> {
    let out = parse(src);
    if out.diags.iter().any(|d| d.is_error()) {
        return Err(FmtError::Syntax(out.diags));
    }
    let mut p = Printer { src, comments: out.comments, ci: 0 };
    let doc = p.module(&out.module);
    let mut s = render(&doc, WIDTH);
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
    Ok(s)
}

/// Minimal single edit turning `old` into `new` (common prefix/suffix trimmed).
pub fn minimal_edit(old: &str, new: &str) -> Option<Edit> {
    if old == new {
        return None;
    }
    let a = old.as_bytes();
    let b = new.as_bytes();
    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    while !old.is_char_boundary(p) {
        p -= 1;
    }
    let mut s = 0;
    while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    while !old.is_char_boundary(a.len() - s) || !new.is_char_boundary(b.len() - s) {
        s -= 1;
    }
    Some(Edit { span: Span::new(p, a.len() - s), text: new[p..b.len() - s].to_string() })
}

/// `drift check` diagnostic for an unformatted file.
pub fn check_formatted(src: &str) -> Option<Diag> {
    let formatted = format_source(src).ok()?;
    let edit = minimal_edit(src, &formatted)?;
    // a warning: layout never stops a program from building or running
    let mut d = Diag::warning("F0001", edit.span, "layout differs from `drift fmt` (the program still runs; `drift check --fix` rewrites it)");
    d.fix.push(edit);
    Some(d)
}

/// Public interface of a module: signatures, types and docs, without bodies, examples or tests.
pub fn outline(src: &str) -> Result<String, FmtError> {
    let out = parse(src);
    if out.diags.iter().any(|d| d.is_error()) {
        return Err(FmtError::Syntax(out.diags));
    }
    let mut p = Printer { src, comments: Vec::new(), ci: 0 };
    let mut parts = Vec::new();
    for it in &out.module.items {
        if matches!(it.kind, ItemKind::Import { .. } | ItemKind::Examples { .. } | ItemKind::Test { .. }) {
            continue;
        }
        let mut s = String::new();
        for d in &it.doc {
            s.push_str(&format!("/// {}\n", d));
        }
        let mut item = it.clone();
        if let ItemKind::Fn(f) = &mut item.kind {
            f.body.stmts.clear();
            f.body.span = Span::empty_at(f.body.span.lo);
        }
        let text = render(&p.item(&item), WIDTH);
        s.push_str(text.trim_end().trim_end_matches("{}").trim_end());
        parts.push(s);
    }
    Ok(parts.join("\n") + "\n")
}

struct Printer<'a> {
    src: &'a str,
    comments: Vec<Comment>,
    ci: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum SeqMode {
    Module,
    Block,
}

fn prec(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Binary { op, .. } => match op {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 4,
            BinOp::BitOr | BinOp::BitXor | BinOp::BitAnd | BinOp::Shl | BinOp::Shr => 5,
            BinOp::Add | BinOp::Sub => 6,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 7,
        },
        ExprKind::Unary { op: UnOp::Not, .. } => 3,
        ExprKind::Range { .. } => 5,
        ExprKind::Unary { op: UnOp::Neg, .. } | ExprKind::Spawn(_) => 8,
        ExprKind::Closure { .. } | ExprKind::Return(_) | ExprKind::If { .. } | ExprKind::Match { .. } | ExprKind::Block(_) => 0,
        _ => 10,
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Or => "or",
        BinOp::And => "and",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

pub fn escape_lit(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            // invisible or easily confused characters stay escaped: controls, Unicode spaces other than ' ',
            // zero-width and direction marks, soft hyphen, BOM
            c if (c as u32) < 0x20
                || (0x7f..=0xa0).contains(&(c as u32))
                || (c.is_whitespace() && c != ' ')
                || matches!(c as u32, 0xad | 0x034f | 0x061c | 0x115f | 0x1160 | 0x180e | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff | 0xfff9..=0xfffb) =>
            {
                out.push_str(&format!("\\u{{{:x}}}", c as u32))
            }
            c => out.push(c),
        }
    }
}

fn ident_path(path: &[Ident]) -> String {
    path.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(".")
}

impl<'a> Printer<'a> {
    fn blank_between(&self, a: u32, b: u32) -> bool {
        if a >= b {
            return false;
        }
        let gap = &self.src[a as usize..b as usize];
        let mut newlines = 0;
        for c in gap.chars() {
            match c {
                '\n' => {
                    newlines += 1;
                    if newlines >= 2 {
                        return true;
                    }
                }
                ' ' | '\t' | '\r' => {}
                _ => newlines = 0,
            }
        }
        false
    }

    fn comment_doc(&self, c: &Comment) -> Doc {
        t(c.text.trim_end().to_string())
    }

    /// Lay out a sequence of nodes, interleaving comments, preserving single blank lines,
    /// and (module level) forcing one blank line between items except consecutive imports.
    fn seq<T>(&mut self, nodes: &[T], span_of: fn(&T) -> Span, is_import: fn(&T) -> bool, build: fn(&mut Self, &T) -> Doc, end: u32, mode: SeqMode) -> Vec<Doc> {
        let mut out: Vec<Doc> = Vec::new();
        let mut last_hi: Option<u32> = None;
        let mut last_import: Option<bool> = None; // Some(_) iff the previous element was a node
        for n in nodes {
            let span = span_of(n);
            while self.ci < self.comments.len() && self.comments[self.ci].span.lo < span.lo {
                let c = self.comments[self.ci].clone();
                self.ci += 1;
                if let Some(h) = last_hi {
                    out.push(Doc::HardLine);
                    if self.blank_between(h, c.span.lo) {
                        out.push(Doc::HardLine);
                    }
                }
                out.push(self.comment_doc(&c));
                last_hi = Some(c.span.hi);
                last_import = None;
            }
            if let Some(h) = last_hi {
                out.push(Doc::HardLine);
                let forced = mode == SeqMode::Module && last_import.is_some() && !(last_import == Some(true) && is_import(n));
                if forced || self.blank_between(h, span.lo) {
                    out.push(Doc::HardLine);
                }
            }
            out.push(build(self, n));
            last_hi = Some(span.hi);
            last_import = Some(is_import(n));
            // trailing comment on the same line
            if self.ci < self.comments.len() {
                let c = &self.comments[self.ci];
                if c.span.lo >= span.hi && !self.src[span.hi as usize..c.span.lo as usize].contains('\n') {
                    let c = c.clone();
                    self.ci += 1;
                    out.push(t(" "));
                    out.push(self.comment_doc(&c));
                    last_hi = Some(c.span.hi);
                }
            }
        }
        while self.ci < self.comments.len() && self.comments[self.ci].span.lo < end {
            let c = self.comments[self.ci].clone();
            self.ci += 1;
            if let Some(h) = last_hi {
                out.push(Doc::HardLine);
                if self.blank_between(h, c.span.lo) {
                    out.push(Doc::HardLine);
                }
            }
            out.push(self.comment_doc(&c));
            last_hi = Some(c.span.hi);
        }
        out
    }

    fn module(&mut self, m: &Module) -> Doc {
        let end = self.src.len() as u32 + 1;
        let docs = self.seq(&m.items, |i| i.span, |i| matches!(i.kind, ItemKind::Import { .. }), |p, i| p.item(i), end, SeqMode::Module);
        cat(docs)
    }

    // ---------- items ----------

    fn item(&mut self, it: &Item) -> Doc {
        let vis = if it.public { "pub " } else { "" };
        match &it.kind {
            ItemKind::Import { path, names } => {
                let mut s = format!("import {}", ident_path(path));
                if let Some(names) = names {
                    s.push_str(&format!(".{{{}}}", names.iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join(", ")));
                }
                t(s)
            }
            ItemKind::Struct { name, generics, fields } => cat(vec![
                t(format!("{}struct {}{} ", vis, name.name, self.generics(generics))),
                self.decl_fields(fields, it.span.hi),
            ]),
            ItemKind::Enum { name, generics, variants } => {
                let head = t(format!("{}enum {}{} {{", vis, name.name, self.generics(generics)));
                if variants.is_empty() {
                    return t(format!("{}enum {}{} {{}}", vis, name.name, self.generics(generics)));
                }
                let body = self.seq(variants, |v| v.span, |_| false, |p, v| p.variant(v), it.span.hi, SeqMode::Block);
                cat(vec![head, nest(cat(vec![Doc::HardLine, cat(body)])), Doc::HardLine, t("}")])
            }
            ItemKind::Const { name, ty, value } => cat(vec![t(format!("{}const {}: ", vis, name.name)), self.ty(ty), t(" = "), self.expr(value, false)]),
            ItemKind::TypeAlias { name, generics, ty } => cat(vec![t(format!("{}type {}{} = ", vis, name.name, self.generics(generics))), self.ty(ty)]),
            ItemKind::Fn(f) => self.fn_decl(vis, f),
            ItemKind::Examples { target, cases } => {
                if cases.is_empty() {
                    return t(format!("examples {} {{}}", target.name));
                }
                let body = self.seq(cases, |e| e.span, |_| false, |p, e| p.expr(e, false), it.span.hi, SeqMode::Block);
                cat(vec![t(format!("examples {} {{", target.name)), nest(cat(vec![Doc::HardLine, cat(body)])), Doc::HardLine, t("}")])
            }
            ItemKind::Test { name, uses, body } => {
                let mut s = String::from("test \"");
                escape_lit(name, &mut s);
                s.push('"');
                s.push_str(&self.uses(uses));
                s.push(' ');
                cat(vec![t(s), self.block(body)])
            }
        }
    }

    fn generics(&self, g: &[Ident]) -> String {
        if g.is_empty() {
            String::new()
        } else {
            format!("[{}]", g.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", "))
        }
    }

    fn uses(&self, u: &[Ident]) -> String {
        if u.is_empty() {
            String::new()
        } else {
            format!(" uses {}", u.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", "))
        }
    }

    /// `{ a: A, b: B }` flat, or one field per line without commas.
    fn decl_fields(&mut self, fields: &[Field], end: u32) -> Doc {
        if fields.is_empty() {
            return t("{}");
        }
        let before = self.ci;
        let docs = self.seq(fields, |f| f.name.span.to(f.ty.span), |_| false, |p, f| cat(vec![t(format!("{}: ", f.name.name)), p.ty(&f.ty)]), end, SeqMode::Block);
        if self.ci != before {
            // Comments inside: always one field per line.
            return cat(vec![t("{"), nest(cat(vec![Doc::HardLine, cat(docs)])), Doc::HardLine, t("}")]);
        }
        // seq separates with HardLine; for the flat form we need `, ` — rebuild with Line separators.
        let mut parts = Vec::new();
        for d in docs {
            match d {
                Doc::HardLine => {
                    if !matches!(parts.last(), Some(Doc::Line)) {
                        parts.push(Doc::IfFlat(","));
                        parts.push(Doc::Line);
                    }
                }
                other => parts.push(other),
            }
        }
        group(cat(vec![t("{"), nest(cat(vec![Doc::Line, cat(parts)])), Doc::Line, t("}")]))
    }

    fn variant(&mut self, v: &Variant) -> Doc {
        match &v.fields {
            VariantFields::Unit => t(v.name.name.clone()),
            VariantFields::Tuple(tys) => {
                let items = tys.iter().map(|ty| self.ty(ty)).collect();
                cat(vec![t(v.name.name.clone()), list("(", items, ")", false)])
            }
            VariantFields::Named(fields) => cat(vec![t(format!("{} ", v.name.name)), self.decl_fields(fields, v.span.hi)]),
        }
    }

    fn fn_decl(&mut self, vis: &str, f: &FnDecl) -> Doc {
        let params: Vec<Doc> = f
            .params
            .iter()
            .map(|p| cat(vec![t(format!("{}{}: ", if p.mutable { "var " } else { "" }, p.name.name)), self.ty(&p.ty)]))
            .collect();
        let sig = cat(vec![
            t(format!(
                "{}fn {}{}{}",
                vis,
                f.owner.as_ref().map(|o| format!("{}.", o.name)).unwrap_or_default(),
                f.name.name,
                self.generics(&f.generics)
            )),
            list("(", params, ")", false),
            t(" -> "),
            // The return type never breaks: long signatures break their parameter list instead.
            {
                let r = self.ty(&f.ret);
                t(render(&r, usize::MAX / 4))
            },
            t(self.uses(&f.uses)),
        ]);
        if f.requires.is_empty() && f.ensures.is_empty() {
            return cat(vec![sig, t(" "), self.block(&f.body)]);
        }
        let mut clauses = Vec::new();
        for r in &f.requires {
            clauses.push(Doc::HardLine);
            clauses.push(t("requires "));
            clauses.push(self.expr(r, true));
        }
        for e in &f.ensures {
            clauses.push(Doc::HardLine);
            clauses.push(t("ensures "));
            clauses.push(self.expr(e, true));
        }
        cat(vec![sig, nest(cat(clauses)), Doc::HardLine, self.block(&f.body)])
    }

    // ---------- types ----------

    fn ty(&mut self, ty: &Type) -> Doc {
        match &ty.kind {
            TypeKind::Named { path, args } => {
                if args.is_empty() {
                    t(ident_path(path))
                } else {
                    let a = args.iter().map(|x| self.ty(x)).collect();
                    cat(vec![t(ident_path(path)), list("[", a, "]", false)])
                }
            }
            TypeKind::Tuple(items) => {
                if items.len() == 1 {
                    return cat(vec![t("("), self.ty(&items[0]), t(",)")]);
                }
                let a = items.iter().map(|x| self.ty(x)).collect();
                list("(", a, ")", false)
            }
            TypeKind::Fn { params, ret, uses } => {
                let a = params.iter().map(|x| self.ty(x)).collect();
                cat(vec![t("fn"), list("(", a, ")", false), t(" -> "), self.ty(ret), t(self.uses(uses))])
            }
        }
    }

    // ---------- blocks and statements ----------

    fn block(&mut self, b: &Block) -> Doc {
        let has_comment = self.ci < self.comments.len() && self.comments[self.ci].span.lo < b.span.hi;
        if b.stmts.is_empty() && !has_comment {
            return t("{}");
        }
        let body = self.seq(&b.stmts, |s| s.span, |_| false, |p, s| p.stmt(s), b.span.hi, SeqMode::Block);
        cat(vec![t("{"), nest(cat(vec![Doc::HardLine, cat(body)])), Doc::HardLine, t("}")])
    }

    fn stmt(&mut self, s: &Stmt) -> Doc {
        match &s.kind {
            StmtKind::Let { mutable, pat, ty, value } => {
                let mut v = vec![t(if *mutable { "var " } else { "let " }), self.pat(pat)];
                if let Some(ty) = ty {
                    v.push(t(": "));
                    v.push(self.ty(ty));
                }
                v.push(t(" = "));
                v.push(self.expr(value, false));
                cat(v)
            }
            StmtKind::Assign { target, op, value } => {
                let o = match op {
                    AssignOp::Set => "=",
                    AssignOp::Add => "+=",
                    AssignOp::Sub => "-=",
                    AssignOp::Mul => "*=",
                    AssignOp::Div => "/=",
                    AssignOp::Rem => "%=",
                };
                cat(vec![self.expr(target, false), t(format!(" {} ", o)), self.expr(value, false)])
            }
            StmtKind::For { pat, iter, body } => cat(vec![t("for "), self.pat(pat), t(" in "), self.expr(iter, true), t(" "), self.block(body)]),
            StmtKind::While { cond, body } => cat(vec![t("while "), self.expr(cond, true), t(" "), self.block(body)]),
            StmtKind::Loop { body } => cat(vec![t("loop "), self.block(body)]),
            StmtKind::Assert { cond, message } => {
                let mut v = vec![t("assert "), self.expr(cond, false)];
                if let Some(m) = message {
                    v.push(t(", "));
                    v.push(self.expr(m, false));
                }
                cat(v)
            }
            StmtKind::Expr(e) => self.expr(e, false),
        }
    }

    // ---------- patterns ----------

    fn pat(&mut self, p: &Pat) -> Doc {
        match &p.kind {
            PatKind::Wildcard => t("_"),
            PatKind::Bind(n) => t(n.clone()),
            PatKind::Lit(e) => self.expr(e, false),
            PatKind::Tuple(items) => {
                if items.len() == 1 {
                    return cat(vec![t("("), self.pat(&items[0]), t(",)")]);
                }
                let v = items.iter().map(|x| self.pat(x)).collect();
                list("(", v, ")", false)
            }
            PatKind::List { items, rest } => {
                let mut v: Vec<Doc> = items.iter().map(|x| self.pat(x)).collect();
                if let Some(r) = rest {
                    v.push(t(format!("..{}", r.as_ref().map(|i| i.name.as_str()).unwrap_or(""))));
                }
                list("[", v, "]", false)
            }
            PatKind::Unit { path } => t(ident_path(path)),
            PatKind::TupleVariant { path, items } => {
                let v = items.iter().map(|x| self.pat(x)).collect();
                cat(vec![t(ident_path(path)), list("(", v, ")", false)])
            }
            PatKind::StructVariant { path, fields, rest } => {
                let mut v: Vec<Doc> = fields
                    .iter()
                    .map(|(n, sub)| match sub {
                        None => t(n.name.clone()),
                        Some(sp) => cat(vec![t(format!("{}: ", n.name)), self.pat(sp)]),
                    })
                    .collect();
                if *rest {
                    v.push(t(".."));
                }
                cat(vec![t(format!("{} ", ident_path(path))), list("{", v, "}", true)])
            }
            PatKind::Or(alts) => {
                let v = alts.iter().map(|x| self.pat(x)).collect();
                join(v, t(" | "))
            }
        }
    }

    // ---------- expressions ----------

    fn paren(d: Doc) -> Doc {
        cat(vec![t("("), d, t(")")])
    }

    /// `ns`: the expression sits in an `if`/`while`/`for`/`match` head, so bare struct literals need parens.
    fn expr(&mut self, e: &Expr, ns: bool) -> Doc {
        match &e.kind {
            ExprKind::Int(s) | ExprKind::Float(s) => t(s.clone()),
            ExprKind::Bool(b) => t(if *b { "true" } else { "false" }),
            ExprKind::Str(parts) => {
                let mut s = String::from("\"");
                for p in parts {
                    match p {
                        StrPart::Lit(l) => escape_lit(l, &mut s),
                        StrPart::Expr(x) => {
                            let d = self.expr(x, false);
                            s.push('{');
                            s.push_str(&render(&d, usize::MAX / 4));
                            s.push('}');
                        }
                    }
                }
                s.push('"');
                t(s)
            }
            ExprKind::RawStr(s) if s.contains('\n') || s.ends_with('"') => {
                let mut lines = Vec::new();
                for l in s.split('\n') {
                    lines.push(Doc::HardLine);
                    lines.push(t(l.trim_end().to_string()));
                }
                cat(vec![t("r\"\"\""), nest(cat(lines)), Doc::HardLine, t("\"\"\"")])
            }
            ExprKind::RawStr(s) if s.contains('"') => t(format!("r\"\"\"{}\"\"\"", s)),
            ExprKind::RawStr(s) => t(format!("r\"{}\"", s)),
            ExprKind::Bytes(b) => {
                let mut s = String::from("b\"");
                for &c in b {
                    match c {
                        b'\\' => s.push_str("\\\\"),
                        b'"' => s.push_str("\\\""),
                        b'\n' => s.push_str("\\n"),
                        b'\t' => s.push_str("\\t"),
                        b'\r' => s.push_str("\\r"),
                        0x20..=0x7e => s.push(c as char),
                        _ => s.push_str(&format!("\\x{:02x}", c)),
                    }
                }
                s.push('"');
                t(s)
            }
            ExprKind::Name(n) => t(n.clone()),
            ExprKind::Todo => t("todo"),
            ExprKind::Break => t("break"),
            ExprKind::Continue => t("continue"),
            ExprKind::Return(v) => match v {
                None => t("return"),
                Some(x) => cat(vec![t("return "), self.expr(x, ns)]),
            },
            ExprKind::Tuple(items) => {
                if items.len() == 1 {
                    return cat(vec![t("("), self.expr(&items[0], false), t(",)")]);
                }
                let v = items.iter().map(|x| self.expr(x, false)).collect();
                list("(", v, ")", false)
            }
            ExprKind::List(items) => {
                let v = items.iter().map(|x| self.expr(x, false)).collect();
                list("[", v, "]", false)
            }
            ExprKind::Map(entries) => {
                let v = entries.iter().map(|(k, val)| cat(vec![self.expr(k, false), t(": "), self.expr(val, false)])).collect();
                list("{", v, "}", false)
            }
            ExprKind::StructLit { path, fields } => {
                let v = fields
                    .iter()
                    .map(|(n, val)| match val {
                        None => t(n.name.clone()),
                        Some(x) => cat(vec![t(format!("{}: ", n.name)), self.expr(x, false)]),
                    })
                    .collect();
                let d = cat(vec![self.expr(path, false), t(" "), list("{", v, "}", true)]);
                if ns { Self::paren(d) } else { d }
            }
            ExprKind::With { base, fields } => {
                let v: Vec<Doc> = fields.iter().map(|(n, x)| cat(vec![t(format!("{}: ", n.name)), self.expr(x, false)])).collect();
                group(cat(vec![
                    t("{"),
                    nest(cat(vec![Doc::Line, self.expr(base, false), t(" with"), nest(cat(vec![Doc::Line, join(v, cat(vec![t(","), Doc::Line])), Doc::IfBreak(",")]))])),
                    Doc::Line,
                    t("}"),
                ]))
            }
            ExprKind::Field { .. } | ExprKind::Call { .. } | ExprKind::Index { .. } | ExprKind::Try(_) => self.postfix_chain(e, ns),
            ExprKind::Unary { op, expr } => {
                let (s, min) = match op {
                    UnOp::Neg => ("-", 8),
                    UnOp::Not => ("not ", 4),
                };
                let inner = self.expr(expr, ns);
                cat(vec![t(s), if prec(expr) < min { Self::paren(inner) } else { inner }])
            }
            ExprKind::Binary { op, .. } => self.binary(e, *op, ns),
            ExprKind::Range { lo, hi, inclusive } => {
                let l = self.operand(lo, 5, true, ns);
                let h = self.operand(hi, 5, true, ns);
                cat(vec![l, t(if *inclusive { "..=" } else { ".." }), h])
            }
            ExprKind::If { cond, then, els } => {
                let mut v = vec![t("if "), self.expr(cond, true), t(" "), self.block(then)];
                if let Some(x) = els {
                    v.push(t(" else "));
                    match &x.kind {
                        ExprKind::Block(b) => v.push(self.block(b)),
                        _ => v.push(self.expr(x, false)),
                    }
                }
                cat(v)
            }
            ExprKind::Match { scrutinee, arms } => {
                let head = cat(vec![t("match "), self.expr(scrutinee, true), t(" {")]);
                let body = self.seq(arms, |a| a.span, |_| false, |p, a| p.arm(a), e.span.hi, SeqMode::Block);
                cat(vec![head, nest(cat(vec![Doc::HardLine, cat(body)])), Doc::HardLine, t("}")])
            }
            ExprKind::Closure { params, ret, body } => {
                let ps: Vec<Doc> = params
                    .iter()
                    .map(|(p, ty)| match ty {
                        None => self.pat(p),
                        Some(ty) => cat(vec![self.pat(p), t(": "), self.ty(ty)]),
                    })
                    .collect();
                let mut v = vec![t("|"), join(ps, t(", ")), t("|")];
                if let Some(r) = ret {
                    v.push(t(" -> "));
                    v.push(self.ty(r));
                }
                v.push(t(" "));
                v.push(self.body_expr(body));
                cat(v)
            }
            ExprKind::Block(b) => self.block(b),
            ExprKind::Spawn(x) => cat(vec![t("spawn "), self.expr(x, ns)]),
        }
    }

    /// Body after `=>` or `|params|`: a block, or an expression where a leading `{` would read as a block.
    fn body_expr(&mut self, e: &Expr) -> Doc {
        match &e.kind {
            ExprKind::Block(b) => self.block(b),
            ExprKind::Map(_) | ExprKind::With { .. } => Self::paren(self.expr(e, false)),
            _ => self.expr(e, false),
        }
    }

    fn arm(&mut self, a: &Arm) -> Doc {
        let mut v = vec![self.pat(&a.pat)];
        if let Some(g) = &a.guard {
            v.push(t(" if "));
            v.push(self.expr(g, false));
        }
        v.push(t(" => "));
        // `None => count += 1`: a block holding one assignment (and no comment) stays on the arm's line
        if let ExprKind::Block(b) = &a.body.kind {
            let has_comment = self.ci < self.comments.len() && self.comments[self.ci].span.lo < b.span.hi;
            if let [s] = b.stmts.as_slice() {
                if matches!(s.kind, StmtKind::Assign { .. }) && !has_comment {
                    v.push(self.stmt(s));
                    return cat(v);
                }
            }
        }
        v.push(self.body_expr(&a.body));
        cat(v)
    }

    fn operand(&mut self, e: &Expr, p: u8, strict: bool, ns: bool) -> Doc {
        // Build exactly once: building consumes comments.
        let need = if strict { prec(e) <= p } else { prec(e) < p };
        if need { Self::paren(self.expr(e, false)) } else { self.expr(e, ns) }
    }

    fn binary(&mut self, e: &Expr, op: BinOp, ns: bool) -> Doc {
        let p = prec(e);
        // Flatten a left-associative chain of the same precedence: a + b - c
        let mut operands = Vec::new();
        let mut ops = Vec::new();
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::Binary { op: o, lhs, rhs } if prec(cur) == p && p != 4 => {
                    operands.push(&**rhs);
                    ops.push(*o);
                    cur = lhs;
                }
                _ => break,
            }
        }
        let _ = op;
        operands.reverse();
        ops.reverse();
        if ops.is_empty() {
            // comparison (non-associative): both sides strict
            if let ExprKind::Binary { op: o, lhs, rhs } = &e.kind {
                let l = self.operand(lhs, p, true, ns);
                let r = self.operand(rhs, p, true, ns);
                return group(cat(vec![l, t(format!(" {}", binop_str(*o))), nest(cat(vec![Doc::Line, r]))]));
            }
        }
        let first = self.operand(cur, p, false, ns);
        let mut rest = Vec::new();
        for (o, x) in ops.iter().zip(operands) {
            rest.push(t(format!(" {}", binop_str(*o))));
            rest.push(Doc::Line);
            rest.push(self.operand(x, p, true, ns));
        }
        group(cat(vec![first, nest(cat(rest))]))
    }

    /// Field access, calls, indexing and `?`, with method chains that break before each `.call`.
    fn postfix_chain(&mut self, e: &Expr, ns: bool) -> Doc {
        enum Link<'e> {
            Field(&'e Ident),
            Call(&'e [Expr]),
            Index(&'e Expr),
            Try,
        }
        let mut links = Vec::new();
        let mut cur = e;
        loop {
            match &cur.kind {
                ExprKind::Field { base, name } => {
                    links.push(Link::Field(name));
                    cur = base;
                }
                ExprKind::Call { callee, args } => {
                    links.push(Link::Call(args));
                    cur = callee;
                }
                ExprKind::Index { base, index } => {
                    links.push(Link::Index(index));
                    cur = base;
                }
                ExprKind::Try(x) => {
                    links.push(Link::Try);
                    cur = x;
                }
                _ => break,
            }
        }
        links.reverse();
        let root = if prec(cur) < 10 { Self::paren(self.expr(cur, false)) } else { self.expr(cur, ns) };
        // method calls = Field immediately followed by Call
        let method_calls = links.windows(2).filter(|w| matches!((&w[0], &w[1]), (Link::Field(_), Link::Call(_)))).count();
        let breakable = method_calls >= 2;
        let mut parts = Vec::new();
        let mut i = 0;
        while i < links.len() {
            match &links[i] {
                Link::Field(name) => {
                    let is_method = matches!(links.get(i + 1), Some(Link::Call(_)));
                    if breakable && is_method {
                        parts.push(Doc::SoftLine);
                    }
                    parts.push(t(format!(".{}", name.name)));
                }
                Link::Call(args) => parts.push(self.args(args)),
                Link::Index(x) => {
                    let d = self.expr(x, false);
                    parts.push(cat(vec![t("["), d, t("]")]));
                }
                Link::Try => parts.push(t("?")),
            }
            i += 1;
        }
        if breakable {
            group(cat(vec![root, nest(cat(parts))]))
        } else {
            cat(vec![root, cat(parts)])
        }
    }

    fn args(&mut self, args: &[Expr]) -> Doc {
        // Hug a trailing block closure: f(a, |x| {\n ...\n})
        if let Some(last) = args.last() {
            if let ExprKind::Closure { body, .. } = &last.kind {
                if matches!(body.kind, ExprKind::Block(_)) {
                    let mut v = vec![t("(")];
                    for a in &args[..args.len() - 1] {
                        v.push(self.expr(a, false));
                        v.push(t(", "));
                    }
                    v.push(self.expr(last, false));
                    v.push(t(")"));
                    return cat(v);
                }
            }
        }
        let v = args.iter().map(|a| self.expr(a, false)).collect();
        list("(", v, ")", false)
    }
}
