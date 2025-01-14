//! Expression Binder
//!
//! This module contains the syntax binder. It's job is to walk the
//! incoming syntax tree and bind each part to produce the semantic
//! tree.
//!
//! At any given level the bind consists of two main steps. First the
//! list of expressions is walked to define any items that need to be
//! made available for mutual recursion. Once this walk is complete a
//! second traveral visits each item and binds symbol and name
//! information. These two phases are represented in the `declare_*`
//! and `bind_*` groups of methods.
//!
//! A bind of a given tree should always produce _some_ result. Parts
//! of the tree which can't be properly bound will result in
//! `Expression::Error` values. If type information can't be resolved
//! for a given part of the tree then the `Typ::Error` type is
//! used. Bound trees which contain either of these error values
//! should also produce diagnostics in the bind. Failure to do so is a
//! bug in the binder.

use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::default::Default;

use super::operators;
use super::tree::{FnDecl, VarDecl};
use super::{BuiltinType, Expression, ExpressionKind, Typ};
use crate::diag::Diagnostic;
use crate::syntax::{
    self,
    text::{Ident, SourceText, Span},
    Constant, InfixOp, PrefixOp, SyntaxNode, TokenKind, TypeRef, VarStyle,
};

/// An item that can appear in a `Scope`
///
/// Symbols represent the different kinds of items that can be bound
/// to names in a given scope. Examples are function arguments, local
/// variables, and function declarations.
#[derive(Debug, PartialEq, Clone)]
pub enum Symbol {
    /// Function argument or local variable
    Variable(VarStyle, Typ),
    /// A Function declaration
    Function(Vec<Typ>, Typ),
    /// A type
    Type(Typ),
}

/// Declaration Scope
///
/// Holds the declared items at a given level in the scope stack
/// during a bind. Once a scope has an item with a given name declared
/// a new item can't be inserted to overwrite it. The return value of
/// `try_declare` exposes the success or failure of declaring an item.
///
/// # Examples
///
/// ```
/// # let interner = Interner::new();
/// let mut scope = Scope::new();
///
/// assert!(scope.try_declare(interner.intern("foo"), Symbol::Type(Typ::Unit));
///
/// // we can look the symbols up later
/// assert_eq!(None, scope.lookup(interner.intern("bar")));
/// assert_eq!(Some(Symbol::Type(Typ::Unit)), scope.lookup(interner.intern("foo")));
/// ```
#[derive(Default)]
pub struct Scope {
    /// Symbols declared in this scope
    symbols: HashMap<Ident, Symbol>,
}

impl Scope {
    /// Create an empty scope
    pub fn new() -> Self {
        Default::default()
    }

    /// Lookup a Symbol from the scope
    ///
    /// Searches the current scope, and any parent scopes, for the
    /// given identifier. If any symbol is bound to the idnetifier it
    /// is returned otherwise `None` is returned.
    pub fn lookup(&self, ident: Ident) -> Option<Symbol> {
        self.symbols.get(&ident).cloned()
    }

    /// Try to declare a Symbol in this scope
    ///
    /// Attempts to insert the given symbol into the symbol
    /// table. Returns `true` if the symbol was inserted succesfully.
    ///
    /// # Examples
    ///
    /// ```
    /// # let id = Interner::new().intern("foo");
    /// # let sym = Symbol::Type(Typ::Unit);
    /// # let mut scope = Scope::new();
    /// assert!(scope.try_declare(id), sym);
    /// assert!(!scope.try_declare(id));
    /// ```
    pub fn try_declare(&mut self, ident: Ident, sym: Symbol) -> bool {
        match self.symbols.entry(ident) {
            Entry::Occupied(_) => false,
            Entry::Vacant(v) => {
                v.insert(sym);
                true
            }
        }
    }
}

/// Stack of scopes
///
/// When binding we require a nested stack of scopes. While walking
/// through a tree the `Binder` will use this stack to record the
/// currently visible scopes.
///
/// Lookups in the scope stack start at the innermost scope and work
/// outward. Once an item is found it is returned. This allows items
/// in inner scopes to shadow those in outer ones. Seen as all items
/// are in the same namespace at the moment this allows variables to
/// shadow functions with the same name and vice-versa.
///
/// The stack is maipulated with the `push()` and `pop()` methods.
pub struct ScopeStack(Vec<Scope>);

