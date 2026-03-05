use std::collections::HashMap;
use std::env;
use std::fs;
use syn::visit::Visit;
use syn::{self, Expr, Lit, Pat, Stmt, Block, ItemFn, ImplItemFn, File as SynFile};

// ══════════════════════════════════════════════════════════════
// Data Structures
// ══════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
struct LiteralInfo {
    value: String,
    kind: String,
    line: usize,
    col: usize,
}

#[derive(Debug, Clone)]
struct Finding {
    fn_name: String,
    fn_line: usize,
    return_line: usize,
    literals: Vec<LiteralInfo>,
    is_implicit: bool, // tail expression vs explicit return
}

#[derive(Debug, Clone)]
struct ConstStaticFinding {
    name: String,
    line: usize,
    kind: &'static str, // "const" or "static"
    literals: Vec<LiteralInfo>,
}

// ══════════════════════════════════════════════════════════════
// Symbolic Value — the core abstraction
// ══════════════════════════════════════════════════════════════

/// A symbolic representation of a value in the program.
/// 
/// The key insight: we only care whether literals appear in "value position"
/// (contributing to the return value) vs "control position" (used for branching).
/// 
/// - Literals in if/match conditions → never enter SymValue → not reported
/// - Literals in if/match branch bodies → enter as Literal → reported
/// - Literals in operations (a + 1) → enter as part of Compound → reported
/// - External calls → Opaque → not reported
#[derive(Debug, Clone)]
enum SymValue {
    /// A literal constant — this is what we're tracking
    Literal(LiteralInfo),
    /// An opaque value — function parameter, external call result, etc.
    /// We can't see inside it, so we don't report it.
    Opaque,
    /// A compound value — composed of sub-values.
    /// Used for: binary ops, tuples, arrays, structs, branch results, etc.
    /// All children are in "value position".
    Compound(Vec<SymValue>),
}

impl SymValue {
    /// Extract all literals from value positions (recursively).
    fn extract_literals(&self) -> Vec<LiteralInfo> {
        match self {
            SymValue::Literal(info) => vec![info.clone()],
            SymValue::Opaque => vec![],
            SymValue::Compound(children) => {
                children.iter().flat_map(|c| c.extract_literals()).collect()
            }
        }
    }

    /// Check if this value contains any literals.
    /// Create a compound from multiple values, flattening where possible.
    fn compound(values: Vec<SymValue>) -> SymValue {
        let mut flat = Vec::new();
        for v in values {
            match v {
                SymValue::Opaque => {} // drop opaque from compounds
                SymValue::Compound(children) => flat.extend(children),
                other => flat.push(other),
            }
        }
        if flat.is_empty() {
            SymValue::Opaque
        } else if flat.len() == 1 {
            flat.into_iter().next().unwrap()
        } else {
            SymValue::Compound(flat)
        }
    }


}

/// Variable environment: maps variable names to their symbolic values.
type VarEnv = HashMap<String, SymValue>;

/// Function return map: maps function names to the symbolic value they return.
/// Built in pass 1, used in pass 2 for cross-function tracking.
type FnReturnMap = HashMap<String, SymValue>;

// ══════════════════════════════════════════════════════════════
// Utility Functions
// ══════════════════════════════════════════════════════════════

/// Check if a literal should be exempt (binary state: bool, 0).
fn is_binary_exempt(lit: &Lit) -> bool {
    match lit {
        Lit::Bool(_) => true,
        Lit::Int(l) => l.base10_digits() == "0",
        _ => false,
    }
}

/// Convert a syn Lit to our LiteralInfo.
fn lit_to_info(lit: &Lit) -> Option<LiteralInfo> {
    // Binary state exemption: bool and 0 are not reportable
    if is_binary_exempt(lit) {
        return None;
    }
    match lit {
        Lit::Str(l) => {
            if l.value().is_empty() {
                return None; // Empty string — sentinel/default, exempt
            }
            Some(LiteralInfo {
                value: format!("\"{}\"", l.value()),
                kind: "string".into(),
                line: l.span().start().line,
                col: l.span().start().column,
            })
        },
        Lit::ByteStr(l) => Some(LiteralInfo {
            value: "b\"...\"".into(),
            kind: "byte_string".into(),
            line: l.span().start().line,
            col: l.span().start().column,
        }),
        Lit::CStr(l) => Some(LiteralInfo {
            value: "c\"...\"".into(),
            kind: "c_string".into(),
            line: l.span().start().line,
            col: l.span().start().column,
        }),
        Lit::Byte(l) => Some(LiteralInfo {
            value: format!("b'{}'", l.value() as char),
            kind: "byte".into(),
            line: l.span().start().line,
            col: l.span().start().column,
        }),
        Lit::Char(l) => Some(LiteralInfo {
            value: format!("'{}'", l.value()),
            kind: "char".into(),
            line: l.span().start().line,
            col: l.span().start().column,
        }),
        Lit::Int(l) => Some(LiteralInfo {
            value: l.base10_digits().into(),
            kind: "integer".into(),
            line: l.span().start().line,
            col: l.span().start().column,
        }),
        Lit::Float(l) => Some(LiteralInfo {
            value: l.base10_digits().into(),
            kind: "float".into(),
            line: l.span().start().line,
            col: l.span().start().column,
        }),
        Lit::Bool(l) => Some(LiteralInfo {
            value: l.value.to_string(),
            kind: "bool".into(),
            line: l.span().start().line,
            col: l.span().start().column,
        }),
        _ => None,
    }
}

