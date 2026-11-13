//! Abstract syntax tree. Every node carries a span into the source file.

use crate::diag::Span;

#[derive(Clone, Debug, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub doc: Vec<String>,
    pub public: bool,
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    Import { path: Vec<Ident>, names: Option<Vec<Ident>> },
    Struct { name: Ident, generics: Vec<Ident>, fields: Vec<Field> },
    Enum { name: Ident, generics: Vec<Ident>, variants: Vec<Variant> },
    Const { name: Ident, ty: Type, value: Expr },
    TypeAlias { name: Ident, generics: Vec<Ident>, ty: Type },
    Fn(FnDecl),
    Examples { target: Ident, cases: Vec<Expr> },
    Test { name: String, uses: Vec<Ident>, body: Block },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: Ident,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub name: Ident,
    pub fields: VariantFields,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VariantFields {
    Unit,
    Tuple(Vec<Type>),
    Named(Vec<Field>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FnDecl {
    /// `fn Type.name(...)`: associated function of `Type`.
    pub owner: Option<Ident>,
    pub name: Ident,
    pub generics: Vec<Ident>,
    pub params: Vec<Param>,
    pub ret: Type,
    pub uses: Vec<Ident>,
    pub requires: Vec<Expr>,
    pub ensures: Vec<Expr>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub mutable: bool,
    pub name: Ident,
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub kind: TypeKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    /// `Int`, `List[T]`, `http.Request`
    Named { path: Vec<Ident>, args: Vec<Type> },
    /// `()` is the empty tuple.
    Tuple(Vec<Type>),
    Fn { params: Vec<Type>, ret: Box<Type>, uses: Vec<Ident> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StmtKind {
    Let { mutable: bool, pat: Pat, ty: Option<Type>, value: Expr },
    Assign { target: Expr, op: AssignOp, value: Expr },
    For { pat: Pat, iter: Expr, body: Block },
    While { cond: Expr, body: Block },
    Loop { body: Block },
    Assert { cond: Expr, message: Option<Expr> },
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Int(String),
    Float(String),
    Bool(bool),
    Str(Vec<StrPart>),
    RawStr(String),
    Bytes(Vec<u8>),
    Name(String),
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    /// `path { field: value, shorthand }`
    StructLit { path: Box<Expr>, fields: Vec<(Ident, Option<Expr>)> },
    With { base: Box<Expr>, fields: Vec<(Ident, Expr)> },
    Field { base: Box<Expr>, name: Ident },
    Index { base: Box<Expr>, index: Box<Expr> },
    Call { callee: Box<Expr>, args: Vec<Expr> },
    Unary { op: UnOp, expr: Box<Expr> },
    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr> },
    Range { lo: Box<Expr>, hi: Box<Expr>, inclusive: bool },
    Try(Box<Expr>),
    If { cond: Box<Expr>, then: Block, els: Option<Box<Expr>> },
    Match { scrutinee: Box<Expr>, arms: Vec<Arm> },
    Closure { params: Vec<(Pat, Option<Type>)>, ret: Option<Type>, body: Box<Expr> },
    Block(Block),
    Spawn(Box<Expr>),
    Return(Option<Box<Expr>>),
    Break,
    Continue,
    Todo,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pat {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PatKind {
    Wildcard,
    Bind(String),
    /// Literal expression: Int, Float, Str, Bool, Bytes, or negated number.
    Lit(Box<Expr>),
    Tuple(Vec<Pat>),
    /// `[a, b, ..rest]`; `rest` is Some(None) for a bare `..`.
    List { items: Vec<Pat>, rest: Option<Option<Ident>> },
    /// `Shape.Empty`, `None`
    Unit { path: Vec<Ident> },
    /// `Some(x)`, `AppError.Http(e)`
    TupleVariant { path: Vec<Ident>, items: Vec<Pat> },
    /// `Shape.Circle { r, center: c, .. }`
    StructVariant { path: Vec<Ident>, fields: Vec<(Ident, Option<Pat>)>, rest: bool },
    Or(Vec<Pat>),
}