impl ScopeStack {
    /// Create a new scope stack with the given scope as the base
    pub fn new(base: Scope) -> Self {
        ScopeStack(vec![base])
    }

    /// Lookup a symbol in the scope stack
    ///
    /// Starts at the innermost 'current' scope and walks outward
    /// searching for a `Symbol` bound to the given `id`. If no symbol
    /// is found then `None` is returned, otherwise a copy of the
    /// symbol is returned.
    pub fn lookup(&self, id: Ident) -> Option<Symbol> {
        self.0.iter().rev().find_map(|s| s.lookup(id))
    }

    /// Get the scope at the top of the stack
    ///
    /// When binding only the current top-most scope can be
    /// manipulated. This method allows mutable access to the
    /// innermost scope to insert new symbols.
    pub fn current_mut(&mut self) -> &mut Scope {
        self.0.last_mut().unwrap()
    }

    /// Push a new scope on to the stack
    ///
    /// Ownership of the scope is passed to the `ScopeStack`. Once the
    /// scope is popped from the stack ownership is returned.
    pub fn push(&mut self, scope: Scope) {
        self.0.push(scope)
    }

    /// Pop the current scope from the stack
    ///
    /// The scope that was popped is returned. If the stack is empty
    /// then `None` is retured. In a normal bind calls to `push` and
    /// `pop` should be ballanced.
    pub fn pop(&mut self) -> Option<Scope> {
        self.0.pop()
    }

    /// Flatten the function declarations current scope into a single
    /// target scope.
    ///
    /// This is intended for creating a new base scope for child items
    /// (functions etc.) Without this import mutual recursion wouldn't
    /// be possible as the child items wouldn't be able to see their
    /// siblings.
    ///
    /// Scope visibility and shadowing is preserved.
    pub fn flatten_decls_into(&self, target: &mut Scope) {
        for scope in self.0.iter().rev() {
            for (id, sym) in scope.symbols.iter() {
                if let Symbol::Function(..) = *sym {
                    target.try_declare(*id, sym.clone());
                }
            }
        }
    }
}

/// Holds the scope information and declared items for an ongoing
/// binding operation.
///
/// A syntax binder can is used to tranform a syntax expression
pub struct Binder {
    /// The current scope
    scopes: ScopeStack,
    /// The diagnostics for the current bind
    diagnostics: Vec<Diagnostic>,
}

impl Binder {
    /// Create Binder for the Given Scope
    pub fn new(scope: Scope) -> Self {
        Binder {
            scopes: ScopeStack::new(scope),
            diagnostics: Vec::new(),
        }
    }

    /// Bind an Expression
    ///
    /// Converts a syntax expression into a semantic one by binding it
    /// in the binder's current scope.
    pub fn bind_tree(&mut self, tree: syntax::SyntaxTree<'_>) -> Expression {
        let source = tree.source();
        add_builtin_types(self.scopes.current_mut(), source);
        let (expr, _end) = tree.into_parts();
        self.declare_expression(&expr);
        self.bind_expression(&expr, source)
    }

    /// Declare any items in the current expression that should be
    /// visible in this scope.
    ///
    /// This is called when entering a new block before the body of
    /// the block is bound. It adds delclarations for any items that
    /// should be mutually recursive into the scope so that they are
    /// available to the following bind.
    pub fn declare_expression(&mut self, expression: &syntax::Expression) {
        use syntax::Expression::*;
        match *expression {
            Function(ref func) => self.declare_function(func),
            Sequence(ref seq) => {
                for expr in seq.iter() {
                    self.declare_expression(expr);
                }
            }
            Grouping(ref group) => self.declare_expression(&group.inner),
            _ => {}
        }
    }

    /// Builds out the type for the function and creates an entry in
    /// the current symbol table for it.
    ///
    /// Function binding is done in two parts. First a declaration
    /// symbol for the function is inserted into the scope by this
    /// method. Later when each expression in the tree is visited
    /// again for binding the body of the function is bound in a new
    /// child scope.
    pub fn declare_function(&mut self, func: &syntax::FunctionExpression) {
        let param_tys = func
            .params
            .iter()
            .map(|param| {
                param
                    .as_inner()
                    .typ
                    .as_ref()
                    .map(|t| self.bind_type(&t.type_ref))
                    .unwrap_or(Typ::Error)
            })
            .collect();
        let ret_ty = self.bind_type(&func.return_type.type_ref);
        self.scopes
            .current_mut()
            .try_declare(func.identifier, Symbol::Function(param_tys, ret_ty));
    }

