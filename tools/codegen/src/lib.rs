//! Legacy-header parser and Rust opcode emitter.
//!
//! Development-time only: nothing in the shipped services depends on this crate.

pub mod emit;
pub mod parse;

pub use emit::{EmitSpec, emit};
pub use parse::{EnumDef, Enumerator, ParseError, parse_enum};
