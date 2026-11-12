//! Drift bootstrap compiler.
pub mod ast;
pub mod check;
pub mod diag;
pub mod driver;
pub mod fmt;
pub mod lexer;
pub mod parser;

pub const CARD: &str = include_str!("../CARD.md");