    /// Bind a Single Expression
    ///
    /// This examines the expression kind and delegates to the
    /// appropriate `bind_*` method.
    pub fn bind_expression(
        &mut self,
        expression: &syntax::Expression,
        source: &SourceText,
    ) -> Expression {
        use syntax::Expression::*;
        match *expression {
            Identifier(ref ident) => self.bind_identifier(ident, source),
            Literal(ref lit) => self.bind_literal(lit),
            Prefix(ref pref) => self.bind_prefix(pref, source),
            Infix(ref innie) => self.bind_infix(innie, source),
            Call(ref call) => self.bind_call(call, source),
            Index(ref index) => self.bind_index(index, source),
            IfThenElse(ref if_else_expr) => self.bind_if_else(if_else_expr, source),
            Function(ref func) => self.bind_function(func, source),
            Loop(ref loop_expr) => self.bind_loop(loop_expr, source),
            Sequence(ref exprs) => self.bind_sequence(&exprs[..], source),
            Print(ref print) => self.bind_print(print, source),
            Declaration(ref decl) => self.bind_declaration(decl, source),
            Grouping(ref group) => self.bind_expression(&group.inner, source),
        }
    }

    /// Bind a refernece to an identifier
    ///
    /// # Errors
    ///
    /// If there is no symbol in the symbol table for the identifier
    /// then a diagnostic is raised.
    pub fn bind_identifier(
        &mut self,
        ident: &syntax::IdentifierExpression,
        source: &SourceText,
    ) -> Expression {
        if let Some(sym) = self.scopes.lookup(ident.ident) {
            let id_str = source.interned_value(ident.ident);
            let typ = match sym {
                Symbol::Variable(_, t) => Some(t),
                Symbol::Function(..) => Some(Typ::Function(ident.ident)),
                // FIXME: First-class types?
                Symbol::Type(..) => None,
            };
            Expression::new(ExpressionKind::Identifier(id_str), typ)
        } else {
            self.diagnostics.push(Diagnostic::new(
                format!(
                    "Can't find '{}' in this scope",
                    source.interned_value(ident.ident)
                ),
                ident.token.span(),
            ));
            Expression::error()
        }
    }

    /// Bind a literal value
    pub fn bind_literal(&mut self, lit: &syntax::LiteralExpression) -> Expression {
        let constant_value = lit.value.clone();
        let typ = Typ::Builtin(match constant_value {
            Constant::Bool(_) => BuiltinType::Bool,
            Constant::Number(_) => BuiltinType::Number,
            Constant::String(_) => BuiltinType::String,
        });
        Expression::new(ExpressionKind::Literal(constant_value), Some(typ))
    }

    /// Prefix operation
    pub fn bind_prefix(
        &mut self,
        pref: &syntax::PrefixExpression,
        source: &SourceText,
    ) -> Expression {
        let bound_inner = self.bind_expression(&pref.inner, source);
        // TODO: Do we wnat some kind of type table for these
        //       operations like we have for infix operators?
        let typ = bound_inner.typ;
        Expression::new(ExpressionKind::Prefix(pref.op, Box::new(bound_inner)), typ)
    }

    /// Bind an infix operator expression
    ///
    /// There are two main kinds of infix operators, assignment and
    /// standard. For assignment this method delgates to `bind_assign`
    /// for the real bind.
    ///
    /// # Errors
    ///
    /// If an assignment expression has an invalid lvalue or if there
    /// is no operator which accepts the given arguments then a
    /// diagnostic is raised.
    pub fn bind_infix(
        &mut self,
        infix: &syntax::InfixOperatorExpression,
        source: &SourceText,
    ) -> Expression {
        if infix.op == InfixOp::Assign {
            if let syntax::Expression::Identifier(ref id) = *infix.left {
                self.bind_assign(id, infix, source)
            } else {
                self.diagnostics.push(Diagnostic::new(
                    "left hand side of an assignment must be an identifier",
                    infix.left.span(),
                ));
                Expression::error()
            }
        } else {
            let lhs = self.bind_expression(&infix.left, source);
            let rhs = self.bind_expression(&infix.right, source);

            let lhs_typ = lhs.typ.unwrap_or(Typ::Unknown);
            let rhs_typ = rhs.typ.unwrap_or(Typ::Unknown);

            // Look the operator up in the operator table to check if
            // it is permissable and what the reutnr type is.
            match operators::find_builtin_op(infix.op, lhs_typ, rhs_typ) {
                Some(operator) => Expression::new(
                    ExpressionKind::Infix(Box::new(lhs), infix.op, Box::new(rhs)),
                    Some(operator.result_typ),
                ),
                None => {
                    self.diagnostics.push(Diagnostic::new(
                        format!("Use of operator `{:?}` with invalid arguments", infix.op),
                        Span::enclosing(infix.left.span(), infix.right.span()),
                    ));
                    Expression::error()
                }
            }
        }
    }

