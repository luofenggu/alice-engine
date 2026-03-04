#!/usr/bin/env python3
"""Guardian - Rust literal escape detector using tree-sitter AST analysis.

Scans Rust source files for string/number/char/bool literals and reports violations.
For exempt directories (persist/external/policy), exempts internal literals
but detects escape through pub fn return paths.

Usage:
    python3 guardian.py <dir> [--brief] [--exclude dir1,dir2]
"""

import sys, os, argparse, re
from tree_sitter import Language, Parser

# --- Grammar setup ---
GRAMMAR_SEARCH = [
    os.path.join(os.path.dirname(os.path.abspath(__file__)), 'rust.so'),
    '/data/tree-sitter-rust.so',
]

def find_grammar():
    for p in GRAMMAR_SEARCH:
        if os.path.exists(p):
            return p
    raise FileNotFoundError("rust.so not found in: " + str(GRAMMAR_SEARCH))

RUST_LANG = Language(find_grammar(), 'rust')

# --- Constants ---
LOG_MACROS = {'info', 'warn', 'error', 'debug', 'trace'}
ERROR_MACROS = {'bail', 'ensure', 'anyhow'}
ERROR_METHODS = {'context', 'with_context'}
LITERAL_TYPES = {
    'string_literal': 'STRING', 'raw_string_literal': 'STRING',
    'integer_literal': 'NUMBER', 'float_literal': 'NUMBER',
    'char_literal': 'CHAR', 'boolean_literal': 'BOOL',
}
SERDE_JSON_FNS = {'from_str', 'from_value', 'to_string', 'to_string_pretty',
                  'to_value', 'from_reader', 'to_writer', 'to_writer_pretty'}
SERDE_JSON_MACROS = {'json'}

# --- Helpers ---
def find_rust_files(directory, excludes):
    result = []
    for root, dirs, files in os.walk(directory):
        dirs[:] = [d for d in dirs if not any(
            os.path.abspath(os.path.join(root, d)).startswith(os.path.abspath(e))
            for e in excludes)]
        for f in files:
            if f.endswith('.rs'):
                result.append(os.path.join(root, f))
    return sorted(result)

def node_text(node, source_bytes=None):
    return node.text.decode('utf-8', errors='replace') if node.text else ''

def get_line_text(source_lines, line_idx):
    if 0 <= line_idx < len(source_lines):
        return source_lines[line_idx].rstrip()
    return ''

# --- AST analysis ---
def get_macro_name(node):
    """Get the macro name from a macro_invocation ancestor."""
    p = node.parent
    while p:
        if p.type == 'macro_invocation':
            mac = p.child_by_field_name('macro')
            if mac:
                # could be path like log::info — take last segment
                if mac.type == 'scoped_identifier':
                    name_node = mac.child_by_field_name('name')
                    return node_text(name_node, node.text) if name_node else None
                return node_text(mac, p.text)
            # fallback: first child
            for c in p.children:
                if c.type == 'identifier':
                    return node_text(c, p.text)
                if c.type == 'scoped_identifier':
                    name_node = c.child_by_field_name('name')
                    return node_text(name_node, p.text) if name_node else None
            return None
        p = p.parent
    return None

def is_in_test_module(node):
    """Check if node is inside a #[cfg(test)] module."""
    p = node.parent
    while p:
        if p.type == 'mod_item':
            # attribute_item is a sibling before mod_item in tree-sitter AST
            sib = p.prev_sibling
            while sib and sib.type == 'attribute_item':
                txt = node_text(sib)
                if 'cfg' in txt and 'test' in txt:
                    return True
                sib = sib.prev_sibling
        p = p.parent
    return False

def is_in_error_chain(node, source_bytes):
    """Check if literal is in .context() / .with_context() call."""
    p = node.parent
    while p:
        if p.type == 'call_expression':
            func = p.child_by_field_name('function')
            if func and func.type == 'field_expression':
                field = func.child_by_field_name('field')
                if field and node_text(field, source_bytes) in ERROR_METHODS:
                    return True
        p = p.parent
    return False

def get_fn_info(node):
    """For a node inside a function, return (fn_node, fn_name, is_pub).
    fn_node is the function_item node."""
    p = node.parent
    while p:
        if p.type == 'function_item':
            name_node = p.child_by_field_name('name')
            fn_name = node_text(name_node, p.text) if name_node else '?'
            is_pub = False
            for c in p.children:
                if c.type == 'visibility_modifier':
                    is_pub = True
                    break
                if c.type == 'function_modifiers' or c.type in ('fn', 'identifier'):
                    break
            return (p, fn_name, is_pub)
        p = p.parent
    return None

