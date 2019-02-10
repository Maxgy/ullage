//! Expression tree parsing using Top-Down Operator Precedence
//! parsing.

#![warn(missing_docs)]

use failure;

pub mod compile;
pub mod low_loader;
pub mod meta;
pub mod sem;
pub mod syntax;

use crate::compile::*;
use crate::low_loader::targets;
use crate::syntax::*;
use docopt::Docopt;
use failure::Error;
use std::fs::File;
use std::io;
use std::io::prelude::*;
use std::path::Path;
use std::process::*;
use serde::Deserialize;

/// Usage Information
///
/// This is a [Docopt] compliant usage description of this program.
///
///  [Docopt]: http://docopt.org/
const USAGE: &str = "
Ullage Compiler

Usage:
  ullage [--version --help]
  ullage [options] [-o <outfile>] [<file>]

Options:
  -h, --help             Show this message.
  --version              Show version.
  -O, --optimise=<lvl>   Set the compilation optimisation level.
                         0 = off, 1 = low, 2 = medium, 3 = high, s = size.
  -o, --output=<out>     Write the output to <out>.
  --target=<triple>      Set the compilation target triple.
  --dumpir               Dump the LLVM IR for the module.
  --dumpast              Dump the syntax tree to stdout and exit.
  --dumptargets          Dump the available targets and exit.
  --dumptargetinfo       Dump information about the given triple.
";

/// Program Arguments
///
/// Structure to capture the command line arguments for the
/// program. This is filled in for us by Docopt.
#[derive(Debug, Deserialize)]
struct Args {
    flag_dumpast: bool,
    flag_output: Option<String>,
    flag_optimise: Option<OptFlag>,
    flag_dumpir: bool,
    flag_dumptargets: bool,
    flag_dumptargetinfo: bool,
    flag_target: Option<String>,
    arg_file: Option<String>,
}

/// Optimisation Level
///
/// Used to hold the requested optimisation level
#[derive(Debug, Deserialize)]
enum OptFlag {
    /// No optimisation
    #[serde(rename = "0")]
    Off,
    /// O1
    #[serde(rename = "1")]
    One,
    /// O2
    #[serde(rename = "2")]
    Two,
    /// O3
    #[serde(rename = "3")]
    Three,
    /// size optimisation
    #[serde(rename = "s")]
    Size,
}

impl From<OptFlag> for OptimisationLevel {
    fn from(flag: OptFlag) -> Self {
        match flag {
            OptFlag::Off => OptimisationLevel::Off,
            OptFlag::One => OptimisationLevel::Low,
            OptFlag::Two => OptimisationLevel::Med,
            OptFlag::Three => OptimisationLevel::High,
            OptFlag::Size => OptimisationLevel::Size,
        }
    }
}

/// Main
///
/// The main function for `ullage`. Parses the options and runs the
/// selected command.
fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| {
            d.help(true)
                .version(Some(format!("ullage {}", meta::version())))
                .deserialize()
        })
        .unwrap_or_else(|e| e.exit());

    if args.flag_dumptargets {
        targets::dump_targets();
        if args.arg_file.is_none() {
            exit(0);
        }
    }

    let triple = args.flag_target.unwrap_or_else(targets::get_default_triple);
    let target = targets::Target::from_triple(&triple).unwrap_or_else(|e| {
        eprintln!("error: could not create target: {}", e);
        exit(1);
    });

    if args.flag_dumptargetinfo {
        println!("{}", target);
        if args.arg_file.is_none() {
            exit(0);
        }
    }

    let output_path = &args.flag_output.unwrap_or_else(|| "a.out".to_string());
    let output_path = Path::new(&output_path);

    // Load the file into memory, so we can parse it into a syntax tree
    let source = read_input(args.arg_file).unwrap_or_else(|e| {
        eprintln!("error: could not read input: {}", e);
        exit(1)
    });

    // Parse the module
    let source = text::SourceText::new(source);
    let tree = parse::parse_tree(&source).unwrap_or_else(|e| {
        eprintln!("error: could not parse source: {}", e);
        exit(1)
    });

    // Are we just dumping the AST or compiling the whole thing?
    if args.flag_dumpast {
        println!("parsed AST: {:#?}", tree);
        exit(0);
    }

    let options = CompilationOptions::default()
        .with_dump_ir(args.flag_dumpir)
        .with_opt_level(
            args.flag_optimise
                .map_or(OptimisationLevel::Off, |o| o.into()),
        );
    let comp = match Compilation::new(&source, tree, options) {
        Ok(c) => c,
        Err(e) => handle_comp_err(&e),
    };

    // Create a compilation, and emit to the output path
    let emit_result = comp.emit(&target, &output_path);

    // Print any failures encountered and return a failure status
    if let Err(e) = emit_result {
        handle_comp_err(&e);
    }
}

/// Read the Compilation Input
///
/// If a file path was supplied then read the contents to a
/// `String`. If no file was provided then the input should be read
/// from standard input instead.
fn read_input(path: Option<String>) -> std::result::Result<String, Error> {
    let mut s = String::new();

    if let Some(path) = path {
        let input_path = Path::new(&path);
        File::open(&input_path)?.read_to_string(&mut s)?;
    } else {
        io::stdin().read_to_string(&mut s)?;
    }
    Ok(s)
}

/// Handles a Compilation Error
///
/// Prints the error to standard output and exits the process.
fn handle_comp_err(err: &CompError) -> ! {
    eprintln!("error: compilation error: {}", err);
    exit(1);
}
