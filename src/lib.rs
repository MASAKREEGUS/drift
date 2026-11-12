//! Drift bootstrap compiler.
pub mod ast;
pub mod check;
pub mod codegen;
pub mod diag;
pub mod driver;
pub mod fmt;
pub mod json;
pub mod lexer;
pub mod mcp;
pub mod parser;
pub mod toolchain;

pub const CARD: &str = include_str!("../CARD.md");
