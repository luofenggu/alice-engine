#!/usr/bin/env python3
"""Guardian: Rust literal position detector.

Three-tier exemption:
  1. Full exemption: policy/, http_protocol.rs — skipped entirely
  2. Escape-guarded: persist/, external/, inference/, util/ — only leak_detector checks
  3. Normal dirs: all literals caught, with minimal line-level exemptions (test/binary/log/error)
"""

import argparse
import os
import subprocess
import sys

from tree_sitter import Language, Parser

GRAMMAR_SEARCH = [
    os.path.join(os.path.dirname(os.path.abspath(__file__)), 'rust.so'),
]

def find_grammar():
    for p in GRAMMAR_SEARCH:
        if os.path.exists(p):
            return p
    raise FileNotFoundError("rust.so not found in: " + str(GRAMMAR_SEARCH))

RUST_LANG = Language(find_grammar(), 'rust')

LOG_MACROS = {'info', 'warn', 'error', 'debug', 'trace'}
ERROR_MACROS = {'bail', 'ensure', 'anyhow'}

LITERAL_TYPES = {
    'string_literal': 'STRING',
    'raw_string_literal': 'STRING',
    'integer_literal': 'NUMBER',
    'float_literal': 'NUMBER',
    'boolean_literal': 'BOOL',
    'char_literal': 'CHAR',
}

# Full exemption: skip entirely, no scanning, no reporting
FULL_EXEMPT_DIRS = {'policy', 'api', 'bindings', 'legacy'}
FULL_EXEMPT_FILES = {'http_protocol.rs'}

# Test files: filename contains '_test' or 'test_' → full exempt
TEST_FILE_PATTERNS = ['_test.', 'test_', '_test.rs']

# Escape-guarded: only leak_detector checks for pub interface leaks
ESCAPE_GUARDED_DIRS = {'persist', 'external', 'inference', 'util'}

LEAK_DETECTOR_PATH = os.path.join(os.path.dirname(__file__), 'leak-detector', 'target', 'release', 'literal_leak_detector')


def find_rust_files(directory, excludes):
    files = []
    for root, dirs, fnames in os.walk(directory):
        dirs[:] = [d for d in dirs if d not in excludes and d != 'target']
        for f in fnames:
            if f.endswith('.rs'):
                files.append(os.path.join(root, f))
    return sorted(files)


def node_text(node, source_bytes=None):
    return node.text.decode('utf-8') if hasattr(node, 'text') else ''


def get_line_text(source_lines, line_idx):
    if 0 <= line_idx < len(source_lines):
        return source_lines[line_idx]
    return ''


def get_macro_name(node):
    """Walk up to find enclosing macro_invocation and return its name."""
    p = node.parent
    while p:
        if p.type == 'macro_invocation':
            mac = p.child_by_field_name('macro')
            if not mac:
                for c in p.children:
                    if c.type in ('identifier', 'scoped_identifier'):
                        mac = c
                        break
            if mac:
                return node_text(mac)
            return None
        if p.type in ('function_item', 'impl_item', 'mod_item'):
            break
        p = p.parent
    return None


def is_in_test_module(node):
    """Check if node is inside a #[cfg(test)] module."""
    p = node.parent
    while p:
        if p.type == 'mod_item':
            # Check for #[cfg(test)] attribute
            prev = p.prev_named_sibling
            while prev and prev.type == 'attribute_item':
                txt = node_text(prev)
                if 'cfg' in txt and 'test' in txt:
                    return True
                prev = prev.prev_named_sibling
            # Also check inline attributes
            for c in p.children:
                if c.type == 'attribute_item':
                    txt = node_text(c)
                    if 'cfg' in txt and 'test' in txt:
                        return True
        p = p.parent
    return False


def is_in_error_chain(node, source_bytes):
    """Check if literal is inside .context() or .with_context() call.
    Must traverse through closures since .with_context(|| format!(...)) is common.
    """
    p = node.parent
    while p:
        if p.type == 'call_expression':
            func = p.child_by_field_name('function')
            if func and func.type == 'field_expression':
                field = func.child_by_field_name('field')
                if field:
                    fname = node_text(field)
                    if fname in ('context', 'with_context'):
                        return True
        if p.type in ('function_item', 'impl_item'):
            break
        p = p.parent
    return False


