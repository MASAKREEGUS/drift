//! Recursive-descent parser with Pratt expressions. Never stops at the first error:
//! items and statements are resynchronized so one run reports every syntax problem.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{self, Kw, StrPiece, Tok, Token, P};

type PResult<T> = Result<T, ()>;

pub struct ParseOutput {
    pub module: Module,
    pub diags: Vec<Diag>,
    pub comments: Vec<lexer::Comment>,
}

pub fn parse(src: &str) -> ParseOutput {
    let lexed = lexer::lex(src);
    let mut p = Parser { src, toks: lexed.tokens, pos: 0, diags: lexed.diags, no_struct: false };
    let module = p.module();
    let mut diags = p.diags;
    diags.sort_by_key(|d| (d.span.lo, d.span.hi));
    ParseOutput { module, diags, comments: lexed.comments }
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
    diags: Vec<Diag>,
    /// Set while parsing the head of `if`/`while`/`for`/`match`: `Name {` is not a struct literal there.
    no_struct: bool,
}

fn is_pascal(s: &str) -> bool {
    s.chars().next().map_or(false, |c| c.is_ascii_uppercase())
}

const ITEM_START: &[Kw] = &[Kw::Pub, Kw::Fn, Kw::Struct, Kw::Enum, Kw::Const, Kw::Type, Kw::Import, Kw::Examples, Kw::Test];

impl<'a> Parser<'a> {
    // ---------- token helpers ----------

