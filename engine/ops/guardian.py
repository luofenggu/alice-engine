#!/usr/bin/env python3
"""
Guardian - 防御器
编译器的延伸，扫描Rust源码中的所有字面量。

规则：代码中不应该有任何裸字面量。所有值的定义权属于配置，不属于代码。
每一个被扫出来的字面量，开发者都要回答"这个值从哪来"。
"""

import os
import re
import sys
from collections import namedtuple

Finding = namedtuple('Finding', ['file', 'line', 'col', 'kind', 'value', 'context'])


def scan_file(filepath):
    """扫描单个Rust文件，返回所有字面量的Finding列表"""
    findings = []
    try:
        with open(filepath, 'r', encoding='utf-8') as f:
            lines = f.readlines()
    except Exception:
        return findings

    in_block_comment = False

    for line_num, raw_line in enumerate(lines, 1):
        line = raw_line.rstrip('\n')

        # 去掉块注释部分，保留非注释部分用于扫描
        processed = ''
        i = 0
        while i < len(line):
            if in_block_comment:
                end = line.find('*/', i)
                if end == -1:
                    i = len(line)
                else:
                    i = end + 2
                    in_block_comment = False
            else:
                start = line.find('/*', i)
                line_comment = line.find('//', i)

                # 行注释先于块注释
                if line_comment != -1 and (start == -1 or line_comment < start):
                    processed += line[i:line_comment]
                    break

                if start != -1:
                    processed += line[i:start]
                    end = line.find('*/', start + 2)
                    if end == -1:
                        in_block_comment = True
                        i = len(line)
                    else:
                        i = end + 2
                else:
                    processed += line[i:]
                    break

        if not processed.strip():
            continue

        # 收集字符串字面量的位置区间，数值扫描时跳过这些区间
        string_regions = []

        # 匹配字符串字面量: "...", r"...", r#"..."#, b"...", br"...", br#"..."#
        str_pat = re.compile(
            r'br#".*?"#'
            r'|br"(?:[^"\\]|\\.)*"'
            r"|r#\".*?\"#"
            r'|r"(?:[^"\\]|\\.)*"'
            r'|b"(?:[^"\\]|\\.)*"'
            r'|"(?:[^"\\]|\\.)*"'
        )
        for m in str_pat.finditer(processed):
            string_regions.append((m.start(), m.end()))
            findings.append(Finding(
                file=filepath, line=line_num, col=m.start() + 1,
                kind='string', value=m.group(), context=line.strip()
            ))

        # 匹配字符字面量 'x', '\n', '\x41', '\u{...}'
        # 排除生命周期标注: 'a 后面跟字母但没有闭合引号
        char_pat = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|[nrt\\0'\"])|[^'\\])'")
        for m in char_pat.finditer(processed):
            if not _in_regions(m.start(), string_regions):
                findings.append(Finding(
                    file=filepath, line=line_num, col=m.start() + 1,
                    kind='char', value=m.group(), context=line.strip()
                ))
                string_regions.append((m.start(), m.end()))

        # 匹配数值字面量
        # 十六进制 0x..., 八进制 0o..., 二进制 0b..., 浮点数, 十进制整数
        num_pat = re.compile(
            r'\b0x[0-9a-fA-F][0-9a-fA-F_]*(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)?\b'
            r'|\b0o[0-7][0-7_]*(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)?\b'
            r'|\b0b[01][01_]*(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize)?\b'
            r'|\b[0-9][0-9_]*\.[0-9][0-9_]*(?:[eE][+-]?[0-9]+)?(?:f32|f64)?\b'
            r'|\b[0-9][0-9_]*[eE][+-]?[0-9]+(?:f32|f64)?\b'
            r'|\b[0-9][0-9_]*(?:u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f32|f64)?\b'
        )
        for m in num_pat.finditer(processed):
            if not _in_regions(m.start(), string_regions):
                findings.append(Finding(
                    file=filepath, line=line_num, col=m.start() + 1,
                    kind='number', value=m.group(), context=line.strip()
                ))

        # 匹配布尔字面量
        bool_pat = re.compile(r'\b(true|false)\b')
        for m in bool_pat.finditer(processed):
            if not _in_regions(m.start(), string_regions):
                findings.append(Finding(
                    file=filepath, line=line_num, col=m.start() + 1,
                    kind='bool', value=m.group(), context=line.strip()
                ))

    return findings


def _in_regions(pos, regions):
    """检查pos是否在任何已知区间内"""
    for start, end in regions:
        if start <= pos < end:
            return True
    return False


def scan_directory(directory):
    """递归扫描目录下所有.rs文件"""
    findings = []
    for root, dirs, files in os.walk(directory):
        for fname in sorted(files):
            if fname.endswith('.rs'):
                fpath = os.path.join(root, fname)
                findings.extend(scan_file(fpath))
    return findings


def print_report(findings, base_dir):
    """打印扫描报告"""
    if not findings:
        print('[GUARD] No literals found. Code is clean.')
        return

    # 按文件分组
    by_file = {}
    for f in findings:
        rel = os.path.relpath(f.file, base_dir)
        by_file.setdefault(rel, []).append(f)

    # 统计
    counts = {'string': 0, 'number': 0, 'char': 0, 'bool': 0}
    for f in findings:
        counts[f.kind] = counts.get(f.kind, 0) + 1

    print('=' * 70)
    print('[GUARD] Literal Scan Report')
    print('=' * 70)
    print('  Strings: %d  |  Numbers: %d  |  Chars: %d  |  Bools: %d  |  Total: %d' % (
        counts['string'], counts['number'], counts['char'], counts['bool'], len(findings)
    ))
    print('=' * 70)

    for rel_file in sorted(by_file.keys()):
        file_findings = by_file[rel_file]
        print('\n--- %s (%d) ---' % (rel_file, len(file_findings)))
        for f in file_findings:
            ctx = f.context
            if len(ctx) > 100:
                ctx = ctx[:97] + '...'
            kind_tag = '[%s]' % f.kind.upper()
            print('  L%-4d %-10s %-30s  %s' % (f.line, kind_tag, f.value, ctx))

    print('\n' + '=' * 70)
    print('[GUARD] %d literals found across %d files' % (len(findings), len(by_file)))
    print('=' * 70)


def main():
    if len(sys.argv) > 1:
        dirs = sys.argv[1:]
    else:
        # 默认扫描当前目录下的src/
        if os.path.isdir('src'):
            dirs = ['src']
        else:
            print('Usage: python3 guardian.py <dir1> [dir2] ...')
            sys.exit(1)

    base_dir = os.getcwd()
    all_findings = []
    for d in dirs:
        all_findings.extend(scan_directory(d))

    print_report(all_findings, base_dir)


if __name__ == '__main__':
    main()