def get_impl_struct_name(node, source_bytes):
    """If node is inside an impl block, return the struct name."""
    p = node.parent
    while p:
        if p.type == 'impl_item':
            type_node = p.child_by_field_name('type')
            if type_node:
                return node_text(type_node, source_bytes)
            # fallback
            for c in p.children:
                if c.type == 'type_identifier':
                    return node_text(c, source_bytes)
            return None
        p = p.parent
    return None

def _is_in_call_arguments(node, stop_node):
    """Check if node is inside a function/method call's arguments list.
    Walks up from node to stop_node looking for an 'arguments' parent."""
    p = node.parent
    while p and p != stop_node:
        if p.type == 'arguments':
            return True
        p = p.parent
    return False

def is_in_return_path(literal_node, fn_node):
    """Check if a literal is in the return path of a function.
    Return path = explicit return statement OR implicit return (last expr in block).
    Exception: literals used as arguments to function/method calls are NOT escaping,
    even if the call itself is in the return path — the call transforms the literal,
    so the literal value itself does not appear in the return value."""

    # Literals inside call arguments are not escaping
    if _is_in_call_arguments(literal_node, fn_node):
        return False

    # Check explicit return
    p = literal_node.parent
    while p and p != fn_node:
        if p.type == 'return_expression':
            return True
        p = p.parent

    # Check implicit return: literal is part of the last expression in fn body
    body = fn_node.child_by_field_name('body')
    if not body or body.type != 'block':
        return False

    # Find last non-trivial child of block (skip closing brace)
    last_expr = None
    for c in body.children:
        if c.type not in ('{', '}', 'line_comment', 'block_comment'):
            last_expr = c

    if last_expr is None:
        return False

    # If last child is an expression (not a statement like let_declaration, expression_statement with ;)
    # In tree-sitter, implicit return is just an expression node, not wrapped in expression_statement
    if last_expr.type == 'expression_statement':
        # Has semicolon → not implicit return
        return False

    # Check if literal is a descendant of last_expr
    def is_descendant(node, ancestor):
        p = node.parent
        while p:
            if p == ancestor:
                return True
            p = p.parent
        return False

    if not is_descendant(literal_node, last_expr):
        return False

    return True

def collect_impl_metadata(impl_node, source_bytes):
    """Collect metadata about functions and consts in an impl block.
    Returns: {
        'struct_name': str,
        'fns': {name: {'node': fn_node, 'is_pub': bool, 'returns_literal': bool}},
        'consts': {name: {'has_literal': bool}},
    }
    """
    struct_name = None
    type_node = impl_node.child_by_field_name('type')
    if type_node:
        struct_name = node_text(type_node, source_bytes)
    else:
        for c in impl_node.children:
            if c.type == 'type_identifier':
                struct_name = node_text(c, source_bytes)
                break

    fns = {}
    consts = {}

    body = impl_node.child_by_field_name('body')
    if not body:
        # try declaration_list
        for c in impl_node.children:
            if c.type == 'declaration_list':
                body = c
                break

    if body:
        for item in body.children:
            if item.type == 'function_item':
                name_node = item.child_by_field_name('name')
                if name_node:
                    fn_name = node_text(name_node, source_bytes)
                    is_pub = any(c.type == 'visibility_modifier' for c in item.children)
                    fns[fn_name] = {'node': item, 'is_pub': is_pub, 'returns_literal': False}
            elif item.type == 'const_item':
                name_node = item.child_by_field_name('name')
                if name_node:
                    const_name = node_text(name_node, source_bytes)
                    # check if value contains literal
                    value = item.child_by_field_name('value')
                    has_lit = False
                    if value:
                        has_lit = _has_literal_descendant(value)
                    consts[const_name] = {'has_literal': has_lit}

    return {'struct_name': struct_name, 'fns': fns, 'consts': consts}

def _has_literal_descendant(node):
    """Check if node or any descendant is a literal."""
    if node.type in LITERAL_TYPES:
        return True
    for c in node.children:
        if _has_literal_descendant(c):
            return True
    return False

def check_fn_returns_literal(fn_info, source_bytes):
    """Check if a function's return path contains literals."""
    fn_node = fn_info['node']
    body = fn_node.child_by_field_name('body')
    if not body or body.type != 'block':
        return False

    # Check explicit returns
    def check_returns(node):
        if node.type == 'return_expression':
            return _has_literal_descendant(node)
        for c in node.children:
            if check_returns(c):
                return True
        return False

    if check_returns(body):
        return True

    # Check implicit return (last expr)
    last_expr = None
    for c in body.children:
        if c.type not in ('{', '}', 'line_comment', 'block_comment'):
            last_expr = c
    if last_expr and last_expr.type != 'expression_statement':
        if _has_literal_descendant(last_expr):
            return True

    return False