    /// Bind assignment to a given indentifier expression
    ///
    /// The given infix operator should be an assignment
    /// expression. The bound result is the assignment of the rhs of
    /// that expression to the given identifier.
    ///
    /// # Errors
    ///
    /// If the item cannot be assigned to, or the type or mutability
    /// do not match then a diagnostic is raised.
    fn bind_assign(
        &mut self,
        id: &syntax::IdentifierExpression,
        infix: &syntax::InfixOperatorExpression,
        source: &SourceText,
    ) -> Expression {
        match self.scopes.lookup(id.ident) {
            Some(Symbol::Variable(style, typ)) => {
                if style != VarStyle::Mutable {
                    self.diagnostics.push(Diagnostic::new(
                        format!(
                            "Can't assign to '{}', it isn't mutable",
                            source.interned_value(id.ident)
                        ),
                        infix.op_token.span(),
                    ));
                }
                let rhs = self.bind_expression(&infix.right, source);
                let resolved_ty = rhs.typ.unwrap_or(typ);
                if resolved_ty != typ {
                    self.diagnostics.push(Diagnostic::new(
                        format!(
                            "Type mismatch in assignment to '{}' ",
                            source.interned_value(id.ident)
                        ),
                        infix.op_token.span(),
                    ));
                }
                Expression::new(
                    ExpressionKind::Assignment(source.interned_value(id.ident), Box::new(rhs)),
                    Some(resolved_ty),
                )
            }
            Some(_) => {
                self.diagnostics.push(Diagnostic::new(
                    format!(
                        "Can't write to '{}' as it isn't a variable.",
                        source.interned_value(id.ident)
                    ),
                    id.token.span(),
                ));
                Expression::error()
            }
            None => {
                self.diagnostics.push(Diagnostic::new(
                    format!("Can't assign to '{}'", source.interned_value(id.ident)),
                    id.token.span(),
                ));
                Expression::error()
            }
        }
    }

    /// Bind a function call expression
    ///
    /// This binds the arguments to the call and then checks that the
    /// callee is a function.
    ///
    /// # Errors
    ///
    /// If the called item is not a function, or the type or arity of
    /// the function arguments do not match the declared paramters
    /// then a diagnostic is raised.
    pub fn bind_call(&mut self, call: &syntax::CallExpression, source: &SourceText) -> Expression {
        let callee = self.bind_expression(&call.callee, source);
        match callee.typ {
            Some(Typ::Function(id)) => match self.scopes.lookup(id) {
                Some(Symbol::Function(param_tys, ret_ty)) => {
                    let param_count = param_tys.len();
                    let arg_count = call.arguments.len();

                    if arg_count < param_count {
                        self.diagnostics.push(Diagnostic::new(
                            "Too few arguments to call",
                            Span::enclosing(call.open_paren.span(), call.close_paren.span()),
                        ));
                    }

                    if arg_count > param_count {
                        let start = call.arguments[param_count].span().start();
                        self.diagnostics.push(Diagnostic::new(
                            "Too many arguments to call",
                            Span::new(start, call.close_paren.span().start()),
                        ))
                    }

                    let args: Vec<_> = call
                        .arguments
                        .iter()
                        .zip(param_tys)
                        .map(|(arg, param)| {
                            let bound_arg = self.bind_expression(arg, source);
                            if bound_arg.typ != Some(param) {
                                self.diagnostics.push(Diagnostic::new(
                                    format!(
                                        "Invalid argument. Expected '{}' but found '{}'",
                                        param.name(),
                                        bound_arg.typ.unwrap_or(Typ::Unknown).name()
                                    ),
                                    arg.span(),
                                ))
                            }
                            bound_arg
                        })
                        .collect();

                    Expression::new(ExpressionKind::Call(Box::new(callee), args), Some(ret_ty))
                }
                _ => {
                    unreachable!();
                }
            },
            _ => {
                self.diagnostics.push(Diagnostic::new(
                    "Called item is not a function",
                    call.callee.span(),
                ));
                Expression::error()
            }
        }
    }

