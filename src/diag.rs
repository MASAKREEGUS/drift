//! Diagnostics: the stable, machine-readable contract between the compiler and the agent.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: usize, hi: usize) -> Span {
        Span { lo: lo as u32, hi: hi as u32 }
    }
    pub fn to(self, other: Span) -> Span {
        Span { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }
    pub fn empty_at(pos: u32) -> Span {
        Span { lo: pos, hi: pos }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Hole,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Hole => "hole",
        }
    }
}

/// A machine-applicable edit: replace `span` with `text` (empty span = insertion).
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub span: Span,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diag {
    pub code: &'static str,
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    pub expected: Option<String>,
    pub found: Option<String>,
    pub fix: Vec<Edit>,
    /// For holes: names in scope and their types.
    pub scope: Vec<(String, String)>,
}

impl Diag {
    pub fn error(code: &'static str, span: Span, message: impl Into<String>) -> Diag {
        Diag {
            code,
            severity: Severity::Error,
            span,
            message: message.into(),
            expected: None,
            found: None,
            fix: Vec::new(),
            scope: Vec::new(),
        }
    }
    pub fn warning(code: &'static str, span: Span, message: impl Into<String>) -> Diag {
        let mut d = Diag::error(code, span, message);
        d.severity = Severity::Warning;
        d
    }
    pub fn expected(mut self, e: impl Into<String>) -> Diag {
        self.expected = Some(e.into());
        self
    }
    pub fn found(mut self, f: impl Into<String>) -> Diag {
        self.found = Some(f.into());
        self
    }
    pub fn with_fix(mut self, span: Span, text: impl Into<String>) -> Diag {
        self.fix.push(Edit { span, text: text.into() });
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Maps byte offsets to 1-based (line, column) where column counts Unicode scalar values.
pub struct LineIndex<'a> {
    src: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(src: &'a str) -> LineIndex<'a> {
        let mut starts = vec![0];
        for (i, b) in src.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { src, starts }
    }
    pub fn line_col(&self, offset: u32) -> (usize, usize) {
        let off = (offset as usize).min(self.src.len());
        let line = match self.starts.binary_search(&off) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let col = self.src[self.starts[line]..off].chars().count() + 1;
        (line + 1, col)
    }
    pub fn line_text(&self, line: usize) -> &'a str {
        let start = self.starts[line - 1];
        let end = self.starts.get(line).map(|e| e - 1).unwrap_or(self.src.len());
        &self.src[start..end.max(start)]
    }
}

pub fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn span_json(file: &str, idx: &LineIndex, span: Span) -> String {
    let (l1, c1) = idx.line_col(span.lo);
    let (l2, c2) = idx.line_col(span.hi);
    format!(
        "{{\"file\":{},\"start\":[{},{}],\"end\":[{},{}]}}",
        json_str(file),
        l1,
        c1,
        l2,
        c2
    )
}

/// One diagnostic per line (JSON Lines).
pub fn render_json(d: &Diag, file: &str, idx: &LineIndex) -> String {
    let mut s = format!(
        "{{\"code\":{},\"severity\":{},\"span\":{},\"message\":{}",
        json_str(d.code),
        json_str(d.severity.as_str()),
        span_json(file, idx, d.span),
        json_str(&d.message)
    );
    if let Some(e) = &d.expected {
        s.push_str(&format!(",\"expected\":{}", json_str(e)));
    }
    if let Some(f) = &d.found {
        s.push_str(&format!(",\"found\":{}", json_str(f)));
    }
    if !d.scope.is_empty() {
        let items: Vec<String> = d.scope.iter().map(|(k, v)| format!("{}:{}", json_str(k), json_str(v))).collect();
        s.push_str(&format!(",\"scope\":{{{}}}", items.join(",")));
    }
    if !d.fix.is_empty() {
        let edits: Vec<String> = d
            .fix
            .iter()
            .map(|e| format!("{{\"span\":{},\"text\":{}}}", span_json(file, idx, e.span), json_str(&e.text)))
            .collect();
        s.push_str(&format!(",\"fix\":{{\"edits\":[{}]}}", edits.join(",")));
    }
    s.push('}');
    s
}

pub fn render_human(d: &Diag, file: &str, idx: &LineIndex) -> String {
    let (l, c) = idx.line_col(d.span.lo);
    let mut s = format!("{}[{}] {}:{}:{}: {}\n", d.severity.as_str(), d.code, file, l, c, d.message);
    let text = idx.line_text(l);
    s.push_str(&format!("  {:>4} | {}\n", l, text));
    let (l2, c2) = idx.line_col(d.span.hi);
    let width = if l2 == l { (c2.saturating_sub(c)).max(1) } else { 1 };
    s.push_str(&format!("       | {}{}\n", " ".repeat(c - 1), "^".repeat(width)));
    if let Some(e) = &d.expected {
        s.push_str(&format!("       = expected: {}\n", e));
    }
    if let Some(f) = &d.found {
        s.push_str(&format!("       = found: {}\n", f));
    }
    if !d.scope.is_empty() {
        let items: Vec<String> = d.scope.iter().map(|(k, v)| format!("{}: {}", k, v)).collect();
        s.push_str(&format!("       = in scope: {}\n", items.join(", ")));
    }
    for e in &d.fix {
        let (l1, c1) = idx.line_col(e.span.lo);
        let (l2, c2) = idx.line_col(e.span.hi);
        if e.span.lo == e.span.hi {
            s.push_str(&format!("       = fix: insert {} at {}:{}\n", json_str(&e.text), l1, c1));
        } else {
            s.push_str(&format!("       = fix: replace {}:{}..{}:{} with {}\n", l1, c1, l2, c2, json_str(&e.text)));
        }
    }
    s
}

/// Apply non-overlapping edits (any order) to `src`.
pub fn apply_edits(src: &str, edits: &[Edit]) -> String {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.span.lo));
    let mut out = src.to_string();
    for e in sorted {
        out.replace_range(e.span.lo as usize..e.span.hi as usize, &e.text);
    }
    out
}
