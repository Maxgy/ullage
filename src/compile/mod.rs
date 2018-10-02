//! This module contians the code required to compile a parsed tree
//! down to LLVM bytecode.

use crate::low_loader::prelude::*;
use crate::sem;
use std::path::Path;
use std::process::Command;
use crate::syntax;
use tempfile::Builder;

pub use self::error::{Error, Result};

pub mod error;

mod lower;
mod lower_context;

/// Add the Core Declarations to the Module
///
/// This method is responsible for making sure that
/// declarations/definitions of any builtin funtions are emitted.
fn add_core_decls(ctx: &mut Context, module: &mut Module) -> Result<()> {
    add_printf_decl(ctx, module);
    module.add_global(ctx.const_str("%d\n"), "printf_num_format");
    module.add_global(ctx.const_str("%s\n"), "printf_cstr_format");
    module.add_global(ctx.const_str("true"), "print_true");
    module.add_global(ctx.const_str("false"), "print_false");
    Ok(())
}

/// Add a Printf Declaration to the Module
///
/// Creates a new function in the given module which maps to the
/// `printf` function. This will be used by the `print` operator
/// to write output.
fn add_printf_decl(ctx: &mut Context, module: &mut Module) {
    let mut params = [ctx.cstr_type()];
    let int_type = ctx.int_type(32);
    ctx.add_varargs_function(module, "printf", int_type, &mut params);
}

/// Compilation State
///
/// Encompases the inputs and settings for a given compilation.
pub struct Compilation {
    /// The `Expression`s which are being compiled.
    expr: sem::Expression,
}

impl Compilation {
    /// Create a new compilation
    pub fn new(expr: syntax::Expression) -> Result<Self> {
        let mut trans_sess = sem::SemCtx::new();
        let sem_expr = sem::transform_expression(&mut trans_sess, expr)?;
        Ok(Compilation { expr: sem_expr })
    }

    /// Emit
    ///
    /// Performs the compilation, emitting the results to the given file.
    pub fn emit(self, output_path: &Path, dump_ir: bool) -> Result<()> {
        let mut ctx = Context::new();
        let name = output_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("fallback_module_name");
        let mut module = ctx.add_module(name);

        add_core_decls(&mut ctx, &mut module)?;

        let fun = {
            let mut lower_ctx = lower_context::LowerContext::new(&mut ctx, &mut module);
            lower_ctx.add_core_types();
            lower::lower_as_main(&mut lower_ctx, self.expr)?
        };

        // Check what we have, and dump it to the screen
        if dump_ir {
            module.dump();
        }
        fun.verify_or_panic();

        // Create a tempdir to write the LLVM IR to
        let temp_file = Builder::new().prefix("ullage").suffix(".ll").tempfile()?;

        module.write_to_file(temp_file.path())?;

        // Shell out to Clang to link the final assembly
        let output = Command::new("clang")
            .arg(temp_file.path())
            .arg("-o")
            .arg(output_path)
            .output()?;
        let status = output.status;

        if status.success() {
            Ok(())
        } else {
            Err(Error::Generic(match status.code() {
                Some(c) => format!("clang failed with exit status: {}", c),
                None => "clang failed with unknown exit status".into(),
            }))
        }
    }
}
