//! Semantic Transforms
//!
//! This module contains the logic for converting a syntax expression
//! tree into a semantic one. The main entry point for this module is
//! the [`transform_expression`] function.
//!
//! [`transform_expression`]: ./function.transform_expression.html

use syntax::{Constant, Expression as SyntaxExpr};

use super::types::{BuiltinType, Typ};
use super::tree::*;

/// Transform Expression
///
/// Convert a syntax expression into a symantic one.
pub fn transform_expression(expr: SyntaxExpr) -> Expression {
    match expr {
        SyntaxExpr::Literal(c) => {
            let typ = Typ::Builtin(match &c {
                &Constant::Bool(_) => BuiltinType::Bool,
                &Constant::Number(_) => BuiltinType::Number,
                &Constant::String(_) => BuiltinType::String,
            });
            Expression::new(ExpressionKind::Literal(c), Some(typ))
        }
        SyntaxExpr::Sequence(seq) => {
            let transformed = seq.into_iter()
                .map(transform_expression)
                .collect::<Vec<_>>();
            let typ = transformed.last().and_then(|e| e.typ.clone());
            Expression::new(ExpressionKind::Sequence(transformed), typ)
        }
        expr => Expression::new(ExpressionKind::Fixme(expr), None),
    }
}