    /// Bind an index/slice expression
    pub fn bind_index(
        &mut self,
        index: &syntax::IndexExpression,
        source: &SourceText,
    ) -> Expression {
        let _indexee = self.bind_expression(&index.indexee, source);
        let _inddex = self.bind_expression(&index.index, source);

        // TODO: Index expressions.
        self.diagnostics.push(Diagnostic::new(
            "Index expressions are not yet supported",
            Span::enclosing(index.open_bracket.span(), index.close_bracket.span()),
        ));
        Expression::error()
    }

    /// Bind a if then else expression
    pub fn bind_if_else(
        &mut self,
        if_else: &syntax::IfElseExpression,
        source: &SourceText,
    ) -> Expression {
        let cond = self.bind_expression(&if_else.cond, source);
        let if_true = self.bind_expression(&if_else.if_true, source);
        let if_false = self.bind_expression(&if_else.if_false, source);

        // Check that the condition type is bool
        //
        // TODO: Bind a conversion to bool here to allow `if` to
        //       coerce values to `Bool`
        let cond_ty = cond.typ.unwrap_or(Typ::Unknown);
        if cond_ty != Typ::Builtin(BuiltinType::Bool) {
            self.diagnostics.push(Diagnostic::new(
                format!(
                    "Condition expression should be 'Bool' but is '{}'",
                    cond_ty.name()
                ),
                if_else.cond.span(),
            ));
        }

        let typ = if_true.typ;
        let true_typ = if_true.typ.unwrap_or(Typ::Unknown);
        let false_typ = if_false.typ.unwrap_or(Typ::Unknown);

        // TODO: This doesn't deal with the case of both types being
        //       missing. Hopefully we can get rid of optional types
        //       on the bound tree and rely on `Typ::Unknown` so we
        //       don't have to handle such cases.
        if true_typ != false_typ {
            self.diagnostics.push(Diagnostic::new(
                format!(
                    "If and else have mismatched types. '{}' and '{}'",
                    true_typ.name(),
                    false_typ.name()
                ),
                Span::enclosing(if_else.if_true.span(), if_else.if_false.span()),
            ));
        }

        Expression::new(
            ExpressionKind::IfThenElse(Box::new(cond), Box::new(if_true), Box::new(if_false)),
            typ,
        )
    }

    /// Bind a function definition
    ///
    /// This binds the body of the function using a new binder. The
    /// function type should already have been added to the current
    /// scope by `declare_function`.
    ///
    /// # Errors
    ///
    /// Any errors from the bindig of the function body are added to
    /// this `Binder`'s diagnostics.
    pub fn bind_function(
        &mut self,
        func: &syntax::FunctionExpression,
        source: &SourceText,
    ) -> Expression {
        let mut parent_scope = Scope::new();
        self.scopes.flatten_decls_into(&mut parent_scope);

        let mut seen_idents = HashSet::new();
        let params = func
            .params
            .iter()
            .map(|p| {
                let p = p.as_inner();
                let typ = match p.typ.as_ref() {
                    Some(anno) => self.bind_type(&anno.type_ref),
                    None => {
                        self.diagnostics.push(Diagnostic::new(
                            format!("Parameter '{}' missing type", source.interned_value(p.id)),
                            p.id_tok.span(),
                        ));
                        Typ::Error
                    }
                };
                if !seen_idents.insert(p.id) {
                    self.diagnostics.push(Diagnostic::new(
                        format!(
                            "Duplicate function parameter '{}'",
                            source.interned_value(p.id)
                        ),
                        p.id_tok.span(),
                    ));
                }
                parent_scope.try_declare(p.id, Symbol::Variable(VarStyle::Mutable, typ));
                VarDecl {
                    ident: source.interned_value(p.id),
                    ty: Some(typ),
                }
            })
            .collect();

        let mut binder = Binder::new(parent_scope);
        let bound_body = binder.bind_block(&func.body, source);
        let ret_ty = self.bind_type(&func.return_type.type_ref);

        // Report any diagnostics from the child binder in this bind.
        self.diagnostics.append(&mut binder.take_diagnostics());

        Expression::new(
            ExpressionKind::Function(FnDecl {
                ident: source.interned_value(func.identifier),
                ret_ty,
                params,
                body: Box::new(bound_body),
            }),
            Some(Typ::Error),
        )
    }

