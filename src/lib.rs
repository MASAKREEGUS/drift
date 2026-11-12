//! Drift bootstrap compiler.
pub mod ast;
pub mod diag;
pub mod lexer;
pub mod parser;

pub const CARD: &str = include_str!("../CARD.md");