/// Get the starting line of any expression (best-effort).
fn get_expr_line(expr: &Expr) -> usize {
    match expr {
        Expr::Lit(e) => match &e.lit {
            Lit::Int(l) => l.span().start().line,
            Lit::Float(l) => l.span().start().line,
            Lit::Str(l) => l.span().start().line,
            Lit::Bool(l) => l.span().start().line,
            Lit::Char(l) => l.span().start().line,
            Lit::Byte(l) => l.span().start().line,
            Lit::ByteStr(l) => l.span().start().line,
            _ => 0,
        },
        Expr::Path(e) => e.path.segments.first()
            .map(|s| s.ident.span().start().line).unwrap_or(0),
        Expr::Return(e) => e.return_token.span.start().line,
        Expr::Binary(e) => get_expr_line(&e.left),
        Expr::Unary(e) => get_expr_line(&e.expr),
        Expr::Paren(e) => get_expr_line(&e.expr),
        Expr::Reference(e) => get_expr_line(&e.expr),
        Expr::Block(e) => e.block.stmts.first()
            .map(|s| get_stmt_line(s)).unwrap_or(0),
        Expr::If(e) => get_expr_line(&e.cond),
        Expr::Match(e) => get_expr_line(&e.expr),
        Expr::Call(e) => get_expr_line(&e.func),
        Expr::MethodCall(e) => get_expr_line(&e.receiver),
        Expr::Tuple(e) => e.elems.first()
            .map(|e| get_expr_line(e)).unwrap_or(0),
        Expr::Array(e) => e.elems.first()
            .map(|e| get_expr_line(e)).unwrap_or(0),
        Expr::Struct(e) => e.path.segments.first()
            .map(|s| s.ident.span().start().line).unwrap_or(0),
        Expr::Field(e) => get_expr_line(&e.base),
        Expr::Index(e) => get_expr_line(&e.expr),
        Expr::Cast(e) => get_expr_line(&e.expr),
        Expr::Loop(e) => e.loop_token.span.start().line,
        Expr::Repeat(e) => get_expr_line(&e.expr),
        Expr::Unsafe(e) => e.block.stmts.first()
            .map(|s| get_stmt_line(s)).unwrap_or(0),
        Expr::Try(e) => get_expr_line(&e.expr),
        Expr::Range(e) => e.start.as_ref()
            .map(|e| get_expr_line(e)).unwrap_or(0),
        _ => 0,
    }
}

fn get_stmt_line(stmt: &Stmt) -> usize {
    match stmt {
        Stmt::Local(l) => l.let_token.span.start().line,
        Stmt::Expr(e, _) => get_expr_line(e),
        Stmt::Item(_) => 0,
        Stmt::Macro(m) => m.mac.path.segments.first()
            .map(|s| s.ident.span().start().line).unwrap_or(0),
    }
}

/// Extract variable names from a pattern.
fn extract_pat_names(pat: &Pat) -> Vec<String> {
    match pat {
        Pat::Ident(p) => vec![p.ident.to_string()],
        Pat::Tuple(p) => p.elems.iter().flat_map(extract_pat_names).collect(),
        Pat::TupleStruct(p) => p.elems.iter().flat_map(extract_pat_names).collect(),
        Pat::Struct(p) => p.fields.iter().flat_map(|f| extract_pat_names(&f.pat)).collect(),
        Pat::Reference(p) => extract_pat_names(&p.pat),
        Pat::Type(p) => extract_pat_names(&p.pat),
        Pat::Slice(p) => p.elems.iter().flat_map(extract_pat_names).collect(),
        Pat::Or(p) => p.cases.first().map(|c| extract_pat_names(c)).unwrap_or_default(),
        Pat::Wild(_) => vec![],
        _ => vec![],
    }
}