    /// Bind a loop expression
    pub fn bind_loop(
        &mut self,
        loop_expr: &syntax::LoopExpression,
        source: &SourceText,
    ) -> Expression {
        let mut condition = self.bind_expression(&loop_expr.condition, source);
        if loop_expr.kw_token.kind == TokenKind::Word(Ident::Until) {
            let typ = condition.typ;
            condition = Expression::new(
                ExpressionKind::Prefix(PrefixOp::Not, Box::new(condition)),
                typ,
            );
        }
        let body = self.bind_block(&loop_expr.body, source);
        Expression::new(
            ExpressionKind::Loop(Box::new(condition), Box::new(body)),
            Some(Typ::Unit),
        )
    }

    /// Bind a sequence of expressions
    pub fn bind_sequence(
        &mut self,
        exprs: &[syntax::Expression],
        source: &SourceText,
    ) -> Expression {
        let transformed: Vec<_> = exprs
            .iter()
            .map(|e| self.bind_expression(e, source))
            .collect();
        let typ = transformed.last().and_then(|e| e.typ).unwrap_or(Typ::Unit);
        Expression::new(ExpressionKind::Sequence(transformed), Some(typ))
    }

    /// Bind a `print` expression
    pub fn bind_print(
        &mut self,
        print: &syntax::PrintExpression,
        source: &SourceText,
    ) -> Expression {
        let bound_printee = self.bind_expression(&print.inner, source);
        // TODO: Does the print expression convert things to `String`s?
        let typ = bound_printee.typ;
        Expression::new(ExpressionKind::Print(Box::new(bound_printee)), typ)
    }

    /// Bind Variable Declaration Statement
    ///
    /// Declarations are bound in two steps. First the initialiser is
    /// bound with the current scope, then a new symbol is inserted
    /// into the scope for the declared variable.
    ///
    /// Variable declarations have type inference if the type clause
    /// is missing. If not the type of the initialiser should be
    /// convertable to the declaration's type annotation.
    pub fn bind_declaration(
        &mut self,
        decl: &syntax::DeclarationExpression,
        source: &SourceText,
    ) -> Expression {
        let decl_type = if let Some(anno) = &decl.id.typ {
            self.bind_type(&anno.type_ref)
        } else {
            Typ::Unknown
        };
        let bound_initialiser = self.bind_expression(&decl.initialiser, source);

        let id = decl.id.id;

        // If we don't have a type annotation in the declaration then
        // infer the type from the initialiser
        let ty = if decl_type != Typ::Unknown {
            match bound_initialiser.typ {
                Some(t) if t != decl_type => {
                    // The declaration type doesn't match the
                    // expression being used to initialise it.
                    self.diagnostics.push(Diagnostic::new(
                        format!(
                            "Initialiser doesn't match declaration type for '{}'",
                            source.interned_value(id)
                        ),
                        decl.id.id_tok.span(),
                    ));
                    Some(Typ::Error)
                }
                _ => Some(decl_type),
            }
        } else {
            bound_initialiser.typ
        };

        self.scopes
            .current_mut()
            .try_declare(id, Symbol::Variable(decl.style, ty.unwrap_or(Typ::Unknown)));

        let is_mut = decl.style == VarStyle::Mutable;
        Expression::new(
            ExpressionKind::Declaration(
                VarDecl {
                    ident: source.interned_value(id),
                    ty,
                },
                is_mut,
                Box::new(bound_initialiser),
            ),
            ty,
        )
    }

    /// Bind a block expression
    ///
    /// Creates a new scope and binds the contents of the block in
    /// that scope before popping that scope from the stack.
    pub fn bind_block(&mut self, block: &syntax::BlockBody, source: &SourceText) -> Expression {
        self.scopes.push(Scope::new());
        let bound = self.bind_expression(&block.contents, source);
        self.scopes.pop();
        bound
    }