def classify_file(filepath):
    """Classify a file into exemption tier.
    Returns: 'full_exempt', 'escape_guarded', or 'normal'
    """
    basename = os.path.basename(filepath)
    if basename in FULL_EXEMPT_FILES:
        return 'full_exempt'
    # Test files: filename contains "test" (e.g. prompt_dump_test.rs, test_flatten.rs)
    name_no_ext = os.path.splitext(basename)[0]
    if 'test' in name_no_ext:
        return 'full_exempt'
    parts = filepath.replace(os.sep, '/').split('/')
    if any(p in FULL_EXEMPT_DIRS for p in parts):
        return 'full_exempt'
    if any(p in ESCAPE_GUARDED_DIRS for p in parts):
        return 'escape_guarded'
    return 'normal'


def scan_file(filepath, source_bytes, parser):
    """Scan a normal-tier file for literal violations.
    Only called for 'normal' files — full_exempt and escape_guarded are handled elsewhere.
    """
    tree = parser.parse(source_bytes)
    source_lines = source_bytes.decode('utf-8', errors='replace').split('\n')
    findings = []

    # Detect serde_json usage (json package implies literal expectations)
    def check_json_usage(node):
        """Find any serde_json usage — the package itself is a violation in normal dirs."""
        if node.type == 'macro_invocation':
            mac = node.child_by_field_name('macro')
            if not mac:
                for c in node.children:
                    if c.type in ('identifier', 'scoped_identifier'):
                        mac = c
                        break
            if mac:
                mac_text = node_text(mac)
                if 'serde_json' in mac_text or mac_text == 'json':
                    line = node.start_point[0] + 1
                    if not is_in_test_module(node):
                        context = get_line_text(source_lines, node.start_point[0])
                        findings.append({
                            'line': line,
                            'kind': 'JSON',
                            'value': mac_text + '!(...)',
                            'context': context.strip(),
                            'exempt': False,
                            'exempt_reason': '',
                        })
        if node.type in ('call_expression', 'field_expression'):
            text = node_text(node)
            if 'serde_json::' in text and node.type == 'call_expression':
                func = node.child_by_field_name('function')
                if func:
                    func_text = node_text(func)
                    if 'serde_json::' in func_text:
                        line = node.start_point[0] + 1
                        if not is_in_test_module(node):
                            context = get_line_text(source_lines, node.start_point[0])
                            findings.append({
                                'line': line,
                                'kind': 'JSON',
                                'value': func_text,
                                'context': context.strip(),
                                'exempt': False,
                                'exempt_reason': '',
                            })
        for c in node.children:
            check_json_usage(c)
    check_json_usage(tree.root_node)

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

            # 1. Boolean and zero literals (binary state — exempt by design)
            if kind == 'BOOL':
                exempt = True
                exempt_reason = 'binary'
            if not exempt and kind == 'NUMBER' and value == '0':
                exempt = True
                exempt_reason = 'binary'

            # 2. Empty string literals (binary state — sentinel/default)
            if not exempt and kind == 'STRING' and value == '""':
                exempt = True
                exempt_reason = 'binary'

            # 3. Test module
            if not exempt and is_in_test_module(node):
                exempt = True
                exempt_reason = 'test'

            # 4. Log macros (info!, warn!, error!, debug!, trace!)
            if not exempt:
                macro = get_macro_name(node)
                if macro:
                    macro_base = macro.rstrip('!')
                    # Handle scoped macros: tracing::info -> info
                    if '::' in macro_base:
                        macro_base = macro_base.rsplit('::', 1)[1]
                    if macro_base in LOG_MACROS:
                        exempt = True
                        exempt_reason = 'log'

            # 6. Error macros (bail!, ensure!, anyhow!)
            if not exempt:
                macro = get_macro_name(node)
                if macro:
                    macro_base = macro.rstrip('!')
                    if '::' in macro_base:
                        macro_base = macro_base.rsplit('::', 1)[1]
                    if macro_base in ERROR_MACROS:
                        exempt = True
                        exempt_reason = 'error'

            # 7. Error method chain (.context(), .with_context())
            if not exempt:
                if is_in_error_chain(node, source_bytes):
                    exempt = True
                    exempt_reason = 'error'

            # (json_error/json_ok exemption removed — json! implies literal expectation)

            findings.append({
                'line': line,
                'kind': kind,
                'value': value,
                'context': context.strip(),
                'exempt': exempt,
                'exempt_reason': exempt_reason,
            })

        for c in node.children:
            visit(c)

    visit(tree.root_node)
    findings.sort(key=lambda f: f['line'])
    return findings