    fn tok(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn tok_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos].span
    }
    fn prev_span(&self) -> Span {
        if self.pos == 0 { self.toks[0].span } else { self.toks[self.pos - 1].span }
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn at_p(&self, p: P) -> bool {
        *self.tok() == Tok::P(p)
    }
    fn at_kw(&self, k: Kw) -> bool {
        *self.tok() == Tok::Kw(k)
    }
    fn eat_p(&mut self, p: P) -> bool {
        if self.at_p(p) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, k: Kw) -> bool {
        if self.at_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn at_nl(&self) -> bool {
        matches!(self.tok(), Tok::Newline | Tok::Eof)
    }
    fn skip_nl(&mut self) {
        while matches!(self.tok(), Tok::Newline) || self.at_p(P::Semi) {
            self.bump();
        }
    }

    fn error_here(&mut self, code: &'static str, expected: &str) {
        let found = self.tok().describe();
        let span = self.span();
        self.diags.push(
            Diag::error(code, span, format!("expected {}, found {}", expected, found))
                .expected(expected.to_string())
                .found(found),
        );
    }

    fn expect_p(&mut self, p: P) -> PResult<Span> {
        if self.at_p(p) {
            Ok(self.bump().span)
        } else {
            let found = self.tok().describe();
            let span = self.span();
            let mut d = Diag::error("P0001", span, format!("expected `{}`, found {}", p.as_str(), found))
                .expected(format!("`{}`", p.as_str()))
                .found(found);
            if matches!(p, P::RParen | P::RBracket | P::RBrace | P::Colon | P::Comma | P::Eq) {
                d = d.with_fix(Span::empty_at(self.prev_span().hi), p.as_str());
            }
            self.diags.push(d);
            Err(())
        }
    }

    fn ident(&mut self) -> PResult<Ident> {
        match self.tok().clone() {
            Tok::Ident(name) => {
                let span = self.bump().span;
                Ok(Ident { name, span })
            }
            Tok::Kw(k) => {
                let span = self.span();
                self.diags.push(
                    Diag::error("P0002", span, format!("`{}` is a reserved keyword and cannot be used as a name", k.as_str()))
                        .expected("identifier")
                        .found(format!("keyword `{}`", k.as_str()))
                        .with_fix(span, format!("{}_", k.as_str())),
                );
                self.bump();
                Err(())
            }
            _ => {
                self.error_here("P0001", "identifier");
                Err(())
            }
        }
    }

    /// Statement/item terminator: newline, `;`, or a closing brace / EOF left for the caller.
    fn end_of_stmt(&mut self) -> PResult<()> {
        if matches!(self.tok(), Tok::Newline) || self.at_p(P::Semi) {
            self.bump();
            Ok(())
        } else if self.at_p(P::RBrace) || matches!(self.tok(), Tok::Eof) {
            Ok(())
        } else {
            let found = self.tok().describe();
            let span = self.span();
            self.diags.push(
                Diag::error("P0003", span, format!("expected end of line, found {}", found))
                    .expected("end of line")
                    .found(found),
            );
            Err(())
        }
    }

    // ---------- recovery ----------

    fn sync_item(&mut self) {
        let mut depth: i32 = 0;
        loop {
            match self.tok() {
                Tok::Eof => return,
                Tok::P(P::LBrace) | Tok::P(P::LParen) | Tok::P(P::LBracket) => depth += 1,
                Tok::P(P::RBrace) | Tok::P(P::RParen) | Tok::P(P::RBracket) => depth -= 1,
                Tok::Newline if depth <= 0 => {
                    self.bump();
                    if let Tok::Kw(k) = self.tok() {
                        if ITEM_START.contains(k) {
                            return;
                        }
                    }
                    depth = 0;
                    continue;
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Skip to the end of the current statement inside a block (newline at depth 0, or the closing `}`).
    fn sync_stmt(&mut self) {
        let mut depth: i32 = 0;
        loop {
            match self.tok() {
                Tok::Eof => return,
                Tok::P(P::RBrace) if depth == 0 => return,
                Tok::Newline if depth == 0 => {
                    self.bump();
                    return;
                }
                Tok::P(P::LBrace) | Tok::P(P::LParen) | Tok::P(P::LBracket) => depth += 1,
                Tok::P(P::RBrace) | Tok::P(P::RParen) | Tok::P(P::RBracket) => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }

    // ---------- items ----------

    fn module(&mut self) -> Module {
        let mut items = Vec::new();
        loop {
            self.skip_nl();
            if matches!(self.tok(), Tok::Eof) {
                break;
            }
            match self.item() {
                Ok(it) => {
                    items.push(it);
                    if self.end_of_stmt().is_err() {
                        self.sync_item();
                    }
                }
                Err(()) => self.sync_item(),
            }
        }
        Module { items }
    }

    fn item(&mut self) -> PResult<Item> {
        let doc = self.toks[self.pos].doc.clone();
        let lo = self.span();
        let public = self.eat_kw(Kw::Pub);
        let kind = match self.tok() {
            Tok::Kw(Kw::Fn) => ItemKind::Fn(self.fn_decl()?),
            Tok::Kw(Kw::Struct) => self.struct_decl()?,
            Tok::Kw(Kw::Enum) => self.enum_decl()?,
            Tok::Kw(Kw::Const) => self.const_decl()?,
            Tok::Kw(Kw::Type) => self.type_alias()?,
            Tok::Kw(Kw::Import) => self.import()?,
            Tok::Kw(Kw::Examples) => self.examples()?,
            Tok::Kw(Kw::Test) => self.test()?,
            Tok::Kw(Kw::Let) | Tok::Kw(Kw::Var) => {
                let span = self.span();
                self.diags.push(
                    Diag::error("P0010", span, "top-level `let`/`var` is not allowed: Drift has no global variables; use `const` for compile-time constants")
                        .expected("item (fn, struct, enum, const, type, import, examples, test)")
                        .found(self.tok().describe()),
                );
                return Err(());
            }
            _ => {
                self.error_here("P0010", "item (fn, struct, enum, const, type, import, examples, test)");
                return Err(());
            }
        };
        if public && matches!(kind, ItemKind::Import { .. } | ItemKind::Examples { .. } | ItemKind::Test { .. }) {
            self.diags.push(
                Diag::error("P0011", lo, "`pub` has no meaning on import, examples or test")
                    .with_fix(Span::new(lo.lo as usize, lo.hi as usize + 1), ""),
            );
        }
        Ok(Item { doc, public, kind, span: lo.to(self.prev_span()) })
    }

    fn generics(&mut self) -> PResult<Vec<Ident>> {
        let mut out = Vec::new();
        if self.eat_p(P::LBracket) {
            while !self.at_p(P::RBracket) {
                out.push(self.ident()?);
                if !self.eat_p(P::Comma) {
                    break;
                }
            }
            self.expect_p(P::RBracket)?;
        }
        Ok(out)
    }

    fn fn_decl(&mut self) -> PResult<FnDecl> {
        self.bump(); // fn
        let name = self.ident()?;
        let generics = self.generics()?;
        self.expect_p(P::LParen)?;
        let mut params = Vec::new();
        while !self.at_p(P::RParen) {
            let mutable = self.eat_kw(Kw::Var);
            let pname = self.ident()?;
            if mutable && pname.name != "self" {
                self.diags.push(
                    Diag::error("P0020", pname.span, "only `self` can be a `var` parameter; return a new value instead")
                        .expected("var self")
                        .found(format!("var {}", pname.name)),
                );
            }
            if !self.at_p(P::Colon) {
                let span = self.span();
                self.diags.push(
                    Diag::error("P0021", pname.span, format!("parameter `{}` needs a type: signatures are always complete", pname.name))
                        .expected("`name: Type`")
                        .with_fix(Span::empty_at(pname.span.hi), ": todo_type"),
                );
                let _ = span;
                return Err(());
            }
            self.bump();
            let ty = self.ty()?;
            params.push(Param { mutable, name: pname, ty });
            if !self.eat_p(P::Comma) {
                break;
            }
        }
        self.expect_p(P::RParen)?;
        let ret = if self.eat_p(P::Arrow) {
            self.ty()?
        } else {
            let at = self.prev_span().hi;
            self.diags.push(
                Diag::error("P0022", Span::empty_at(at), format!("function `{}` must declare its return type", name.name))
                    .expected("`-> Type`")
                    .found(self.tok().describe())
                    .with_fix(Span::empty_at(at), " -> ()"),
            );
            Type { kind: TypeKind::Tuple(vec![]), span: Span::empty_at(at) }
        };
        let uses = self.uses()?;
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        loop {
            let save = self.pos;
            self.skip_nl();
            if self.eat_kw(Kw::Requires) {
                requires.push(self.expr_no_struct()?);
            } else if self.eat_kw(Kw::Ensures) {
                ensures.push(self.expr_no_struct()?);
            } else {
                if !self.at_p(P::LBrace) {
                    self.pos = save;
                }
                break;
            }
        }
        if !self.at_p(P::LBrace) {
            self.error_here("P0001", "`{` to start the function body");
            return Err(());
        }
        let body = self.block()?;
        Ok(FnDecl { name, generics, params, ret, uses, requires, ensures, body })
    }

    fn uses(&mut self) -> PResult<Vec<Ident>> {
        let mut out = Vec::new();
        if self.eat_kw(Kw::Uses) {
            loop {
                // `spawn` is both a keyword and an effect name.
                if self.at_kw(Kw::Spawn) {
                    let span = self.bump().span;
                    out.push(Ident { name: "spawn".into(), span });
                } else {
                    out.push(self.ident()?);
                }
                if !self.eat_p(P::Comma) {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Fields separated by `,` or newlines, between braces already opened by the caller.
    fn fields(&mut self) -> PResult<Vec<Field>> {
        let mut fields = Vec::new();
        loop {
            self.skip_nl();
            if self.at_p(P::RBrace) {
                break;
            }
            let name = self.ident()?;
            self.expect_p(P::Colon)?;
            let ty = self.ty()?;
            fields.push(Field { name, ty });
            if !self.eat_p(P::Comma) && !matches!(self.tok(), Tok::Newline) && !self.at_p(P::RBrace) {
                self.error_here("P0001", "`,`, end of line or `}`");
                return Err(());
            }
        }
        self.expect_p(P::RBrace)?;
        Ok(fields)
    }

    fn struct_decl(&mut self) -> PResult<ItemKind> {
        self.bump();
        let name = self.ident()?;
        let generics = self.generics()?;
        self.expect_p(P::LBrace)?;
        let fields = self.fields()?;
        Ok(ItemKind::Struct { name, generics, fields })
    }

    fn enum_decl(&mut self) -> PResult<ItemKind> {
        self.bump();
        let name = self.ident()?;
        let generics = self.generics()?;
        self.expect_p(P::LBrace)?;
        let mut variants = Vec::new();
        loop {
            self.skip_nl();
            if self.at_p(P::RBrace) {
                break;
            }
            let vname = self.ident()?;
            let fields = if self.eat_p(P::LBrace) {
                VariantFields::Named(self.fields()?)
            } else if self.eat_p(P::LParen) {
                let mut tys = Vec::new();
                while !self.at_p(P::RParen) {
                    tys.push(self.ty()?);
                    if !self.eat_p(P::Comma) {
                        break;
                    }
                }
                self.expect_p(P::RParen)?;
                VariantFields::Tuple(tys)
            } else {
                VariantFields::Unit
            };
            let span = vname.span.to(self.prev_span());
            variants.push(Variant { name: vname, fields, span });
            if !self.eat_p(P::Comma) && !matches!(self.tok(), Tok::Newline) && !self.at_p(P::RBrace) {
                self.error_here("P0001", "`,`, end of line or `}` after enum variant");
                return Err(());
            }
        }
        self.expect_p(P::RBrace)?;
        Ok(ItemKind::Enum { name, generics, variants })
    }

    fn const_decl(&mut self) -> PResult<ItemKind> {
        self.bump();
        let name = self.ident()?;
        if !self.at_p(P::Colon) {
            self.diags.push(
                Diag::error("P0023", name.span, "`const` needs an explicit type")
                    .expected("`const NAME: Type = value`")
                    .with_fix(Span::empty_at(name.span.hi), ": todo_type"),
            );
            return Err(());
        }
        self.bump();
        let ty = self.ty()?;
        self.expect_p(P::Eq)?;
        let value = self.expr()?;
        Ok(ItemKind::Const { name, ty, value })
    }

    fn type_alias(&mut self) -> PResult<ItemKind> {
        self.bump();
        let name = self.ident()?;
        let generics = self.generics()?;
        self.expect_p(P::Eq)?;
        let ty = self.ty()?;
        Ok(ItemKind::TypeAlias { name, generics, ty })
    }

    fn import(&mut self) -> PResult<ItemKind> {
        self.bump();
        let mut path = vec![self.ident()?];
        let mut names = None;
        while self.eat_p(P::Dot) {
            if self.eat_p(P::LBrace) {
                let mut list = Vec::new();
                loop {
                    self.skip_nl();
                    if self.at_p(P::RBrace) {
                        break;
                    }
                    list.push(self.ident()?);
                    if !self.eat_p(P::Comma) {
                        self.skip_nl();
                        break;
                    }
                }
                self.expect_p(P::RBrace)?;
                names = Some(list);
                break;
            }
            path.push(self.ident()?);
        }
        Ok(ItemKind::Import { path, names })
    }

    fn examples(&mut self) -> PResult<ItemKind> {
        self.bump();
        let target = self.ident()?;
        self.expect_p(P::LBrace)?;
        let mut cases = Vec::new();
        loop {
            self.skip_nl();
            if self.at_p(P::RBrace) || matches!(self.tok(), Tok::Eof) {
                break;
            }
            match self.expr().and_then(|e| self.end_of_stmt().map(|_| e)) {
                Ok(e) => cases.push(e),
                Err(()) => self.sync_stmt(),
            }
        }
        self.expect_p(P::RBrace)?;
        Ok(ItemKind::Examples { target, cases })
    }

    fn test(&mut self) -> PResult<ItemKind> {
        self.bump();
        let name = match self.tok().clone() {
            Tok::Str(pieces) if pieces.iter().all(|p| matches!(p, StrPiece::Lit(_))) => {
                self.bump();
                pieces
                    .into_iter()
                    .map(|p| match p {
                        StrPiece::Lit(s) => s,
                        StrPiece::Interp(_) => unreachable!(),
                    })
                    .collect()
            }
            _ => {
                self.error_here("P0024", "test name as a plain string literal");
                return Err(());
            }
        };
        let uses = self.uses()?;
        let body = self.block()?;
        Ok(ItemKind::Test { name, uses, body })
    }

    // ---------- types ----------

    fn ty(&mut self) -> PResult<Type> {
        let lo = self.span();
        if self.eat_p(P::LParen) {
            let mut items = Vec::new();
            while !self.at_p(P::RParen) {
                items.push(self.ty()?);
                if !self.eat_p(P::Comma) {
                    break;
                }
            }
            self.expect_p(P::RParen)?;
            if items.len() == 1 {
                self.diags.push(
                    Diag::error("P0030", lo.to(self.prev_span()), "parenthesized type: write the type without parentheses")
                        .with_fix(lo.to(self.prev_span()), self.src[items[0].span.lo as usize..items[0].span.hi as usize].to_string()),
                );
                return Ok(items.pop().unwrap());
            }
            return Ok(Type { kind: TypeKind::Tuple(items), span: lo.to(self.prev_span()) });
        }
        if self.eat_kw(Kw::Fn) {
            self.expect_p(P::LParen)?;
            let mut params = Vec::new();
            while !self.at_p(P::RParen) {
                params.push(self.ty()?);
                if !self.eat_p(P::Comma) {
                    break;
                }
            }
            self.expect_p(P::RParen)?;
            self.expect_p(P::Arrow)?;
            let ret = Box::new(self.ty()?);
            let uses = self.uses()?;
            return Ok(Type { kind: TypeKind::Fn { params, ret, uses }, span: lo.to(self.prev_span()) });
        }
        if let Tok::P(P::Lt) = self.tok() {
            self.error_here("P0031", "type");
            return Err(());
        }
        let mut path = vec![self.ident()?];
        while self.at_p(P::Dot) {
            self.bump();
            path.push(self.ident()?);
        }
        let mut args = Vec::new();
        if self.eat_p(P::LBracket) {
            while !self.at_p(P::RBracket) {
                args.push(self.ty()?);
                if !self.eat_p(P::Comma) {
                    break;
                }
            }
            self.expect_p(P::RBracket)?;
        } else if self.at_p(P::Lt) {
            // Agents trained on Rust/TS write `List<Int>`: point to `List[Int]`.
            let open = self.span();
            self.diags.push(
                Diag::error("P0031", open, "generic arguments use square brackets in Drift")
                    .expected("`[`")
                    .found("`<`")
                    .with_fix(open, "["),
            );
            self.bump();
            while !self.at_p(P::Gt) && !self.at_nl() {
                args.push(self.ty()?);
                if !self.eat_p(P::Comma) {
                    break;
                }
            }
            if self.at_p(P::Gt) {
                let close = self.bump().span;
                if let Some(d) = self.diags.last_mut() {
                    d.fix.push(crate::diag::Edit { span: close, text: "]".into() });
                }
            }
        }
        Ok(Type { kind: TypeKind::Named { path, args }, span: lo.to(self.prev_span()) })
    }

    // ---------- blocks and statements ----------

    fn block(&mut self) -> PResult<Block> {
        let lo = self.expect_p(P::LBrace)?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut stmts = Vec::new();
        loop {
            self.skip_nl();
            if self.at_p(P::RBrace) || matches!(self.tok(), Tok::Eof) {
                break;
            }
            match self.stmt() {
                Ok(s) => {
                    stmts.push(s);
                    if self.end_of_stmt().is_err() {
                        self.sync_stmt();
                    }
                }
                Err(()) => self.sync_stmt(),
            }
        }
        self.no_struct = saved;
        let hi = self.expect_p(P::RBrace)?;
        Ok(Block { stmts, span: lo.to(hi) })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let lo = self.span();
        let kind = match self.tok() {
            Tok::Kw(Kw::Let) | Tok::Kw(Kw::Var) => {
                let mutable = self.at_kw(Kw::Var);
                self.bump();
                let pat = self.pattern(false)?;
                let ty = if self.eat_p(P::Colon) { Some(self.ty()?) } else { None };
                if !self.at_p(P::Eq) {
                    self.diags.push(
                        Diag::error("P0040", self.span(), "bindings must be initialized: `let x = value`")
                            .expected("`=`")
                            .found(self.tok().describe()),
                    );
                    return Err(());
                }
                self.bump();
                let value = self.expr()?;
                StmtKind::Let { mutable, pat, ty, value }
            }
            Tok::Kw(Kw::For) => {
                self.bump();
                let pat = self.pattern(false)?;
                if !self.eat_kw(Kw::In) {
                    self.error_here("P0001", "`in`");
                    return Err(());
                }
                let iter = self.expr_no_struct()?;
                let body = self.block()?;
                StmtKind::For { pat, iter, body }
            }
            Tok::Kw(Kw::While) => {
                self.bump();
                let cond = self.expr_no_struct()?;
                let body = self.block()?;
                StmtKind::While { cond, body }
            }
            Tok::Kw(Kw::Loop) => {
                self.bump();
                let body = self.block()?;
                StmtKind::Loop { body }
            }
            Tok::Kw(Kw::Assert) => {
                self.bump();
                let cond = self.expr()?;
                let message = if self.eat_p(P::Comma) { Some(self.expr()?) } else { None };
                StmtKind::Assert { cond, message }
            }
            _ => {
                let e = self.expr()?;
                let op = match self.tok() {
                    Tok::P(P::Eq) => Some(AssignOp::Set),
                    Tok::P(P::PlusEq) => Some(AssignOp::Add),
                    Tok::P(P::MinusEq) => Some(AssignOp::Sub),
                    Tok::P(P::StarEq) => Some(AssignOp::Mul),
                    Tok::P(P::SlashEq) => Some(AssignOp::Div),
                    Tok::P(P::PercentEq) => Some(AssignOp::Rem),
                    _ => None,
                };
                match op {
                    Some(op) => {
                        if !matches!(e.kind, ExprKind::Name(_) | ExprKind::Field { .. } | ExprKind::Index { .. }) {
                            self.diags.push(Diag::error("P0041", e.span, "left side of an assignment must be a variable, field or index"));
                        }
                        self.bump();
                        let value = self.expr()?;
                        StmtKind::Assign { target: e, op, value }
                    }
                    None => StmtKind::Expr(e),
                }
            }
        };
        Ok(Stmt { kind, span: lo.to(self.prev_span()) })
    }

    // ---------- patterns ----------

    fn pattern(&mut self, allow_or: bool) -> PResult<Pat> {
        let first = self.pattern_primary()?;
        if !allow_or || !self.at_p(P::Pipe) {
            return Ok(first);
        }
        let lo = first.span;
        let mut alts = vec![first];
        while self.eat_p(P::Pipe) {
            alts.push(self.pattern_primary()?);
        }
        let span = lo.to(self.prev_span());
        Ok(Pat { kind: PatKind::Or(alts), span })
    }

    fn pattern_list(&mut self, close: P) -> PResult<Vec<Pat>> {
        let mut items = Vec::new();
        while !self.at_p(close) {
            items.push(self.pattern(true)?);
            if !self.eat_p(P::Comma) {
                break;
            }
        }
        self.expect_p(close)?;
        Ok(items)
    }

    fn pattern_primary(&mut self) -> PResult<Pat> {
        let lo = self.span();
        let kind = match self.tok().clone() {
            Tok::Ident(name) if name == "_" => {
                self.bump();
                PatKind::Wildcard
            }
            Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::RawStr(_) | Tok::Bytes(_) | Tok::Kw(Kw::True) | Tok::Kw(Kw::False) => {
                let e = self.primary()?;
                PatKind::Lit(Box::new(e))
            }
            Tok::P(P::Minus) => {
                self.bump();
                let e = self.primary()?;
                if !matches!(e.kind, ExprKind::Int(_) | ExprKind::Float(_)) {
                    self.diags.push(Diag::error("P0050", e.span, "only numbers can be negated in a pattern"));
                }
                let span = lo.to(e.span);
                PatKind::Lit(Box::new(Expr { kind: ExprKind::Unary { op: UnOp::Neg, expr: Box::new(e) }, span }))
            }
            Tok::P(P::LParen) => {
                self.bump();
                let mut items = Vec::new();
                let mut trailing_comma = false;
                while !self.at_p(P::RParen) {
                    items.push(self.pattern(true)?);
                    trailing_comma = self.eat_p(P::Comma);
                    if !trailing_comma {
                        break;
                    }
                }
                self.expect_p(P::RParen)?;
                if items.len() == 1 && !trailing_comma {
                    return Ok(items.pop().unwrap());
                }
                PatKind::Tuple(items)
            }
            Tok::P(P::LBracket) => {
                self.bump();
                let mut items = Vec::new();
                let mut rest = None;
                while !self.at_p(P::RBracket) {
                    if self.eat_p(P::DotDot) {
                        let name = if let Tok::Ident(_) = self.tok() { Some(self.ident()?) } else { None };
                        rest = Some(name);
                        self.eat_p(P::Comma);
                        if !self.at_p(P::RBracket) {
                            self.diags.push(Diag::error("P0051", self.span(), "`..rest` must be the last element of a list pattern"));
                            return Err(());
                        }
                        break;
                    }
                    items.push(self.pattern(true)?);
                    if !self.eat_p(P::Comma) {
                        break;
                    }
                }
                self.expect_p(P::RBracket)?;
                PatKind::List { items, rest }
            }
            Tok::Ident(name) => {
                let first = self.ident()?;
                if !is_pascal(&name) && !self.at_p(P::Dot) {
                    PatKind::Bind(name)
                } else {
                    let mut path = vec![first];
                    while self.eat_p(P::Dot) {
                        path.push(self.ident()?);
                    }
                    if !is_pascal(&path.last().unwrap().name) {
                        self.diags.push(Diag::error("P0052", lo.to(self.prev_span()), "a dotted pattern must name a variant (PascalCase)"));
                        return Err(());
                    }
                    if self.eat_p(P::LParen) {
                        let items = self.pattern_list(P::RParen)?;
                        PatKind::TupleVariant { path, items }
                    } else if self.at_p(P::LBrace) {
                        self.bump();
                        let mut fields = Vec::new();
                        let mut rest = false;
                        loop {
                            self.skip_nl();
                            if self.at_p(P::RBrace) {
                                break;
                            }
                            if self.eat_p(P::DotDot) {
                                rest = true;
                                self.skip_nl();
                                break;
                            }
                            let fname = self.ident()?;
                            let sub = if self.eat_p(P::Colon) { Some(self.pattern(true)?) } else { None };
                            fields.push((fname, sub));
                            if !self.eat_p(P::Comma) {
                                self.skip_nl();
                                break;
                            }
                        }
                        self.expect_p(P::RBrace)?;
                        PatKind::StructVariant { path, fields, rest }
                    } else {
                        PatKind::Unit { path }
                    }
                }
            }
            _ => {
                self.error_here("P0001", "pattern");
                return Err(());
            }
        };
        Ok(Pat { kind, span: lo.to(self.prev_span()) })
    }

    // ---------- expressions ----------

    pub fn expr(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = false;
        let r = self.expr_bp(0);
        self.no_struct = saved;
        r
    }

    fn expr_no_struct(&mut self) -> PResult<Expr> {
        let saved = self.no_struct;
        self.no_struct = true;
        let r = self.expr_bp(0);
        self.no_struct = saved;
        r
    }

    fn infix(&self) -> Option<(u8, u8, Infix)> {
        Some(match self.tok() {
            Tok::Kw(Kw::Or) => (1, 2, Infix::Bin(BinOp::Or)),
            Tok::Kw(Kw::And) => (3, 4, Infix::Bin(BinOp::And)),
            Tok::P(P::EqEq) => (7, 8, Infix::Bin(BinOp::Eq)),
            Tok::P(P::Ne) => (7, 8, Infix::Bin(BinOp::Ne)),
            Tok::P(P::Lt) => (7, 8, Infix::Bin(BinOp::Lt)),
            Tok::P(P::Le) => (7, 8, Infix::Bin(BinOp::Le)),
            Tok::P(P::Gt) => (7, 8, Infix::Bin(BinOp::Gt)),
            Tok::P(P::Ge) => (7, 8, Infix::Bin(BinOp::Ge)),
            Tok::P(P::DotDot) => (9, 10, Infix::Range(false)),
            Tok::P(P::DotDotEq) => (9, 10, Infix::Range(true)),
            Tok::P(P::Plus) => (11, 12, Infix::Bin(BinOp::Add)),
            Tok::P(P::Minus) => (11, 12, Infix::Bin(BinOp::Sub)),
            Tok::P(P::Star) => (13, 14, Infix::Bin(BinOp::Mul)),
            Tok::P(P::Slash) => (13, 14, Infix::Bin(BinOp::Div)),
            Tok::P(P::Percent) => (13, 14, Infix::Bin(BinOp::Rem)),
            _ => return None,
        })
    }

    fn expr_bp(&mut self, min_bp: u8) -> PResult<Expr> {
        let lo = self.span();
        let mut lhs = match self.tok() {
            Tok::Kw(Kw::Not) => {
                self.bump();
                let e = self.expr_bp(5)?;
                Expr { span: lo.to(e.span), kind: ExprKind::Unary { op: UnOp::Not, expr: Box::new(e) } }
            }
            Tok::P(P::Minus) => {
                self.bump();
                let e = self.expr_bp(15)?;
                Expr { span: lo.to(e.span), kind: ExprKind::Unary { op: UnOp::Neg, expr: Box::new(e) } }
            }
            _ => self.postfix()?,
        };
        loop {
            // `||` written as boolean or: two adjacent pipes in operator position.
            if self.at_p(P::Pipe) && *self.tok_at(1) == Tok::P(P::Pipe) && self.toks[self.pos + 1].span.lo == self.span().hi {
                if 1 < min_bp {
                    break;
                }
                let span = self.span().to(self.toks[self.pos + 1].span);
                self.diags.push(
                    Diag::error("L0001", span, "`||` is not Drift; use `or`").found("`||`").with_fix(span, "or"),
                );
                self.bump();
                self.bump();
                let rhs = self.expr_bp(2)?;
                let span = lhs.span.to(rhs.span);
                lhs = Expr { kind: ExprKind::Binary { op: BinOp::Or, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span };
                continue;
            }
            let Some((l_bp, r_bp, op)) = self.infix() else { break };
            if l_bp < min_bp {
                break;
            }
            let op_span = self.bump().span;
            let rhs = self.expr_bp(r_bp)?;
            let span = lhs.span.to(rhs.span);
            lhs = match op {
                Infix::Bin(op) => Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span },
                Infix::Range(inclusive) => Expr { kind: ExprKind::Range { lo: Box::new(lhs), hi: Box::new(rhs), inclusive }, span },
            };
            if l_bp == 7 {
                if let Some((7, _, _)) = self.infix() {
                    let second = self.span();
                    // `a < b < c` → `a < b and b < c`
                    let middle = match &lhs.kind {
                        ExprKind::Binary { rhs, .. } => self.src[rhs.span.lo as usize..rhs.span.hi as usize].to_string(),
                        _ => String::new(),
                    };
                    self.diags.push(
                        Diag::error("P0060", op_span.to(second), "comparisons cannot be chained; combine them with `and`")
                            .expected("`a < b and b < c`")
                            .with_fix(Span::empty_at(second.lo), format!("and {} ", middle)),
                    );
                    return Err(());
                }
            }
        }
        Ok(lhs)
    }

    /// True when `e` is a path that can name a struct or struct variant: `Point`, `Shape.Circle`, `http.Response`.
    fn is_type_path(e: &Expr) -> bool {
        fn path_ok(e: &Expr) -> bool {
            match &e.kind {
                ExprKind::Name(_) => true,
                ExprKind::Field { base, .. } => path_ok(base),
                _ => false,
            }
        }
        match &e.kind {
            ExprKind::Name(n) => is_pascal(n),
            ExprKind::Field { base, name } => is_pascal(&name.name) && path_ok(base),
            _ => false,
        }
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.tok() {
                Tok::P(P::Dot) => {
                    self.bump();
                    let name = match self.tok().clone() {
                        Tok::Int(_) => {
                            let span = self.span();
                            self.diags.push(Diag::error("P0061", span, "tuple fields are accessed by destructuring: `let (a, b) = t`"));
                            return Err(());
                        }
                        _ => self.ident()?,
                    };
                    let span = e.span.to(name.span);
                    e = Expr { kind: ExprKind::Field { base: Box::new(e), name }, span };
                }
                Tok::P(P::LParen) => {
                    self.bump();
                    let args = self.expr_list(P::RParen)?;
                    let span = e.span.to(self.prev_span());
                    e = Expr { kind: ExprKind::Call { callee: Box::new(e), args }, span };
                }
                Tok::P(P::LBracket) => {
                    self.bump();
                    let index = self.expr()?;
                    self.expect_p(P::RBracket)?;
                    let span = e.span.to(self.prev_span());
                    e = Expr { kind: ExprKind::Index { base: Box::new(e), index: Box::new(index) }, span };
                }
                Tok::P(P::Question) => {
                    let span = e.span.to(self.bump().span);
                    e = Expr { kind: ExprKind::Try(Box::new(e)), span };
                }
                Tok::P(P::LBrace) if !self.no_struct && Self::is_type_path(&e) => {
                    e = self.struct_lit(e)?;
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn struct_lit(&mut self, path: Expr) -> PResult<Expr> {
        self.bump(); // {
        let mut fields = Vec::new();
        loop {
            self.skip_nl();
            if self.at_p(P::RBrace) {
                break;
            }
            let name = self.ident()?;
            let value = if self.eat_p(P::Colon) { Some(self.expr()?) } else { None };
            fields.push((name, value));
            if !self.eat_p(P::Comma) {
                self.skip_nl();
                if !self.at_p(P::RBrace) {
                    self.error_here("P0001", "`,` or `}` in struct literal");
                    return Err(());
                }
            }
        }
        let hi = self.expect_p(P::RBrace)?;
        let span = path.span.to(hi);
        Ok(Expr { kind: ExprKind::StructLit { path: Box::new(path), fields }, span })
    }

    fn expr_list(&mut self, close: P) -> PResult<Vec<Expr>> {
        let saved = self.no_struct;
        self.no_struct = false;
        let mut out = Vec::new();
        loop {
            self.skip_nl();
            if self.at_p(close) {
                break;
            }
            out.push(self.expr()?);
            self.skip_nl();
            if !self.eat_p(P::Comma) {
                break;
            }
        }
        self.skip_nl();
        self.no_struct = saved;
        self.expect_p(close)?;
        Ok(out)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let kind = match self.tok().clone() {
            Tok::Int(s) => {
                self.bump();
                ExprKind::Int(s)
            }
            Tok::Float(s) => {
                self.bump();
                ExprKind::Float(s)
            }
            Tok::Kw(Kw::True) => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::Kw(Kw::False) => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Str(pieces) => {
                self.bump();
                ExprKind::Str(self.interpolate(pieces))
            }
            Tok::RawStr(s) => {
                self.bump();
                ExprKind::RawStr(s)
            }
            Tok::Bytes(b) => {
                self.bump();
                ExprKind::Bytes(b)
            }
            Tok::Ident(name) => {
                self.bump();
                ExprKind::Name(name)
            }
            Tok::Kw(Kw::Todo) => {
                self.bump();
                ExprKind::Todo
            }
            Tok::Kw(Kw::Break) => {
                self.bump();
                ExprKind::Break
            }
            Tok::Kw(Kw::Continue) => {
                self.bump();
                ExprKind::Continue
            }
            Tok::Kw(Kw::Return) => {
                self.bump();
                if self.at_nl() || self.at_p(P::RBrace) {
                    ExprKind::Return(None)
                } else {
                    ExprKind::Return(Some(Box::new(self.expr()?)))
                }
            }
            Tok::Kw(Kw::Spawn) => {
                self.bump();
                let e = self.postfix()?;
                if !matches!(e.kind, ExprKind::Call { .. }) {
                    self.diags.push(Diag::error("P0070", e.span, "`spawn` must be followed by a function call").expected("`spawn f(args)`"));
                }
                ExprKind::Spawn(Box::new(e))
            }
            Tok::P(P::LParen) => {
                self.bump();
                let saved = self.no_struct;
                self.no_struct = false;
                if self.eat_p(P::RParen) {
                    self.no_struct = saved;
                    ExprKind::Tuple(vec![])
                } else {
                    let first = self.expr()?;
                    if self.eat_p(P::Comma) {
                        let mut items = vec![first];
                        items.extend(self.expr_list(P::RParen)?);
                        self.no_struct = saved;
                        ExprKind::Tuple(items)
                    } else {
                        self.expect_p(P::RParen)?;
                        self.no_struct = saved;
                        // Parenthesized expression: keep inner node, widen span.
                        return Ok(Expr { kind: first.kind, span: lo.to(self.prev_span()) });
                    }
                }
            }
            Tok::P(P::LBracket) => {
                self.bump();
                ExprKind::List(self.expr_list(P::RBracket)?)
            }
            Tok::P(P::LBrace) => return self.brace_expr(),
            Tok::P(P::Pipe) => return self.closure(),
            Tok::Kw(Kw::If) => return self.if_expr(),
            Tok::Kw(Kw::Match) => return self.match_expr(),
            Tok::Kw(k @ (Kw::For | Kw::While | Kw::Loop | Kw::Let | Kw::Var)) => {
                let span = self.span();
                self.diags.push(Diag::error("P0071", span, format!("`{}` is a statement and cannot be used as a value", k.as_str())));
                return Err(());
            }
            _ => {
                self.error_here("P0001", "expression");
                return Err(());
            }
        };
        Ok(Expr { kind, span: lo.to(self.prev_span()) })
    }

    fn interpolate(&mut self, pieces: Vec<StrPiece>) -> Vec<StrPart> {
        let mut parts = Vec::new();
        for piece in pieces {
            match piece {
                StrPiece::Lit(s) => parts.push(StrPart::Lit(s)),
                StrPiece::Interp(span) => {
                    let lexed = lexer::lex_range(self.src, span.lo as usize, span.hi as usize);
                    let mut sub = Parser { src: self.src, toks: lexed.tokens, pos: 0, diags: lexed.diags, no_struct: false };
                    match sub.expr() {
                        Ok(e) => {
                            sub.skip_nl();
                            if !matches!(sub.tok(), Tok::Eof) {
                                sub.error_here("P0080", "end of interpolation `}`");
                            }
                            parts.push(StrPart::Expr(e));
                        }
                        Err(()) => {}
                    }
                    self.diags.extend(sub.diags);
                }
            }
        }
        parts
    }

    /// `{}` empty map, `{k: v, ...}` map, or `{ base with f: v }`.
    fn brace_expr(&mut self) -> PResult<Expr> {
        let lo = self.bump().span;
        self.skip_nl();
        if self.at_p(P::RBrace) {
            let hi = self.bump().span;
            return Ok(Expr { kind: ExprKind::Map(vec![]), span: lo.to(hi) });
        }
        let first = self.expr()?;
        if self.eat_kw(Kw::With) {
            let mut fields = Vec::new();
            loop {
                self.skip_nl();
                if self.at_p(P::RBrace) {
                    break;
                }
                let name = self.ident()?;
                self.expect_p(P::Colon)?;
                let value = self.expr()?;
                fields.push((name, value));
                if !self.eat_p(P::Comma) {
                    self.skip_nl();
                    break;
                }
            }
            let hi = self.expect_p(P::RBrace)?;
            return Ok(Expr { kind: ExprKind::With { base: Box::new(first), fields }, span: lo.to(hi) });
        }
        if !self.at_p(P::Colon) {
            let span = self.span();
            self.diags.push(
                Diag::error("P0090", span, "`{` in expression position starts a Map literal or a `{ v with ... }` update; blocks are only allowed after fn/if/else/for/while/loop/=>/closure params")
                    .expected("`:` or `with`")
                    .found(self.tok().describe()),
            );
            return Err(());
        }
        self.bump();
        let mut entries = vec![(first, self.expr()?)];
        loop {
            self.skip_nl();
            if !self.eat_p(P::Comma) {
                break;
            }
            self.skip_nl();
            if self.at_p(P::RBrace) {
                break;
            }
            let k = self.expr()?;
            self.expect_p(P::Colon)?;
            let v = self.expr()?;
            entries.push((k, v));
        }
        self.skip_nl();
        let hi = self.expect_p(P::RBrace)?;
        Ok(Expr { kind: ExprKind::Map(entries), span: lo.to(hi) })
    }

    fn closure(&mut self) -> PResult<Expr> {
        let lo = self.bump().span; // |
        let mut params = Vec::new();
        while !self.at_p(P::Pipe) {
            let pat = self.pattern(false)?;
            let ty = if self.eat_p(P::Colon) { Some(self.ty()?) } else { None };
            params.push((pat, ty));
            if !self.eat_p(P::Comma) {
                break;
            }
        }
        self.expect_p(P::Pipe)?;
        let ret = if self.eat_p(P::Arrow) { Some(self.ty()?) } else { None };
        let saved = self.no_struct;
        self.no_struct = false;
        let body = if self.at_p(P::LBrace) {
            let b = self.block()?;
            let span = b.span;
            Expr { kind: ExprKind::Block(b), span }
        } else if ret.is_some() {
            self.error_here("P0100", "`{` (a closure with a return type needs a block body)");
            self.no_struct = saved;
            return Err(());
        } else {
            self.expr()?
        };
        self.no_struct = saved;
        let span = lo.to(body.span);
        Ok(Expr { kind: ExprKind::Closure { params, ret, body: Box::new(body) }, span })
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let lo = self.bump().span; // if
        let cond = self.expr_no_struct()?;
        let then = self.block()?;
        let els = if self.eat_kw(Kw::Else) {
            if self.at_kw(Kw::If) {
                Some(Box::new(self.if_expr()?))
            } else {
                let b = self.block()?;
                let span = b.span;
                Some(Box::new(Expr { kind: ExprKind::Block(b), span }))
            }
        } else {
            None
        };
        Ok(Expr { kind: ExprKind::If { cond: Box::new(cond), then, els }, span: lo.to(self.prev_span()) })
    }

    fn match_expr(&mut self) -> PResult<Expr> {
        let lo = self.bump().span; // match
        let scrutinee = self.expr_no_struct()?;
        self.expect_p(P::LBrace)?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut arms = Vec::new();
        loop {
            self.skip_nl();
            if self.at_p(P::RBrace) || matches!(self.tok(), Tok::Eof) {
                break;
            }
            match self.arm() {
                Ok(a) => {
                    arms.push(a);
                    if self.eat_p(P::Comma) {
                        continue;
                    }
                    if self.end_of_stmt().is_err() {
                        self.sync_stmt();
                    }
                }
                Err(()) => self.sync_stmt(),
            }
        }
        self.no_struct = saved;
        let hi = self.expect_p(P::RBrace)?;
        if arms.is_empty() {
            self.diags.push(Diag::error("P0110", lo.to(hi), "`match` needs at least one arm"));
        }
        Ok(Expr { kind: ExprKind::Match { scrutinee: Box::new(scrutinee), arms }, span: lo.to(hi) })
    }

    fn arm(&mut self) -> PResult<Arm> {
        let lo = self.span();
        let pat = self.pattern(true)?;
        let guard = if self.eat_kw(Kw::If) { Some(self.expr()?) } else { None };
        if !self.at_p(P::FatArrow) {
            let found = self.tok().describe();
            let span = self.span();
            let mut d = Diag::error("P0001", span, format!("expected `=>` after match pattern, found {}", found)).expected("`=>`").found(found);
            if self.at_p(P::Arrow) || self.at_p(P::Colon) {
                d = d.with_fix(span, "=>");
            }
            self.diags.push(d);
            return Err(());
        }
        self.bump();
        let body = if self.at_p(P::LBrace) {
            let b = self.block()?;
            let span = b.span;
            Expr { kind: ExprKind::Block(b), span }
        } else {
            self.expr()?
        };
        Ok(Arm { pat, guard, span: lo.to(body.span), body })
    }
}

enum Infix {
    Bin(BinOp),
    Range(bool),
}