    /// Bind the type in the current scope
    ///
    /// Looks the type up if there is an annotation. If the annotation
    /// is missing then `None` is retunred.
    pub fn bind_type(&mut self, ty_ref: &TypeRef) -> Typ {
        match *ty_ref {
            TypeRef::Unit(..) => Typ::Unit,
            TypeRef::Simple(ref name) => {
                let id = match name.kind {
                    TokenKind::Word(id) => id,
                    _ => panic!("Expected word token"),
                };
                match self.scopes.lookup(id) {
                    Some(Symbol::Type(ty)) => ty,
                    _ => {
                        self.diagnostics
                            .push(Diagnostic::new("Reference to undefined type", name.span()));
                        Typ::Error
                    }
                }
            }
            // TODO: array and tuple types
            TypeRef::Array(..) => unimplemented!("array types are not yet supported"),
            TypeRef::Tuple(..) => unimplemented!("tuple types are not yet supported"),
            TypeRef::Missing => panic!("Can't lower missing type"),
        }
    }

    /// Clears out the diagnostics list and returns any diagnostics
    /// that have been accumulated.
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.diagnostics.drain(..).collect()
    }
}

/// Add the Default Type Declarations
///
/// Inserts the builtin types `String`, `Bool`, and `Number` types
/// into the given scope. Identifiers for each are looked up from
/// `source`'s interner.
fn add_builtin_types(scope: &mut Scope, source: &SourceText) {
    scope.try_declare(
        source.intern("String"),
        Symbol::Type(Typ::Builtin(BuiltinType::String)),
    );
    scope.try_declare(
        source.intern("Bool"),
        Symbol::Type(Typ::Builtin(BuiltinType::Bool)),
    );
    scope.try_declare(
        source.intern("Number"),
        Symbol::Type(Typ::Builtin(BuiltinType::Number)),
    );
}

#[cfg(test)]
mod test {
    use super::super::BuiltinType;
    use super::*;
    use crate::syntax::text::Interner;
    use crate::syntax::{
        IdentifierExpression, InfixOperatorExpression, Literal, LiteralExpression,
        PrefixExpression, Token, TokenKind,
    };

    #[test]
    fn create_scope() {
        let mut interner = Interner::new();

        let scope = Scope::new();

        assert_eq!(None, scope.lookup(interner.intern("foo")));
    }

    #[test]
    fn declare_and_lookup() {
        let mut interner = Interner::new();
        let mut scope = Scope::new();
        let id = interner.intern("test§");

        assert!(scope.try_declare(id, Symbol::Variable(VarStyle::Mutable, Typ::Unit)));
        let found = scope.lookup(id);
        assert!(!scope.try_declare(id, Symbol::Variable(VarStyle::Mutable, Typ::Unit)));

        assert_eq!(Some(Symbol::Variable(VarStyle::Mutable, Typ::Unit)), found);
    }