def get_return_path_calls(fn_node):
    """Get all method/function calls in the return path of a function."""
    calls = []
    body = fn_node.child_by_field_name('body')
    if not body or body.type != 'block':
        return calls

    def collect_calls_in_node(node):
        if node.type == 'call_expression':
            func = node.child_by_field_name('function')
            if func and func.type == 'field_expression':
                field = func.child_by_field_name('field')
                if field:
                    calls.append(node_text(field, func.text))
        for c in node.children:
            collect_calls_in_node(c)

    # Explicit returns
    def check_returns(node):
        if node.type == 'return_expression':
            collect_calls_in_node(node)
        for c in node.children:
            check_returns(c)
    check_returns(body)

    # Implicit return
    last_expr = None
    for c in body.children:
        if c.type not in ('{', '}', 'line_comment', 'block_comment'):
            last_expr = c
    if last_expr and last_expr.type != 'expression_statement':
        collect_calls_in_node(last_expr)

    return calls

def get_return_path_identifiers(fn_node, source_bytes):
    """Get all identifiers in the return path of a function."""
    idents = []
    body = fn_node.child_by_field_name('body')
    if not body or body.type != 'block':
        return idents

    def collect_idents(node):
        if node.type == 'identifier':
            idents.append(node_text(node, source_bytes))
        for c in node.children:
            collect_idents(c)

    # Explicit returns
    def check_returns(node):
        if node.type == 'return_expression':
            collect_idents(node)
        for c in node.children:
            check_returns(c)
    check_returns(body)

    # Implicit return
    last_expr = None
    for c in body.children:
        if c.type not in ('{', '}', 'line_comment', 'block_comment'):
            last_expr = c
    if last_expr and last_expr.type != 'expression_statement':
        collect_idents(last_expr)

    return idents

# --- serde_json detection ---
def detect_serde_json_usage(root_node, source_bytes, is_exempt_dir, source_lines):
    """Detect serde_json function calls and json! macro invocations.
    Only exempt inside persist struct impl blocks and test modules."""
    findings = []

    def visit(node):
        detected = False
        detail = ''

        # Check call_expression for serde_json::xxx
        if node.type == 'call_expression':
            func = node.child_by_field_name('function')
            if func:
                si = None
                if func.type == 'scoped_identifier':
                    si = func
                elif func.type == 'generic_function':
                    # serde_json::from_str::<T>(...)
                    for c in func.children:
                        if c.type == 'scoped_identifier':
                            si = c
                            break
                if si:
                    path = si.child_by_field_name('path')
                    name = si.child_by_field_name('name')
                    if path and name:
                        if node_text(path, source_bytes) == 'serde_json' and \
                           node_text(name, source_bytes) in SERDE_JSON_FNS:
                            detected = True
                            detail = 'serde_json::' + node_text(name, source_bytes)

        # Check macro_invocation for json!
        elif node.type == 'macro_invocation':
            mac = node.child_by_field_name('macro')
            if not mac:
                for c in node.children:
                    if c.type == 'identifier':
                        mac = c
                        break
                    if c.type == 'scoped_identifier':
                        mac = c
                        break
            if mac:
                if mac.type == 'identifier' and node_text(mac, source_bytes) in SERDE_JSON_MACROS:
                    detected = True
                    detail = 'json! macro'
                elif mac.type == 'scoped_identifier':
                    name_node = mac.child_by_field_name('name')
                    if name_node and node_text(name_node, source_bytes) in SERDE_JSON_MACROS:
                        detected = True
                        detail = 'serde_json::json! macro'

        if detected:
            line = node.start_point[0] + 1
            context = get_line_text(source_lines, node.start_point[0])

            exempt = False
            exempt_reason = ''

            # Test module exemption
            if is_in_test_module(node):
                exempt = True
                exempt_reason = 'test'

            # Persist struct exemption
            if not exempt:
                impl_struct = get_impl_struct_name(node, source_bytes)
                if is_exempt_dir:
                    exempt = True
                    exempt_reason = 'persist'

            findings.append({
                'line': line,
                'kind': 'SERDE_JSON',
                'value': detail,
                'context': context.strip(),
                'exempt': exempt,
                'exempt_reason': exempt_reason,
                'escape': False,
            })

        for c in node.children:
            visit(c)

    visit(root_node)
    return findings

