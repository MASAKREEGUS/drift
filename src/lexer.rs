//! Lexer. Produces significant tokens (with doc comments attached) plus a side list of comments,
//! so the formatter can reproduce the file losslessly.
//!
//! Newline rules (CARD.md "Invariants"):
//! - inside `(` or `[` newlines are ignored, unless a `{` opened since (blocks inside closures);
//! - a newline after a continuation token (binary operator, `,`, `(`, `[`, `{`, `=>`, `->`, `|`, ...) is ignored;
//! - a newline followed by a line starting with `.` (but not `..`) is ignored;
//! - consecutive newlines collapse into one token.

use crate::diag::{Diag, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    Fn, Let, Var, If, Else, Match, For, In, While, Loop, Break, Continue, Return, Pub, Import, Uses,
    Requires, Ensures, Examples, Test, Todo, Spawn, Struct, Enum, Const, Type, With, And, Or, Not,
    True, False, Assert,
}

impl Kw {
    fn from_str(s: &str) -> Option<Kw> {
        Some(match s {
            "fn" => Kw::Fn,
            "let" => Kw::Let,
            "var" => Kw::Var,
            "if" => Kw::If,
            "else" => Kw::Else,
            "match" => Kw::Match,
            "for" => Kw::For,
            "in" => Kw::In,
            "while" => Kw::While,
            "loop" => Kw::Loop,
            "break" => Kw::Break,
            "continue" => Kw::Continue,
            "return" => Kw::Return,
            "pub" => Kw::Pub,
            "import" => Kw::Import,
            "uses" => Kw::Uses,
            "requires" => Kw::Requires,
            "ensures" => Kw::Ensures,
            "examples" => Kw::Examples,
            "test" => Kw::Test,
            "todo" => Kw::Todo,
            "spawn" => Kw::Spawn,
            "struct" => Kw::Struct,
            "enum" => Kw::Enum,
            "const" => Kw::Const,
            "type" => Kw::Type,
            "with" => Kw::With,
            "and" => Kw::And,
            "or" => Kw::Or,
            "not" => Kw::Not,
            "true" => Kw::True,
            "false" => Kw::False,
            "assert" => Kw::Assert,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Kw::Fn => "fn",
            Kw::Let => "let",
            Kw::Var => "var",
            Kw::If => "if",
            Kw::Else => "else",
            Kw::Match => "match",
            Kw::For => "for",
            Kw::In => "in",
            Kw::While => "while",
            Kw::Loop => "loop",
            Kw::Break => "break",
            Kw::Continue => "continue",
            Kw::Return => "return",
            Kw::Pub => "pub",
            Kw::Import => "import",
            Kw::Uses => "uses",
            Kw::Requires => "requires",
            Kw::Ensures => "ensures",
            Kw::Examples => "examples",
            Kw::Test => "test",
            Kw::Todo => "todo",
            Kw::Spawn => "spawn",
            Kw::Struct => "struct",
            Kw::Enum => "enum",
            Kw::Const => "const",
            Kw::Type => "type",
            Kw::With => "with",
            Kw::And => "and",
            Kw::Or => "or",
            Kw::Not => "not",
            Kw::True => "true",
            Kw::False => "false",
            Kw::Assert => "assert",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum P {
    LParen, RParen, LBracket, RBracket, LBrace, RBrace, Comma, Dot, DotDot, DotDotEq, Colon, Semi,
    Arrow, FatArrow, Pipe, Question, Plus, Minus, Star, Slash, Percent, Eq, EqEq, Ne, Lt, Le, Gt, Ge,
    PlusEq, MinusEq, StarEq, SlashEq, PercentEq, Amp, Caret, Shl, Shr,
}

impl P {
    pub fn as_str(self) -> &'static str {
        match self {
            P::LParen => "(",
            P::RParen => ")",
            P::LBracket => "[",
            P::RBracket => "]",
            P::LBrace => "{",
            P::RBrace => "}",
            P::Comma => ",",
            P::Dot => ".",
            P::DotDot => "..",
            P::DotDotEq => "..=",
            P::Colon => ":",
            P::Semi => ";",
            P::Arrow => "->",
            P::FatArrow => "=>",
            P::Pipe => "|",
            P::Question => "?",
            P::Plus => "+",
            P::Minus => "-",
            P::Star => "*",
            P::Slash => "/",
            P::Percent => "%",
            P::Amp => "&",
            P::Caret => "^",
            P::Shl => "<<",
            P::Shr => ">>",
            P::Eq => "=",
            P::EqEq => "==",
            P::Ne => "!=",
            P::Lt => "<",
            P::Le => "<=",
            P::Gt => ">",
            P::Ge => ">=",
            P::PlusEq => "+=",
            P::MinusEq => "-=",
            P::StarEq => "*=",
            P::SlashEq => "/=",
            P::PercentEq => "%=",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPiece {
    Lit(String),
    /// Byte range of the interpolated expression source inside the file.
    Interp(Span),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(String),
    Float(String),
    Str(Vec<StrPiece>),
    RawStr(String),
    Bytes(Vec<u8>),
    Kw(Kw),
    P(P),
    Newline,
    Eof,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("identifier `{}`", s),
            Tok::Int(s) | Tok::Float(s) => format!("number `{}`", s),
            Tok::Str(_) | Tok::RawStr(_) => "string literal".into(),
            Tok::Bytes(_) => "bytes literal".into(),
            Tok::Kw(k) => format!("keyword `{}`", k.as_str()),
            Tok::P(p) => format!("`{}`", p.as_str()),
            Tok::Newline => "end of line".into(),
            Tok::Eof => "end of file".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// `///` doc lines immediately preceding this token.
    pub doc: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub span: Span,
    pub text: String,
    pub doc: bool,
}

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    pub diags: Vec<Diag>,
}

pub fn lex(src: &str) -> Lexed {
    lex_range(src, 0, src.len())
}

/// Lex `src[start..end]`, producing spans relative to the whole `src` (used for interpolations).
pub fn lex_range(src: &str, start: usize, end: usize) -> Lexed {
    let mut lx = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: start,
        end,
        tokens: Vec::new(),
        comments: Vec::new(),
        diags: Vec::new(),
        delims: Vec::new(),
        pending_doc: Vec::new(),
    };
    lx.run();
    Lexed { tokens: lx.tokens, comments: lx.comments, diags: lx.diags }
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    end: usize,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    diags: Vec<Diag>,
    delims: Vec<u8>,
    pending_doc: Vec<String>,
}

fn is_continuation(t: &Tok) -> bool {
    match t {
        Tok::P(p) => !matches!(p, P::RParen | P::RBracket | P::RBrace | P::Question | P::Semi),
        Tok::Kw(k) => matches!(k, Kw::And | Kw::Or | Kw::Not | Kw::With | Kw::In),
        _ => false,
    }
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> u8 {
        if self.pos < self.end { self.bytes[self.pos] } else { 0 }
    }
    fn peek_at(&self, n: usize) -> u8 {
        if self.pos + n < self.end { self.bytes[self.pos + n] } else { 0 }
    }

    fn push(&mut self, tok: Tok, lo: usize) {
        let doc = std::mem::take(&mut self.pending_doc);
        self.tokens.push(Token { tok, span: Span::new(lo, self.pos), doc });
    }

    fn err(&mut self, code: &'static str, lo: usize, hi: usize, msg: impl Into<String>) {
        self.diags.push(Diag::error(code, Span::new(lo, hi), msg));
    }

    fn newlines_ignored(&self) -> bool {
        matches!(self.delims.last(), Some(b'(') | Some(b'['))
    }

    /// True if the next significant character starts a `.` continuation line.
    fn next_line_starts_with_dot(&self) -> bool {
        let mut i = self.pos;
        while i < self.end {
            match self.bytes[i] {
                b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                b'/' if i + 1 < self.end && self.bytes[i + 1] == b'/' => {
                    while i < self.end && self.bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'.' => return !(i + 1 < self.end && self.bytes[i + 1] == b'.'),
                _ => return false,
            }
        }
        false
    }

    fn newline(&mut self, at: usize) {
        if self.newlines_ignored() {
            return;
        }
        match self.tokens.last() {
            None => return,
            Some(t) if t.tok == Tok::Newline || is_continuation(&t.tok) => return,
            _ => {}
        }
        if self.next_line_starts_with_dot() {
            return;
        }
        self.tokens.push(Token { tok: Tok::Newline, span: Span::new(at, at + 1), doc: Vec::new() });
    }

    fn run(&mut self) {
        while self.pos < self.end {
            let c = self.peek();
            let lo = self.pos;
            match c {
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'\n' => {
                    self.pos += 1;
                    self.newline(lo);
                }
                b'/' if self.peek_at(1) == b'/' => self.comment(),
                b'0'..=b'9' => self.number(),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                    if (c == b'r' || c == b'b') && self.peek_at(1) == b'"' {
                        if c == b'r' {
                            self.raw_string();
                        } else {
                            self.byte_string();
                        }
                    } else {
                        self.ident();
                    }
                }
                b'"' => self.string(),
                _ => self.punct(),
            }
        }
        let at = self.end;
        if !self.tokens.is_empty() && self.tokens.last().map(|t| &t.tok) != Some(&Tok::Newline) {
            self.tokens.push(Token { tok: Tok::Newline, span: Span::new(at, at), doc: Vec::new() });
        }
        self.tokens.push(Token { tok: Tok::Eof, span: Span::new(at, at), doc: std::mem::take(&mut self.pending_doc) });
    }

    fn comment(&mut self) {
        let lo = self.pos;
        while self.pos < self.end && self.bytes[self.pos] != b'\n' {
            self.pos += 1;
        }
        let text = &self.src[lo..self.pos];
        let doc = text.starts_with("///") && !text.starts_with("////");
        if doc {
            let line = text[3..].strip_prefix(' ').unwrap_or(&text[3..]);
            self.pending_doc.push(line.to_string());
        }
        self.comments.push(Comment { span: Span::new(lo, self.pos), text: text.to_string(), doc });
    }

    fn ident(&mut self) {
        let lo = self.pos;
        while self.pos < self.end && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_') {
            self.pos += 1;
        }
        let s = &self.src[lo..self.pos];
        let tok = match Kw::from_str(s) {
            Some(k) => Tok::Kw(k),
            None => Tok::Ident(s.to_string()),
        };
        self.push(tok, lo);
    }

    fn number(&mut self) {
        let lo = self.pos;
        if self.peek() == b'0' && matches!(self.peek_at(1), b'x' | b'b' | b'o') {
            self.pos += 2;
            let digits_lo = self.pos;
            while self.pos < self.end && (self.bytes[self.pos].is_ascii_hexdigit() || self.bytes[self.pos] == b'_') {
                self.pos += 1;
            }
            if self.pos == digits_lo {
                self.err("L0004", lo, self.pos, "number literal has no digits after its prefix");
            }
            let text = self.src[lo..self.pos].to_string();
            self.push(Tok::Int(text), lo);
            return;
        }
        let mut float = false;
        self.digits();
        if self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
            float = true;
            self.pos += 1;
            self.digits();
        }
        if matches!(self.peek(), b'e' | b'E')
            && (self.peek_at(1).is_ascii_digit()
                || (matches!(self.peek_at(1), b'+' | b'-') && self.peek_at(2).is_ascii_digit()))
        {
            float = true;
            self.pos += 2;
            self.digits();
        }
        if self.peek().is_ascii_alphabetic() || self.peek() == b'_' {
            let bad_lo = self.pos;
            while self.pos < self.end && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_') {
                self.pos += 1;
            }
            self.err("L0004", bad_lo, self.pos, "numeric literals take no suffix; use a conversion such as `x.to_u8()`");
        }
        let text = self.src[lo..self.pos].to_string();
        self.push(if float { Tok::Float(text) } else { Tok::Int(text) }, lo);
    }

    fn digits(&mut self) {
        while self.pos < self.end && (self.bytes[self.pos].is_ascii_digit() || self.bytes[self.pos] == b'_') {
            self.pos += 1;
        }
    }

    /// Parses one escape sequence after `\`; returns the char or None after reporting an error.
    fn escape(&mut self, allow_brace: bool) -> Option<char> {
        let lo = self.pos;
        self.pos += 1; // backslash
        let c = self.peek();
        self.pos += 1;
        Some(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0' => '\0',
            b'\\' => '\\',
            b'"' => '"',
            b'{' if allow_brace => '{',
            b'}' if allow_brace => '}',
            b'u' if self.peek() == b'{' => {
                self.pos += 1;
                let hex_lo = self.pos;
                while self.pos < self.end && self.bytes[self.pos].is_ascii_hexdigit() {
                    self.pos += 1;
                }
                let hex = &self.src[hex_lo..self.pos];
                if self.peek() != b'}' {
                    self.err("L0003", lo, self.pos, "unterminated `\\u{...}` escape");
                    return None;
                }
                self.pos += 1;
                match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => ch,
                    None => {
                        self.err("L0003", lo, self.pos, format!("`\\u{{{}}}` is not a valid Unicode scalar value", hex));
                        return None;
                    }
                }
            }
            _ => {
                let hi = (lo + 2).min(self.end);
                self.diags.push(
                    Diag::error("L0003", Span::new(lo, hi), "unknown escape sequence")
                        .expected("one of \\n \\t \\r \\0 \\\\ \\\" \\{ \\} \\u{XXXX}")
                        .with_fix(Span::new(lo, lo + 1), "\\\\"),
                );
                return None;
            }
        })
    }

    fn string(&mut self) {
        let lo = self.pos;
        let triple = self.peek_at(1) == b'"' && self.peek_at(2) == b'"';
        if triple {
            self.pos += 3;
            return self.triple_string(lo);
        }
        self.pos += 1;
        let mut pieces = Vec::new();
        let mut lit = String::new();
        loop {
            if self.pos >= self.end || self.peek() == b'\n' {
                self.diags.push(
                    Diag::error("L0002", Span::new(lo, self.pos), "unterminated string literal")
                        .with_fix(Span::empty_at(self.pos as u32), "\""),
                );
                break;
            }
            match self.peek() {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    if let Some(ch) = self.escape(true) {
                        lit.push(ch);
                    }
                }
                b'{' => {
                    if !lit.is_empty() {
                        pieces.push(StrPiece::Lit(std::mem::take(&mut lit)));
                    }
                    if let Some(span) = self.interpolation() {
                        pieces.push(StrPiece::Interp(span));
                    }
                }
                b'}' => {
                    self.diags.push(
                        Diag::error("L0005", Span::new(self.pos, self.pos + 1), "unmatched `}` in string; write `\\}`")
                            .with_fix(Span::new(self.pos, self.pos + 1), "\\}"),
                    );
                    self.pos += 1;
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    lit.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        if !lit.is_empty() || pieces.is_empty() {
            pieces.push(StrPiece::Lit(lit));
        }
        self.push(Tok::Str(pieces), lo);
    }

    /// Byte index of the `}` closing an interpolation whose expression starts at `i`, stepping over
    /// nested string literals (which may interpolate too). `None` if it does not close on this line.
    fn scan_interp(&self, mut i: usize) -> Option<usize> {
        let b = self.src.as_bytes();
        let mut depth = 0usize;
        while i < self.end {
            match b[i] {
                b'\n' => return None,
                b'{' => depth += 1,
                b'}' if depth == 0 => return Some(i),
                b'}' => depth -= 1,
                b'"' if i > 0 && b[i - 1] == b'r' && (i < 2 || !(b[i - 2].is_ascii_alphanumeric() || b[i - 2] == b'_')) => {
                    i += 1;
                    while i < self.end && b[i] != b'"' {
                        if b[i] == b'\n' {
                            return None;
                        }
                        i += 1;
                    }
                    if i >= self.end {
                        return None;
                    }
                }
                b'"' => {
                    i = self.scan_nested_str(i + 1)?;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    /// Index just past the closing quote of a string literal whose body starts at `i`.
    fn scan_nested_str(&self, mut i: usize) -> Option<usize> {
        let b = self.src.as_bytes();
        while i < self.end {
            match b[i] {
                b'\n' => return None,
                b'\\' => i += 2,
                b'"' => return Some(i + 1),
                b'{' => i = self.scan_interp(i + 1)? + 1,
                _ => i += 1,
            }
        }
        None
    }

    /// At `{` inside a string: returns the span of the expression between the braces.
    fn interpolation(&mut self) -> Option<Span> {
        let open = self.pos;
        if let Some(close) = self.scan_interp(open + 1) {
            if close > open + 1 {
                self.pos = close + 1;
                return Some(Span::new(open + 1, close));
            }
        }
        self.pos += 1;
        let start = self.pos;
        let mut depth = 0usize;
        // open `(`/`[`: a quote inside a call such as `{m.get("k")}` starts a literal, it does not end the string
        let mut parens = 0usize;
        while self.pos < self.end {
            match self.peek() {
                b'\n' => break,
                b'(' | b'[' => parens += 1,
                b')' | b']' => parens = parens.saturating_sub(1),
                b'{' => depth += 1,
                b'}' if depth == 0 => {
                    let span = Span::new(start, self.pos);
                    self.pos += 1;
                    if span.lo == span.hi {
                        self.diags.push(
                            Diag::error("L0006", Span::new(open, self.pos), "empty interpolation `{}`; write `\\{\\}` for literal braces")
                                .with_fix(Span::new(open, self.pos), "\\{\\}"),
                        );
                        return None;
                    }
                    return Some(span);
                }
                b'}' => depth -= 1,
                b'"' if depth == 0 && parens == 0 => {
                    // Interpolations never contain string literals (CARD), so this quote closes the
                    // string and the `}` is missing. Leave the quote for the string lexer.
                    let span = Span::new(start, self.pos);
                    self.diags.push(
                        Diag::error("L0005", Span::new(open, self.pos), "interpolation is missing its closing `}`")
                            .expected("`}`")
                            .found("`\"`")
                            .with_fix(Span::empty_at(self.pos as u32), "}"),
                    );
                    return if span.lo == span.hi { None } else { Some(span) };
                }
                b'"' => {
                    self.diags.push(Diag::error(
                        "L0008",
                        Span::new(self.pos, self.pos + 1),
                        "string literal inside this interpolation is not closed on this line",
                    ));
                    self.pos += 1;
                    while self.pos < self.end && self.peek() != b'"' && self.peek() != b'\n' {
                        self.pos += 1;
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        self.err("L0005", open, self.pos, "unterminated interpolation: missing `}`");
        None
    }

    fn triple_string(&mut self, lo: usize) {
        // Find the closing delimiter first to compute the common indentation.
        let body_start = self.pos;
        let close = self.src[body_start..self.end].find("\"\"\"").map(|i| body_start + i);
        let Some(close) = close else {
            self.pos = self.end;
            self.err("L0002", lo, self.end, "unterminated `\"\"\"` string");
            return;
        };
        let body = &self.src[body_start..close];
        let indent = body
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .min()
            .unwrap_or(0);
        // Skip a newline right after the opening quotes.
        if self.peek() == b'\n' {
            self.pos += 1;
            self.skip_indent(indent);
        }
        let mut pieces = Vec::new();
        let mut lit = String::new();
        while self.pos < close {
            match self.peek() {
                b'\\' => {
                    if let Some(ch) = self.escape(true) {
                        lit.push(ch);
                    }
                }
                b'{' => {
                    if !lit.is_empty() {
                        pieces.push(StrPiece::Lit(std::mem::take(&mut lit)));
                    }
                    if let Some(span) = self.interpolation() {
                        pieces.push(StrPiece::Interp(span));
                    }
                }
                b'\n' => {
                    lit.push('\n');
                    self.pos += 1;
                    self.skip_indent(indent);
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    lit.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        // Drop the final newline before the closing quotes when they sit on their own line.
        if lit.ends_with('\n') && self.src[..close].rsplit('\n').next().map_or(false, |l| l.trim().is_empty()) {
            lit.pop();
        }
        if !lit.is_empty() || pieces.is_empty() {
            pieces.push(StrPiece::Lit(lit));
        }
        self.pos = close + 3;
        self.push(Tok::Str(pieces), lo);
    }

    fn skip_indent(&mut self, n: usize) {
        let mut k = 0;
        while k < n && self.peek() == b' ' {
            self.pos += 1;
            k += 1;
        }
    }

    /// `r"""..."""`: no escapes, no interpolation; indentation handled like `"""`.
    fn raw_triple_string(&mut self) {
        let lo = self.pos;
        let body_start = self.pos + 4;
        let Some(close) = self.src[body_start..self.end].find("\"\"\"").map(|i| body_start + i) else {
            self.pos = self.end;
            self.err("L0002", lo, self.end, "unterminated `r\"\"\"` string");
            return;
        };
        let body = &self.src[body_start..close];
        let text = if let Some(rest) = body.strip_prefix('\n') {
            let lines: Vec<&str> = rest.split('\n').collect();
            let indent = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start_matches(' ').len()).min().unwrap_or(0);
            let mut out: Vec<String> = lines
                .iter()
                .map(|l| {
                    let strip = (l.len() - l.trim_start_matches(' ').len()).min(indent);
                    l[strip..].to_string()
                })
                .collect();
            if lines.len() > 1 && lines.last().map_or(false, |l| l.trim().is_empty()) {
                out.pop();
            }
            out.join("\n")
        } else {
            body.to_string()
        };
        self.pos = close + 3;
        self.push(Tok::RawStr(text), lo);
    }

    fn raw_string(&mut self) {
        if self.src[self.pos..self.end].starts_with("r\"\"\"") {
            return self.raw_triple_string();
        }
        let lo = self.pos;
        self.pos += 2;
        let start = self.pos;
        while self.pos < self.end && self.peek() != b'"' && self.peek() != b'\n' {
            self.pos += 1;
        }
        let text = self.src[start..self.pos].to_string();
        if self.peek() == b'"' {
            self.pos += 1;
        } else {
            self.diags.push(
                Diag::error("L0002", Span::new(lo, self.pos), "unterminated raw string")
                    .with_fix(Span::empty_at(self.pos as u32), "\""),
            );
        }
        self.push(Tok::RawStr(text), lo);
    }

    fn byte_string(&mut self) {
        let lo = self.pos;
        self.pos += 2;
        let mut out = Vec::new();
        loop {
            if self.pos >= self.end || self.peek() == b'\n' {
                self.diags.push(
                    Diag::error("L0002", Span::new(lo, self.pos), "unterminated bytes literal")
                        .with_fix(Span::empty_at(self.pos as u32), "\""),
                );
                break;
            }
            match self.peek() {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' if self.peek_at(1) == b'x' => {
                    let h = &self.src[(self.pos + 2).min(self.end)..(self.pos + 4).min(self.end)];
                    match u8::from_str_radix(h, 16) {
                        Ok(b) if h.len() == 2 => {
                            out.push(b);
                            self.pos += 4;
                        }
                        _ => {
                            self.err("L0003", self.pos, (self.pos + 4).min(self.end), "`\\x` needs two hex digits");
                            self.pos += 2;
                        }
                    }
                }
                b'\\' => {
                    if let Some(ch) = self.escape(false) {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                }
                b if b.is_ascii() => {
                    out.push(b);
                    self.pos += 1;
                }
                _ => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    let p = self.pos;
                    self.pos += ch.len_utf8();
                    self.err("L0007", p, self.pos, "bytes literals are ASCII only; use `\\xNN`");
                }
            }
        }
        self.push(Tok::Bytes(out), lo);
    }

    fn punct(&mut self) {
        let lo = self.pos;
        let c = self.peek();
        let n = self.peek_at(1);
        let (p, len) = match (c, n) {
            (b'-', b'>') => (P::Arrow, 2),
            (b'=', b'>') => (P::FatArrow, 2),
            (b'=', b'=') => (P::EqEq, 2),
            (b'!', b'=') => (P::Ne, 2),
            (b'<', b'<') => (P::Shl, 2),
            (b'>', b'>') => (P::Shr, 2),
            (b'<', b'=') => (P::Le, 2),
            (b'>', b'=') => (P::Ge, 2),
            (b'+', b'=') => (P::PlusEq, 2),
            (b'-', b'=') => (P::MinusEq, 2),
            (b'*', b'=') => (P::StarEq, 2),
            (b'/', b'=') => (P::SlashEq, 2),
            (b'%', b'=') => (P::PercentEq, 2),
            (b'.', b'.') if self.peek_at(2) == b'=' => (P::DotDotEq, 3),
            (b'.', b'.') => (P::DotDot, 2),
            (b'(', _) => (P::LParen, 1),
            (b')', _) => (P::RParen, 1),
            (b'[', _) => (P::LBracket, 1),
            (b']', _) => (P::RBracket, 1),
            (b'{', _) => (P::LBrace, 1),
            (b'}', _) => (P::RBrace, 1),
            (b',', _) => (P::Comma, 1),
            (b'.', _) => (P::Dot, 1),
            (b':', _) => (P::Colon, 1),
            (b';', _) => (P::Semi, 1),
            (b'|', _) => (P::Pipe, 1),
            (b'&', b) if b != b'&' => (P::Amp, 1),
            (b'^', _) => (P::Caret, 1),
            (b'?', _) => (P::Question, 1),
            (b'+', _) => (P::Plus, 1),
            (b'-', _) => (P::Minus, 1),
            (b'*', _) => (P::Star, 1),
            (b'/', _) => (P::Slash, 1),
            (b'%', _) => (P::Percent, 1),
            (b'=', _) => (P::Eq, 1),
            (b'<', _) => (P::Lt, 1),
            (b'>', _) => (P::Gt, 1),
            _ => {
                let ch = self.src[self.pos..].chars().next().unwrap();
                self.pos += ch.len_utf8();
                let (msg, fix) = match ch {
                    '&' if n == b'&' => ("`&&` is not Drift; use `and`", Some(("and", 2))),
                    '|' => unreachable!(),
                    '!' => ("`!` is not Drift; use `not`", Some(("not ", 1))),
                    _ => ("unexpected character", None),
                };
                let mut d = Diag::error("L0001", Span::new(lo, self.pos), msg).found(format!("`{}`", ch));
                if let Some((text, len)) = fix {
                    self.pos = lo + len;
                    d = d.with_fix(Span::new(lo, lo + len), text);
                    self.diags.push(d);
                    // Recover as if the fix were applied, so no cascading diagnostics.
                    let kw = if len == 2 { Kw::And } else { Kw::Not };
                    self.push(Tok::Kw(kw), lo);
                    return;
                }
                self.diags.push(d);
                return;
            }
        };
        // `||` used as boolean or: report with a fix, but keep lexing as two pipes (empty closure params).
        self.pos += len;
        match p {
            P::LParen => self.delims.push(b'('),
            P::LBracket => self.delims.push(b'['),
            P::LBrace => self.delims.push(b'{'),
            P::RParen | P::RBracket | P::RBrace => {
                self.delims.pop();
            }
            _ => {}
        }
        self.push(Tok::P(p), lo);
    }
}