/// Extract variable names from an expression (for assignment targets).
fn extract_expr_names(expr: &Expr) -> Vec<String> {
    match expr {
        Expr::Path(p) => p.path.get_ident().map(|i| vec![i.to_string()]).unwrap_or_default(),
        Expr::Tuple(t) => t.elems.iter().flat_map(extract_expr_names).collect(),
        _ => vec![],
    }
}

/// Convert a syn::Path to a string.
fn path_to_string(path: &syn::Path) -> String {
    path.segments.iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

// ══════════════════════════════════════════════════════════════
// Symbolic Evaluator — the heart of the analysis
// ══════════════════════════════════════════════════════════════

/// Symbolically evaluate an expression, returning its SymValue.
/// 
/// Key rules:
/// - Literals → SymValue::Literal
/// - Variables → look up in env
/// - if/match conditions → evaluated but result DISCARDED (control position)
/// - if/match branch values → collected into Compound (value position)
/// - Binary/unary ops → Compound of operands (value position)
/// - Function/method calls → Opaque (unless cross-function tracking applies)
/// - Tuples/arrays/structs → Compound of elements
fn sym_eval(expr: &Expr, env: &VarEnv, fn_returns: &FnReturnMap) -> SymValue {
    match expr {
        // ── Literals ──
        Expr::Lit(expr_lit) => {
            match lit_to_info(&expr_lit.lit) {
                Some(info) => SymValue::Literal(info),
                None => SymValue::Opaque,
            }
        }

        // ── Variable / Path ──
        Expr::Path(expr_path) => {
            if let Some(ident) = expr_path.path.get_ident() {
                let name = ident.to_string();
                // true/false are paths in syn — exempt as binary state
                if name == "true" || name == "false" {
                    return SymValue::Opaque;
                }
                // Look up in variable environment
                env.get(&name).cloned().unwrap_or(SymValue::Opaque)
            } else {
                // Multi-segment path (e.g., Self::ROLE_AGENT, Message::ROLE_AGENT)
                // Try to look up in fn_returns (which includes const values)
                let full_path = expr_path.path.segments.iter()
                    .map(|s| s.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::");
                fn_returns.get(&full_path).cloned().unwrap_or(SymValue::Opaque)
            }
        }

        // ── Binary operation ──
        Expr::Binary(e) => {
            use syn::BinOp;
            match &e.op {
                // Comparison and logical operators produce bool — operands are control position
                BinOp::Eq(_) | BinOp::Ne(_) |
                BinOp::Lt(_) | BinOp::Le(_) |
                BinOp::Gt(_) | BinOp::Ge(_) |
                BinOp::And(_) | BinOp::Or(_) => SymValue::Opaque,
                // Arithmetic/bitwise operators — operands are value position
                _ => {
                    let left = sym_eval(&e.left, env, fn_returns);
                    let right = sym_eval(&e.right, env, fn_returns);
                    SymValue::compound(vec![left, right])
                }
            }
        }

        // ── Unary operation: operand is in value position ──
        Expr::Unary(e) => {
            sym_eval(&e.expr, env, fn_returns)
        }

        // ── Parenthesized ──
        Expr::Paren(e) => sym_eval(&e.expr, env, fn_returns),

        // ── Tuple: all elements in value position ──
        Expr::Tuple(e) => {
            let vals: Vec<_> = e.elems.iter().map(|el| sym_eval(el, env, fn_returns)).collect();
            SymValue::compound(vals)
        }

        // ── Array: all elements in value position ──
        Expr::Array(e) => {
            let vals: Vec<_> = e.elems.iter().map(|el| sym_eval(el, env, fn_returns)).collect();
            SymValue::compound(vals)
        }

        // ── Struct literal: field values in value position ──
        Expr::Struct(e) => {
            let vals: Vec<_> = e.fields.iter().map(|f| sym_eval(&f.expr, env, fn_returns)).collect();
            SymValue::compound(vals)
        }

        // ── If expression: condition is CONTROL, branches are VALUE ──
        Expr::If(e) => {
            // Condition is control position — we evaluate it for side effects
            // but DO NOT include its SymValue in the result.
            // (In a pure analysis, conditions have no side effects on our env,
            //  but we still need to process the branches.)
            
            let then_val = sym_eval_block(&e.then_branch, env, fn_returns);
            let else_val = match &e.else_branch {
                Some((_, else_expr)) => sym_eval(else_expr, env, fn_returns),
                None => SymValue::Opaque, // unit type, no value
            };
            SymValue::compound(vec![then_val, else_val])
        }

        // ── Match: scrutinee and patterns are CONTROL, arm bodies are VALUE ──
        Expr::Match(e) => {
            // Scrutinee is control position — discarded
            // Pattern literals are control position — discarded
            // Arm bodies are value position
            let vals: Vec<_> = e.arms.iter().map(|arm| {
                // Bind pattern variables as Opaque in a local env
                let mut arm_env = env.clone();
                for name in extract_pat_names(&arm.pat) {
                    arm_env.insert(name, SymValue::Opaque);
                }
                sym_eval(&arm.body, &arm_env, fn_returns)
            }).collect();
            SymValue::compound(vals)
        }

        // ── Block expression ──
        Expr::Block(e) => sym_eval_block(&e.block, env, fn_returns),

        // ── Unsafe block ──
        Expr::Unsafe(e) => sym_eval_block(&e.block, env, fn_returns),

        // ── Loop with break value ──
        Expr::Loop(e) => {
            let mut break_vals = Vec::new();
            collect_break_values(&e.body, env, fn_returns, &mut break_vals);
            SymValue::compound(break_vals)
        }

        // ── While/For: don't produce values ──
        Expr::While(_) | Expr::ForLoop(_) => SymValue::Opaque,

        // ── Return: analyzed separately by the visitor ──
        Expr::Return(e) => {
            match &e.expr {
                Some(ret_expr) => sym_eval(ret_expr, env, fn_returns),
                None => SymValue::Opaque,
            }
        }

        // ── Reference: pass through ──
        Expr::Reference(e) => sym_eval(&e.expr, env, fn_returns),

        // ── Cast: pass through ──
        Expr::Cast(e) => sym_eval(&e.expr, env, fn_returns),

        // ── Field access: opaque (we don't track struct fields) ──
        Expr::Field(_) => SymValue::Opaque,

        // ── Index: the index is CONTROL position, the collection is opaque ──
        Expr::Index(_) => SymValue::Opaque,

        // ── Range: start and end are value position ──
        Expr::Range(e) => {
            let mut vals = Vec::new();
            if let Some(ref start) = e.start {
                vals.push(sym_eval(start, env, fn_returns));
            }
            if let Some(ref end) = e.end {
                vals.push(sym_eval(end, env, fn_returns));
            }
            SymValue::compound(vals)
        }

        // ── Try (?): pass through ──
        Expr::Try(e) => sym_eval(&e.expr, env, fn_returns),

        // ── Repeat [expr; N]: expr is value, N is control ──
        Expr::Repeat(e) => sym_eval(&e.expr, env, fn_returns),

        // ── Function call ──
        Expr::Call(e) => {
            if let Expr::Path(func_path) = &*e.func {
                let fn_name = path_to_string(&func_path.path);

                // 1. Cross-function tracking (same-file functions)
                if let Some(fn_val) = fn_returns.get(&fn_name) {
                    return fn_val.clone();
                }

                // 2. Authorized transparent wrappers — these don't consume
                //    the contract, they just wrap/convert the value.
                //    Use short_name (last 2 segments) for matching to handle
                //    fully-qualified paths like std::sync::Arc::new.
                let segments: Vec<&str> = fn_name.split("::").collect();
                let short_name = if segments.len() >= 2 {
                    format!("{}::{}", segments[segments.len()-2], segments[segments.len()-1])
                } else {
                    fn_name.clone()
                };

                let check_name = |name: &str| -> bool {
                    fn_name == name || short_name == name
                };

                // Result/Option constructors: value passes through
                if check_name("Ok") || check_name("Some") || check_name("Err") {
                    if let Some(arg) = e.args.first() {
                        return sym_eval(arg, env, fn_returns);
                    }
                }
                // Heap/smart-pointer wrappers: value passes through
                if check_name("Box::new") || check_name("Arc::new") || check_name("Rc::new")
                    || check_name("Cell::new") || check_name("RefCell::new")
                    || check_name("Mutex::new") || check_name("RwLock::new")
                {
                    if let Some(arg) = e.args.first() {
                        return sym_eval(arg, env, fn_returns);
                    }
                }
                // Type conversion constructors: value passes through
                if check_name("String::from") || check_name("Vec::from") || check_name("PathBuf::from")
                    || check_name("OsString::from") || check_name("CString::new")
                {
                    if let Some(arg) = e.args.first() {
                        return sym_eval(arg, env, fn_returns);
                    }
                }
            }
            // External/unknown call → Opaque
            SymValue::Opaque
        }

        // ── Method call ──
        Expr::MethodCall(e) => {
            let method = e.method.to_string();
            match method.as_str() {
                // Type conversions: value semantics preserved
                "to_string" | "to_owned" | "clone" | "into" => {
                    sym_eval(&e.receiver, env, fn_returns)
                }
                // Unwrapping: value passes through
                "unwrap" | "expect" | "unwrap_or_default" => {
                    sym_eval(&e.receiver, env, fn_returns)
                }
                // unwrap_or: either the receiver's inner value or the fallback
                "unwrap_or" => {
                    let recv = sym_eval(&e.receiver, env, fn_returns);
                    if let Some(arg) = e.args.first() {
                        let fallback = sym_eval(arg, env, fn_returns);
                        SymValue::compound(vec![recv, fallback])
                    } else {
                        recv
                    }
                }
                // unwrap_or_else: receiver passes through, closure is opaque
                "unwrap_or_else" => {
                    sym_eval(&e.receiver, env, fn_returns)
                }
                // map/and_then on Option/Result: closure transforms the value,
                // but we can't see inside closures → Opaque
                // (The original value is consumed, not returned)
                "map" | "and_then" | "map_err" | "map_or" | "map_or_else" => {
                    SymValue::Opaque
                }
                // ok(): Result<T,E> → Option<T>, value passes through
                "ok" | "err" => {
                    sym_eval(&e.receiver, env, fn_returns)
                }
                // Anything else: Opaque (external method, unknown semantics)
                _ => SymValue::Opaque,
            }
        }

        // ── Closure: if returned, it's opaque for now ──
        Expr::Closure(_) => SymValue::Opaque,

        // ── Assign: handled at statement level ──
        Expr::Assign(_) => SymValue::Opaque,

        // ── Let expression (if let): opaque ──
        Expr::Let(_) => SymValue::Opaque,

        // ── Anything else: opaque ──
        _ => SymValue::Opaque,
    }
}

/// Symbolically evaluate a block, returning the SymValue of its tail expression.
/// Also processes statements to build up the variable environment.
fn sym_eval_block(block: &Block, parent_env: &VarEnv, fn_returns: &FnReturnMap) -> SymValue {
    let mut env = parent_env.clone();
    let num_stmts = block.stmts.len();

    for (i, stmt) in block.stmts.iter().enumerate() {
        let is_last = i == num_stmts - 1;
        match stmt {
            Stmt::Local(local) => {
                process_local(local, &mut env, fn_returns);
            }
            Stmt::Expr(expr, semi) => {
                // Only the LAST statement without semicolon is the tail expression.
                // syn parses block expressions (if, match, loop, etc.) as Expr(_, None)
                // even when they're not the last statement, so we must check position.
                if is_last && semi.is_none() {
                    // Tail expression — this IS the block's value
                    return sym_eval(expr, &env, fn_returns);
                } else {
                    // Expression statement (with or without semicolon, not tail)
                    process_expr_stmt(expr, &mut env, fn_returns);
                }
            }
            _ => {}
        }
    }

    SymValue::Opaque // no tail expression → unit
}

/// Collect break values from a loop body.
fn collect_break_values(
    block: &Block,
    env: &VarEnv,
    fn_returns: &FnReturnMap,
    results: &mut Vec<SymValue>,
) {
    collect_breaks_in_block(block, env, fn_returns, results);
}

/// Helper: traverse a block maintaining env, looking for break expressions.
fn collect_breaks_in_block(
    block: &Block,
    env: &VarEnv,
    fn_returns: &FnReturnMap,
    results: &mut Vec<SymValue>,
) {
    let mut env = env.clone();
    for stmt in &block.stmts {
        match stmt {
            Stmt::Local(local) => {
                process_local(local, &mut env, fn_returns);
            }
            Stmt::Expr(expr, _) => {
                collect_breaks_in_expr(expr, &env, fn_returns, results);
            }
            _ => {}
        }
    }
}

/// Recursively find break expressions with values.
fn collect_breaks_in_expr(
    expr: &Expr,
    env: &VarEnv,
    fn_returns: &FnReturnMap,
    results: &mut Vec<SymValue>,
) {
    match expr {
        Expr::Break(e) => {
            if let Some(ref val) = e.expr {
                results.push(sym_eval(val, env, fn_returns));
            }
        }
        Expr::Block(e) => {
            collect_breaks_in_block(&e.block, env, fn_returns, results);
        }
        Expr::If(e) => {
            collect_breaks_in_block(&e.then_branch, env, fn_returns, results);
            if let Some((_, else_expr)) = &e.else_branch {
                collect_breaks_in_expr(else_expr, env, fn_returns, results);
            }
        }
        Expr::Match(e) => {
            for arm in &e.arms {
                collect_breaks_in_expr(&arm.body, env, fn_returns, results);
            }
        }
        // Don't recurse into nested loops — their breaks belong to them
        Expr::Loop(_) | Expr::While(_) | Expr::ForLoop(_) => {}
        // Don't recurse into closures
        Expr::Closure(_) => {}
        _ => {}
    }
}

/// Process a let binding and update the variable environment.
fn process_local(local: &syn::Local, env: &mut VarEnv, fn_returns: &FnReturnMap) {
    if let Some(init) = &local.init {
        let val = sym_eval(&init.expr, env, fn_returns);
        let names = extract_pat_names(&local.pat);
        for name in names {
            env.insert(name, val.clone());
        }

        // Handle let-else: `let PAT = expr else { diverge };`
        // The diverge branch contains a return/break/continue/panic
        // We don't need to do anything special — the else branch diverges,
        // so the variables are only bound when the pattern matches.
    } else {
        // `let x;` — uninitialized
        let names = extract_pat_names(&local.pat);
        for name in names {
            env.insert(name, SymValue::Opaque);
        }
    }
}

/// Process an expression statement for side effects (mainly assignments).
fn process_expr_stmt(expr: &Expr, env: &mut VarEnv, fn_returns: &FnReturnMap) {
    match expr {
        Expr::Assign(assign) => {
            let val = sym_eval(&assign.right, env, fn_returns);
            let names = extract_expr_names(&assign.left);
            for name in names {
                env.insert(name, val.clone());
            }
        }
        Expr::Block(e) => {
            for stmt in &e.block.stmts {
                match stmt {
                    Stmt::Local(local) => process_local(local, env, fn_returns),
                    Stmt::Expr(e, _) => process_expr_stmt(e, env, fn_returns),
                    _ => {}
                }
            }
        }
        // For if/match statements (used for side effects), we conservatively
        // don't update the env — variables assigned in branches might or might not
        // be set depending on the condition.
        _ => {}
    }
}

// ══════════════════════════════════════════════════════════════
// Return Finder — finds explicit return statements
// ══════════════════════════════════════════════════════════════

/// Information about an explicit return found in a function body.
struct ExplicitReturn {
    line: usize,
    sym_value: SymValue,
}

/// Find all explicit return statements in a function body and evaluate them.
fn find_explicit_returns(
    block: &Block,
    parent_env: &VarEnv,
    fn_returns: &FnReturnMap,
) -> Vec<ExplicitReturn> {
    let mut env = parent_env.clone();
    let mut results = Vec::new();

    for stmt in &block.stmts {
        match stmt {
            Stmt::Local(local) => {
                process_local(local, &mut env, fn_returns);
                // Check let-else diverge branch for returns
                if let Some(init) = &local.init {
                    if let Some(ref diverge) = init.diverge {
                        find_returns_in_expr(&diverge.1, &env, fn_returns, &mut results);
                    }
                }
            }
            Stmt::Expr(expr, _) => {
                find_returns_in_expr(expr, &env, fn_returns, &mut results);
                process_expr_stmt(expr, &mut env, fn_returns);
            }
            _ => {}
        }
    }

    results
}

/// Helper: find return statements in a block, maintaining env for variable tracking.
fn find_returns_in_block(
    block: &Block,
    env: &VarEnv,
    fn_returns: &FnReturnMap,
    results: &mut Vec<ExplicitReturn>,
) {
    let mut env = env.clone();
    for stmt in &block.stmts {
        match stmt {
            Stmt::Local(local) => {
                process_local(local, &mut env, fn_returns);
                if let Some(init) = &local.init {
                    if let Some(ref diverge) = init.diverge {
                        find_returns_in_expr(&diverge.1, &env, fn_returns, results);
                    }
                }
            }
            Stmt::Expr(expr, _) => {
                find_returns_in_expr(expr, &env, fn_returns, results);
                process_expr_stmt(expr, &mut env, fn_returns);
            }
            _ => {}
        }
    }
}

/// Recursively find return expressions in an expression tree.
fn find_returns_in_expr(
    expr: &Expr,
    env: &VarEnv,
    fn_returns: &FnReturnMap,
    results: &mut Vec<ExplicitReturn>,
) {
    match expr {
        Expr::Return(e) => {
            let val = match &e.expr {
                Some(ret_expr) => sym_eval(ret_expr, env, fn_returns),
                None => SymValue::Opaque,
            };
            results.push(ExplicitReturn {
                line: e.return_token.span.start().line,
                sym_value: val,
            });
        }
        Expr::Block(e) => {
            find_returns_in_block(&e.block, env, fn_returns, results);
        }
        Expr::If(e) => {
            find_returns_in_block(&e.then_branch, env, fn_returns, results);
            if let Some((_, else_expr)) = &e.else_branch {
                find_returns_in_expr(else_expr, env, fn_returns, results);
            }
        }
        Expr::Match(e) => {
            for arm in &e.arms {
                find_returns_in_expr(&arm.body, env, fn_returns, results);
            }
        }
        Expr::Loop(e) => {
            find_returns_in_block(&e.body, env, fn_returns, results);
        }
        Expr::ForLoop(e) => {
            find_returns_in_block(&e.body, env, fn_returns, results);
        }
        Expr::Unsafe(e) => {
            find_returns_in_block(&e.block, env, fn_returns, results);
        }
        // Don't recurse into closures — they have their own return context
        Expr::Closure(_) => {}
        _ => {}
    }
}

// ══════════════════════════════════════════════════════════════
// Pass 1: Collect function return values
// ══════════════════════════════════════════════════════════════

struct FnReturnCollector {
    fn_returns: FnReturnMap,
    current_impl_type: Option<String>,
}

impl<'ast> Visit<'ast> for FnReturnCollector {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let name = node.sig.ident.to_string();
        let val = self.collect_fn_return_value(&node.block);
        self.fn_returns.insert(name, val);
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // Extract the type name from the impl block
        let type_name = if let syn::Type::Path(tp) = &*node.self_ty {
            tp.path.segments.last().map(|s| s.ident.to_string())
        } else {
            None
        };
        let old = self.current_impl_type.take();
        self.current_impl_type = type_name;
        syn::visit::visit_item_impl(self, node);
        self.current_impl_type = old;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let name = node.sig.ident.to_string();
        let val = self.collect_fn_return_value(&node.block);
        // Store with short name (for same-impl calls)
        self.fn_returns.insert(name.clone(), val.clone());
        // Also store with TypeName::fn_name (for cross-type calls)
        if let Some(ref type_name) = self.current_impl_type {
            self.fn_returns.insert(format!("{}::{}", type_name, name), val);
        }
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        // Collect const values for cross-reference tracking
        let val = sym_eval(&node.expr, &VarEnv::new(), &self.fn_returns);
        let name = node.ident.to_string();
        // Store as Self::CONST_NAME (for intra-impl references)
        self.fn_returns.insert(format!("Self::{}", name), val.clone());
        // Store as TypeName::CONST_NAME (for external references)
        if let Some(ref type_name) = self.current_impl_type {
            self.fn_returns.insert(format!("{}::{}", type_name, name), val);
        }
        syn::visit::visit_impl_item_const(self, node);
    }
}

impl FnReturnCollector {
    fn collect_fn_return_value(&self, block: &Block) -> SymValue {
        // First pass: no cross-function tracking
        let empty_returns = FnReturnMap::new();
        let env = VarEnv::new();

        // Get tail expression value
        let tail_val = sym_eval_block(block, &env, &empty_returns);

        // Also collect explicit returns
        let explicit = find_explicit_returns(block, &env, &empty_returns);

        // Merge all possible return values
        let mut all_vals = vec![tail_val];
        for ret in explicit {
            all_vals.push(ret.sym_value);
        }

        SymValue::compound(all_vals)
    }
}

// ══════════════════════════════════════════════════════════════
// Pass 2: Full analysis with cross-function tracking
// ══════════════════════════════════════════════════════════════

struct FnVisitor {
    findings: Vec<Finding>,
    const_findings: Vec<ConstStaticFinding>,
    fn_returns: FnReturnMap,
    current_impl_type: Option<String>,
    #[allow(dead_code)]
    source: String,
}

impl FnVisitor {
    fn analyze_function(&mut self, name: &str, fn_line: usize, block: &Block) {
        let env = VarEnv::new();

        // 1. Analyze tail expression (implicit return)
        let tail_val = sym_eval_block(block, &env, &self.fn_returns);
        let tail_lits = tail_val.extract_literals();
        if !tail_lits.is_empty() {
            // Find the tail expression for line info
            let return_line = block.stmts.last()
                .map(|s| match s {
                    Stmt::Expr(e, None) => get_expr_line(e),
                    _ => 0,
                })
                .unwrap_or(0);

            self.findings.push(Finding {
                fn_name: name.to_string(),
                fn_line,
                return_line,
                literals: tail_lits,
                is_implicit: true,
            });
        }

        // 2. Find explicit returns
        let explicit = find_explicit_returns(block, &env, &self.fn_returns);
        for ret in explicit {
            let lits = ret.sym_value.extract_literals();
            if !lits.is_empty() {
                self.findings.push(Finding {
                    fn_name: name.to_string(),
                    fn_line,
                    return_line: ret.line,
                    literals: lits,
                    is_implicit: false,
                });
            }
        }
    }
}

impl<'ast> Visit<'ast> for FnVisitor {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // Track current impl type for Self:: resolution
        let prev = self.current_impl_type.take();
        if let syn::Type::Path(tp) = &*node.self_ty {
            if let Some(seg) = tp.path.segments.last() {
                self.current_impl_type = Some(seg.ident.to_string());
            }
        }
        syn::visit::visit_item_impl(self, node);
        self.current_impl_type = prev;
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let name = node.sig.ident.to_string();
        let fn_line = node.sig.ident.span().start().line;
        self.analyze_function(&name, fn_line, &node.block);
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        let name = node.sig.ident.to_string();
        let fn_line = node.sig.ident.span().start().line;
        self.analyze_function(&name, fn_line, &node.block);
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        // Only check pub const
        if matches!(node.vis, syn::Visibility::Public(_)) {
            let val = sym_eval(&node.expr, &VarEnv::new(), &self.fn_returns);
            let lits = val.extract_literals();
            if !lits.is_empty() {
                self.const_findings.push(ConstStaticFinding {
                    name: node.ident.to_string(),
                    line: node.ident.span().start().line,
                    kind: "const",
                    literals: lits,
                });
            }
        }
        syn::visit::visit_item_const(self, node);
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        if matches!(node.vis, syn::Visibility::Public(_)) {
            let val = sym_eval(&node.expr, &VarEnv::new(), &self.fn_returns);
            let lits = val.extract_literals();
            if !lits.is_empty() {
                self.const_findings.push(ConstStaticFinding {
                    name: node.ident.to_string(),
                    line: node.ident.span().start().line,
                    kind: "static",
                    literals: lits,
                });
            }
        }
        syn::visit::visit_item_static(self, node);
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        // Detect pub const inside impl blocks (e.g., impl Message { pub const ROLE_AGENT })
        if matches!(node.vis, syn::Visibility::Public(_)) {
            let val = sym_eval(&node.expr, &VarEnv::new(), &self.fn_returns);
            let lits = val.extract_literals();
            if !lits.is_empty() {
                self.const_findings.push(ConstStaticFinding {
                    name: node.ident.to_string(),
                    line: node.ident.span().start().line,
                    kind: "const",
                    literals: lits,
                });
            }
        }
        syn::visit::visit_impl_item_const(self, node);
    }
}