# --- Main scan ---
# Files exempted at file level (message catalogs, etc.)
EXEMPT_FILES = set()
# Directories exempted at directory level (persist layer, etc.)
EXEMPT_DIRS = {'persist', 'external', 'inference', 'util'}

def scan_file(filepath, source_bytes, parser):
    # File-level exemption: message catalog files
    if os.path.basename(filepath) in EXEMPT_FILES:
        return []
    # Check if file is in an exempt directory (dir-level escape-guarded exemption)
    parts = filepath.replace(os.sep, '/').split('/')
    # Full exemption for api directory (HTTP protocol literals)
    if any(p == "api" for p in parts):
        return []
    # Full exemption for policy directory (strategy parameters, message catalogs)
    if any(p == "policy" for p in parts):
        return []
    is_exempt_dir = any(p in EXEMPT_DIRS for p in parts)
    tree = parser.parse(source_bytes)
    source_lines = source_bytes.decode('utf-8', errors='replace').split('\n')
    findings = []

    # Phase 1: Collect impl metadata (for exempt dirs, all impls; otherwise skip)
    persist_impls = {}  # struct_name -> metadata
    def find_impls(node):
        if node.type == 'impl_item':
            meta = collect_impl_metadata(node, source_bytes)
            sname = meta['struct_name']
            if sname and is_exempt_dir:
                # Mark which private fns return literals
                for fn_name, fn_info in meta['fns'].items():
                    if not fn_info['is_pub']:
                        fn_info['returns_literal'] = check_fn_returns_literal(fn_info, source_bytes)
                persist_impls[sname] = meta
        for c in node.children:
            find_impls(c)
    if is_exempt_dir:
        find_impls(tree.root_node)

    # Phase 2: For persist impls, detect indirect escape in pub fns
    # A pub fn escapes if its return path calls a tainted private fn or references a tainted const
    escape_fns = set()  # (struct_name, fn_name) pairs that have indirect escape
    for sname, meta in persist_impls.items():
        for fn_name, fn_info in meta['fns'].items():
            if not fn_info['is_pub']:
                continue
            # Check calls in return path
            ret_calls = get_return_path_calls(fn_info['node'])
            for call_name in ret_calls:
                if call_name in meta['fns'] and not meta['fns'][call_name]['is_pub']:
                    if meta['fns'][call_name]['returns_literal']:
                        escape_fns.add((sname, fn_name))
            # Check const references in return path
            ret_idents = get_return_path_identifiers(fn_info['node'], source_bytes)
            for ident in ret_idents:
                if ident in meta['consts'] and meta['consts'][ident]['has_literal']:
                    escape_fns.add((sname, fn_name))

    # Phase 3: Walk all literals
    def visit(node):
        if node.type in LITERAL_TYPES:
            kind = LITERAL_TYPES[node.type]
            value = node_text(node, source_bytes)
            if len(value) > 60:
                value = value[:57] + '...'
            line = node.start_point[0] + 1
            context = get_line_text(source_lines, node.start_point[0])

            exempt = False
            exempt_reason = ''
            escape = False

            # 0. Boolean and zero literals (binary state - exempt by design)
            if kind == 'BOOL':
                exempt = True
                exempt_reason = 'binary'
            if not exempt and kind == 'NUMBER' and value == '0':
                exempt = True
                exempt_reason = 'binary'

            # 1. Test module
            if not exempt and is_in_test_module(node):
                exempt = True
                exempt_reason = 'test'

            # 2. Log macros
            if not exempt:
                macro = get_macro_name(node)
                if macro and macro.rstrip('!') in LOG_MACROS:
                    exempt = True
                    exempt_reason = 'log'

            # 2b. Thread naming (strings starting with "thread-")
            if not exempt and kind == 'STRING':
                stripped = value.strip('"')
                if stripped.startswith('thread-'):
                    exempt = True
                    exempt_reason = 'internal'

            # 3. Error macros
            if not exempt:
                macro = get_macro_name(node)
                if macro and macro.rstrip('!') in ERROR_MACROS:
                    exempt = True
                    exempt_reason = 'error'

            # 4. Error method chain
            if not exempt:
                if is_in_error_chain(node, source_bytes):
                    exempt = True
                    exempt_reason = 'error'

            # 5. Exempt directory analysis (dir-level exemption with escape guard)
            if not exempt and is_exempt_dir:
                fn_result = get_fn_info(node)
                if fn_result:
                    fn_node, fn_name, is_pub = fn_result
                    if not is_pub:
                        # Private fn in exempt dir — exempt
                        exempt = True
                        exempt_reason = 'private'
                    else:
                        # Pub fn — check if in return path
                        if is_in_return_path(node, fn_node):
                            # Direct escape!
                            escape = True
                        else:
                            # Not in return path — internal use, exempt
                            exempt = True
                            exempt_reason = 'internal'

                        # Also check indirect escape
                        impl_struct = get_impl_struct_name(node, source_bytes)
                        if not escape and impl_struct and (impl_struct, fn_name) in escape_fns:
                            pass
                else:
                    # Not in a fn (e.g., const, module-level) — exempt
                    exempt = True
                    exempt_reason = 'internal'

            findings.append({
                'line': line,
                'kind': kind,
                'value': value,
                'context': context.strip(),
                'exempt': exempt,
                'exempt_reason': exempt_reason,
                'escape': escape,
            })

        for c in node.children:
            visit(c)

    visit(tree.root_node)

    # Phase 4: Add indirect escape findings
    for (sname, fn_name) in escape_fns:
        if sname in persist_impls:
            fn_info = persist_impls[sname]['fns'].get(fn_name)
            if fn_info:
                fn_node = fn_info['node']
                line = fn_node.start_point[0] + 1
                context = get_line_text(source_lines, fn_node.start_point[0])
                findings.append({
                    'line': line,
                    'kind': 'ESCAPE',
                    'value': 'indirect escape via return',
                    'context': context.strip(),
                    'exempt': False,
                    'exempt_reason': '',
                    'escape': True,
                })

    # Phase 5: Detect serde_json usage outside persist structs
    serde_findings = detect_serde_json_usage(tree.root_node, source_bytes, is_exempt_dir, source_lines)
    findings.extend(serde_findings)

    findings.sort(key=lambda f: f['line'])
    return findings