    #[test]
    fn declare_in_parent() {
        let mut interner = Interner::new();
        let foo_id = interner.intern("foo");
        let bar_id = interner.intern("bar");
        let baz_id = interner.intern("baz");

        let mut scope = Scope::new();
        assert!(scope.try_declare(
            foo_id,
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::Number))
        ));
        assert!(scope.try_declare(bar_id, Symbol::Variable(VarStyle::Mutable, Typ::Unit)));

        let mut scopes = ScopeStack::new(scope);
        let mut scope = Scope::new();

        assert!(scope.try_declare(
            bar_id,
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::String))
        ));
        assert!(scope.try_declare(
            baz_id,
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::Bool))
        ));

        scopes.push(scope);

        let foo_lookup = scopes.lookup(foo_id);
        let bar_lookup = scopes.lookup(bar_id);
        let baz_lookup = scopes.lookup(baz_id);
        let failed = scopes.lookup(interner.intern("nothere"));

        assert_eq!(
            Some(Symbol::Variable(
                VarStyle::Mutable,
                Typ::Builtin(BuiltinType::Number)
            )),
            foo_lookup
        );
        assert_eq!(
            Some(Symbol::Variable(
                VarStyle::Mutable,
                Typ::Builtin(BuiltinType::String)
            )),
            bar_lookup
        );
        assert_eq!(
            Some(Symbol::Variable(
                VarStyle::Mutable,
                Typ::Builtin(BuiltinType::Bool)
            )),
            baz_lookup
        );
        assert_eq!(None, failed);
    }

    #[test]
    fn scope_stack_current() {
        let source = SourceText::new("");
        let mut scopes = ScopeStack::new(Scope::new());

        assert!(scopes.current_mut().try_declare(
            source.intern("foo"),
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::Bool))
        ));
        assert!(!scopes.current_mut().try_declare(
            source.intern("foo"),
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::Bool))
        ));

        scopes.push(Scope::new());

        assert!(scopes.current_mut().try_declare(
            source.intern("foo"),
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::Number))
        ));
        assert!(!scopes.current_mut().try_declare(
            source.intern("foo"),
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::String))
        ));

        assert_eq!(
            Some(Symbol::Variable(
                VarStyle::Mutable,
                Typ::Builtin(BuiltinType::Number)
            )),
            scopes.lookup(source.intern("foo"))
        );

        scopes.pop();

        assert_eq!(
            Some(Symbol::Variable(
                VarStyle::Mutable,
                Typ::Builtin(BuiltinType::Bool)
            )),
            scopes.lookup(source.intern("foo"))
        );
    }

    #[test]
    fn test_add_default_types() {
        let mut scope = Scope::new();
        let source = SourceText::new("");

        add_builtin_types(&mut scope, &source);

        let string_lookup = scope.lookup(source.intern("String"));
        assert_eq!(
            Some(Symbol::Type(Typ::Builtin(BuiltinType::String))),
            string_lookup
        );

        let bool_lookup = scope.lookup(source.intern("Bool"));
        assert_eq!(
            Some(Symbol::Type(Typ::Builtin(BuiltinType::Bool))),
            bool_lookup
        );

        let num_lookup = scope.lookup(source.intern("Number"));
        assert_eq!(
            Some(Symbol::Type(Typ::Builtin(BuiltinType::Number))),
            num_lookup
        );
    }

    #[test]
    fn bind_identifier_lookup() {
        let source = SourceText::new("");
        let mut scope = Scope::new();
        scope.try_declare(
            source.intern("melles"),
            Symbol::Variable(VarStyle::Mutable, Typ::Builtin(BuiltinType::Bool)),
        );
        let mut binder = Binder::new(scope);

        let bound = binder.bind_identifier(
            &IdentifierExpression {
                ident: source.intern("melles"),
                token: Box::new(Token::new(TokenKind::Word(source.intern("melles")))),
            },
            &source,
        );

        assert_eq!(ExpressionKind::Identifier("melles".into()), bound.kind);
        assert_eq!(Some(Typ::Builtin(BuiltinType::Bool)), bound.typ);
    }

    #[test]
    fn bind_const_value() {
        let mut binder = Binder::new(Scope::new());

        let bound = binder.bind_literal(&LiteralExpression {
            token: Box::new(Token::new(TokenKind::Literal(Literal::Number(1337)))),
            value: Constant::Number(1337),
        });

        assert_eq!(ExpressionKind::Literal(Constant::Number(1337)), bound.kind);
        assert_eq!(Some(Typ::Builtin(BuiltinType::Number)), bound.typ);
    }

    #[test]
    fn bind_prefix_expression() {
        let source = SourceText::new("");
        let mut binder = Binder::new(Scope::new());

        let bound = binder.bind_prefix(
            &PrefixExpression {
                op_token: Box::new(Token::new(TokenKind::Minus)),
                op: PrefixOp::Negate,
                inner: Box::new(syntax::Expression::constant_num(
                    Token::new(TokenKind::Literal(Literal::Number(23))),
                    23,
                )),
            },
            &source,
        );

        assert_eq!(
            ExpressionKind::Prefix(
                PrefixOp::Negate,
                Box::new(Expression::new(
                    ExpressionKind::Literal(Constant::Number(23)),
                    Some(Typ::Builtin(BuiltinType::Number))
                ))
            ),
            bound.kind
        );
        assert_eq!(Some(Typ::Builtin(BuiltinType::Number)), bound.typ);
    }

    // TODO: need a better way of creating the expression trees to run
    //       the binder over for these tests. More complex tests may
    //       also benefit from snapshot testing.

    // Infix(ref innie)
    // Call(ref call)
    // Index(ref index)
    // IfThenElse(ref if_else_expr)
    // Function(ref func)
    // Loop(ref loop_expr)
    // Sequence(ref exprs)
    // Print(ref print)
    // Declaration(ref decl)
    // Grouping(ref group)
}
