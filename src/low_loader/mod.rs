//! Low Loader
//!
//! Low-level bindings to LLVM for building JIT compilers.

extern crate llvm_sys;

pub mod module;
pub mod context;
pub mod function;
pub mod builder;

/// Prelude Module
///
/// This module just re-exports useful types to help cut down on using
/// statements.
pub mod prelude {
    pub use super::context::Context;
    pub use super::module::Module;
    pub use super::function::Function;
    pub use super::builder::Builder;
}