def main():
    ap = argparse.ArgumentParser(description='Guardian: Rust literal escape detector')
    ap.add_argument('directory', help='Directory to scan')
    ap.add_argument('--brief', action='store_true', help='Show only first 3 violations per file')
    ap.add_argument('--exclude', default='', help='Comma-separated directories to exclude')
    args = ap.parse_args()

    excludes = [e.strip() for e in args.exclude.split(',') if e.strip()]

    parser = Parser()
    parser.set_language(RUST_LANG)

    files = find_rust_files(args.directory, excludes)
    if not files:
        print('No .rs files found in', args.directory)
        sys.exit(0)

    total_violations = 0
    total_exempted = 0
    total_escapes = 0
    exempt_counts = {}
    file_results = []

    for filepath in files:
        with open(filepath, 'rb') as f:
            source_bytes = f.read()
        findings = scan_file(filepath, source_bytes, parser)

        violations = [f for f in findings if not f['exempt']]
        exempted = [f for f in findings if f['exempt']]
        escapes = [f for f in violations if f['escape']]

        if violations:
            file_results.append((filepath, violations))
        total_violations += len(violations)
        total_exempted += len(exempted)
        total_escapes += len(escapes)

        for f in exempted:
            r = f['exempt_reason']
            exempt_counts[r] = exempt_counts.get(r, 0) + 1

    # Output
    for filepath, violations in file_results:
        rel = os.path.relpath(filepath)
        print('\n--- {} ({}) ---'.format(rel, len(violations)))
        show = violations[:3] if args.brief else violations
        for f in show:
            esc_mark = ' [ESCAPE!]' if f['escape'] else ''
            print('  L{:<5} [{:<6}] {:<30} {}{}'.format(
                f['line'], f['kind'], f['value'], f['context'][:60], esc_mark))
        if args.brief and len(violations) > 3:
            print('  ... and {} more'.format(len(violations) - 3))

    # Summary
    print()
    print('=' * 70)
    parts = ['[GUARD] {} violations'.format(total_violations)]
    if total_escapes:
        parts.append('{} ESCAPES'.format(total_escapes))
    parts.append('across {} files'.format(len(file_results)))
    parts.append('({} exempted)'.format(total_exempted))
    print(' '.join(parts))
    if exempt_counts:
        counts_str = ', '.join('{}={}'.format(k, v) for k, v in
                               sorted(exempt_counts.items(), key=lambda x: -x[1]))
        print('  Exemptions: {}'.format(counts_str))
    print('=' * 70)

    sys.exit(1 if total_violations > 0 else 0)

if __name__ == '__main__':
    main()