def run_leak_detector(files):
    """Run leak_detector on escape-guarded files, return list of escape violations."""
    if not files:
        return []
    if not os.path.isfile(LEAK_DETECTOR_PATH):
        print('[WARN] leak_detector not found at {}'.format(LEAK_DETECTOR_PATH), file=sys.stderr)
        return []

    try:
        result = subprocess.run(
            [LEAK_DETECTOR_PATH] + files,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        universal_newlines=True, timeout=30
        )
    except subprocess.TimeoutExpired:
        print('[WARN] leak_detector timed out', file=sys.stderr)
        return []

    if result.returncode == 0:
        return []  # No leaks found

    # Parse leak_detector output
    escapes = []
    current_file = None
    for line in result.stdout.splitlines():
        line = line.strip()
        if line.startswith('━━━') and line.endswith('━━━'):
            # File header: ━━━ /path/to/file.rs ━━━
            current_file = line.strip('━━━ ').strip()
        elif line.startswith('⚠') and current_file:
            # ⚠ pub const `NAME` (line N) — exposes literal(s)
            # ⚠ fn `name` (line N) — tail expr at line N
            escapes.append({
                'file': current_file,
                'detail': line,
            })

    return escapes


def main():
    ap = argparse.ArgumentParser(description='Guardian: Rust literal position detector')
    ap.add_argument('directory', help='Directory to scan')
    ap.add_argument('--brief', action='store_true', help='Show only first 3 violations per file')
    ap.add_argument('--exclude', default='', help='Comma-separated directories to exclude')
    args = ap.parse_args()

    excludes = [e.strip() for e in args.exclude.split(',') if e.strip()]

    parser = Parser()
    parser.set_language(RUST_LANG)

    all_files = find_rust_files(args.directory, excludes)
    if not all_files:
        print('No .rs files found in', args.directory)
        sys.exit(0)

    # Classify files into tiers
    normal_files = []
    escape_guarded_files = []
    full_exempt_count = 0

    for filepath in all_files:
        tier = classify_file(filepath)
        if tier == 'full_exempt':
            full_exempt_count += 1
        elif tier == 'escape_guarded':
            escape_guarded_files.append(filepath)
        else:
            normal_files.append(filepath)

    # Phase 1: Scan normal files for literal violations
    total_violations = 0
    total_exempted = 0
    exempt_counts = {}
    file_results = []

    for filepath in normal_files:
        with open(filepath, 'rb') as f:
            source_bytes = f.read()
        findings = scan_file(filepath, source_bytes, parser)

        violations = [f for f in findings if not f['exempt']]
        exempted = [f for f in findings if f['exempt']]

        if violations:
            file_results.append((filepath, violations))
        total_violations += len(violations)
        total_exempted += len(exempted)

        for f in exempted:
            r = f['exempt_reason']
            exempt_counts[r] = exempt_counts.get(r, 0) + 1

    # Phase 2: Run leak_detector on escape-guarded files
    escape_violations = run_leak_detector(escape_guarded_files)

    # Output: normal file violations
    for filepath, violations in file_results:
        rel = os.path.relpath(filepath)
        print('\n--- {} ({}) ---'.format(rel, len(violations)))
        show = violations[:3] if args.brief else violations
        for f in show:
            print('  L{:<5} [{:<6}] {:<30} {}'.format(
                f['line'], f['kind'], f['value'], f['context'][:60]))
        if args.brief and len(violations) > 3:
            print('  ... and {} more'.format(len(violations) - 3))

    # Output: escape violations from leak_detector
    if escape_violations:
        print('\n--- ESCAPE VIOLATIONS (leak_detector) ---')
        for esc in escape_violations:
            print('  {} : {}'.format(esc['file'], esc['detail']))

    total_escapes = len(escape_violations)
    total_violations += total_escapes

    # Summary
    print()
    print('=' * 70)
    parts = ['[GUARD] {} violations'.format(total_violations)]
    if total_escapes:
        parts.append('({} escapes)'.format(total_escapes))
    parts.append('across {} files'.format(len(file_results) + (1 if escape_violations else 0)))
    parts.append('({} exempted)'.format(total_exempted))
    print(' '.join(parts))
    if exempt_counts:
        counts_str = ', '.join('{}={}'.format(k, v) for k, v in
                               sorted(exempt_counts.items(), key=lambda x: -x[1]))
        print('  Exemptions: {}'.format(counts_str))
    if escape_guarded_files:
        print('  Escape-guarded: {} files checked by leak_detector'.format(len(escape_guarded_files)))
    if full_exempt_count:
        print('  Full-exempt: {} files skipped'.format(full_exempt_count))
    print('=' * 70)

    sys.exit(1 if total_violations > 0 else 0)

if __name__ == '__main__':
    main()