// ══════════════════════════════════════════════════════════════
// Main
// ══════════════════════════════════════════════════════════════

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <file.rs> [file2.rs ...]", args[0]);
        eprintln!("Analyzes Rust source files for literal constant leaks.");
        eprintln!();
        eprintln!("A literal 'escapes' when it appears in VALUE position of a return expression.");
        eprintln!("Literals in CONTROL position (if conditions, match patterns, indices) are OK.");
        std::process::exit(1);
    }

    let mut total_findings = 0;

    for file_path in &args[1..] {
        let source = match fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ERROR] Cannot read {}: {}", file_path, e);
                continue;
            }
        };

        let syntax: SynFile = match syn::parse_file(&source) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[ERROR] Cannot parse {}: {}", file_path, e);
                continue;
            }
        };

        // Pass 1: Collect function return values (no cross-function tracking)
        let mut collector = FnReturnCollector {
            fn_returns: FnReturnMap::new(),
            current_impl_type: None,
        };
        collector.visit_file(&syntax);

        // Pass 2: Full analysis with cross-function tracking
        let mut visitor = FnVisitor {
            findings: Vec::new(),
            const_findings: Vec::new(),
            fn_returns: collector.fn_returns,
            source: source.clone(),
            current_impl_type: None,
        };
        visitor.visit_file(&syntax);

        let file_findings = visitor.findings.len() + visitor.const_findings.len();
        if file_findings > 0 {
            println!("━━━ {} ━━━", file_path);

            // Print const/static findings first
            for cf in &visitor.const_findings {
                println!();
                println!("  ⚠ pub {} `{}` (line {}) — exposes literal(s)",
                    cf.kind, cf.name, cf.line);
                for lit in &cf.literals {
                    println!("    literal: {} ({}) at line {}:{}", lit.value, lit.kind, lit.line, lit.col);
                }
            }

            // Print function findings
            for finding in &visitor.findings {
                let return_type = if finding.is_implicit { "tail expr" } else { "return" };
                println!();
                println!("  ⚠ fn `{}` (line {}) — {} at line {}",
                    finding.fn_name, finding.fn_line, return_type, finding.return_line);
                for lit in &finding.literals {
                    println!("    literal: {} ({}) at line {}:{}", lit.value, lit.kind, lit.line, lit.col);
                }
                // Show the source line
                if finding.return_line > 0 {
                    if let Some(line) = source.lines().nth(finding.return_line - 1) {
                        println!("    source: {}", line.trim());
                    }
                }
            }

            println!();
            total_findings += file_findings;
        }
    }

    if total_findings == 0 {
        println!("✓ No literal constant leaks found.");
    } else {
        let fn_count = total_findings; // simplified
        println!("Found {} potential literal leak(s).", fn_count);
    }
}















