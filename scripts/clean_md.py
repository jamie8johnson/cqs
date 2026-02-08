#!/usr/bin/env python3
"""
Clean PDF-converted markdown files by removing conversion artifacts.

Usage:
    python scripts/clean_md.py samples/md/
    python scripts/clean_md.py samples/md/file.md

Processes all .md files in the given directory or individual files.
Creates .bak backups before modifying.
"""

import argparse
import re
import sys
from pathlib import Path
from typing import List, Tuple


def extract_document_title(lines: List[str]) -> str:
    """Extract the document title from the first H1 heading after boilerplate."""
    for line in lines:
        line = line.strip()
        if line.startswith('# ') and not line.startswith('## '):
            # Remove the '# ' prefix and return the title
            return line[2:].strip()
    return ""


def remove_copyright_boilerplate(lines: List[str]) -> Tuple[List[str], int]:
    """
    Rule 1: Remove copyright boilerplate from the start of the file.
    Detects © 2015-2024 by AVEVA Group Limited in first 80 lines,
    removes everything up to and including the line with softwaresupport.aveva.com.
    """
    copyright_pattern = re.compile(r'© 2015-2024.*AVEVA Group Limited', re.IGNORECASE)
    support_pattern = re.compile(r'softwaresupport\.aveva\.com')

    copyright_found = False
    end_idx = -1

    for i, line in enumerate(lines[:80]):
        if copyright_pattern.search(line):
            copyright_found = True
        if copyright_found and support_pattern.search(line):
            end_idx = i
            break

    if end_idx >= 0:
        removed = end_idx + 1
        return lines[end_idx + 1:], removed

    return lines, 0


def remove_page_boundaries(lines: List[str], doc_title: str) -> Tuple[List[str], int]:
    """
    Rule 2: Remove page boundary blocks.
    Pattern: copyright footer (©) → blank lines → Page N → blank lines → doc title echo → section name → blank lines
    """
    page_pattern = re.compile(r'^Page\s+\d+\s*$')
    copyright_pattern = re.compile(r'^©')

    # Find all Page N line indices
    page_indices = []
    for i, line in enumerate(lines):
        if page_pattern.match(line.strip()):
            page_indices.append(i)

    # For each Page N, look backward for © line
    ranges_to_remove = []

    for page_idx in page_indices:
        # Look backward up to 5 lines for copyright footer
        start_idx = page_idx
        for i in range(page_idx - 1, max(page_idx - 6, -1), -1):
            if copyright_pattern.match(lines[i].strip()):
                start_idx = i
                break

        # Look forward from Page N to find where real content starts
        end_idx = page_idx + 1
        found_content = False

        for i in range(page_idx + 1, min(page_idx + 20, len(lines))):
            line = lines[i].strip()

            # Skip blank lines
            if not line:
                end_idx = i + 1
                continue

            # If it's a heading, this is content
            if line.startswith('#'):
                found_content = True
                break

            # Check if it's the document title echo (fuzzy match)
            if doc_title and (doc_title in line or line in doc_title):
                end_idx = i + 1
                continue

            # If line looks like a section name echo (no special chars, reasonable length)
            # This is heuristic - plain text line that's not too long
            if len(line) < 100 and not any(c in line for c in ['•', '**', '`', '[', ']', '(', ')']):
                end_idx = i + 1
                continue

            # Otherwise, this is real content
            found_content = True
            break

        if not found_content:
            end_idx = min(page_idx + 20, len(lines))

        ranges_to_remove.append((start_idx, end_idx))

    # Merge overlapping ranges and remove
    ranges_to_remove.sort()
    merged = []
    for start, end in ranges_to_remove:
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))

    # Build new lines list excluding removed ranges
    result = []
    removed_count = 0
    idx = 0

    for start, end in merged:
        result.extend(lines[idx:start])
        removed_count += end - start
        idx = end

    result.extend(lines[idx:])

    return result, removed_count


def remove_toc_section(lines: List[str]) -> Tuple[List[str], int]:
    """
    Rule 3: Remove table of contents section.
    Find "# Contents" heading and remove everything until the next H1 heading.
    """
    toc_start = -1
    toc_end = -1

    for i, line in enumerate(lines):
        stripped = line.strip()

        # Find TOC start
        if stripped == '# Contents':
            toc_start = i
            continue

        # Find next H1 after TOC start
        if toc_start >= 0 and stripped.startswith('# ') and not stripped.startswith('##'):
            toc_end = i
            break

    if toc_start >= 0:
        if toc_end < 0:
            toc_end = len(lines)

        removed = toc_end - toc_start
        return lines[:toc_start] + lines[toc_end:], removed

    return lines, 0


def remove_chapter_headings(lines: List[str]) -> Tuple[List[str], int]:
    """
    Rule 4: Remove "### Chapter N" heading artifacts.
    Strips lines matching ^#{1,6}\\s+Chapter\\s+\\d+\\s*$
    """
    chapter_pattern = re.compile(r'^#{1,6}\s+Chapter\s+\d+\s*$')

    result = []
    removed = 0

    for line in lines:
        if chapter_pattern.match(line.strip()):
            removed += 1
        else:
            result.append(line)

    return result, removed


def replace_bold_bullets(lines: List[str]) -> Tuple[List[str], int]:
    """
    Rule 5: Replace **•** with -
    """
    result = []
    replaced = 0

    for line in lines:
        new_line = line.replace('**•**', '-')
        if new_line != line:
            replaced += 1
        result.append(new_line)

    return result, replaced


def remove_strikethrough(lines: List[str]) -> Tuple[List[str], int]:
    """
    Rule 6: Replace ~~text~~ with text
    """
    strikethrough_pattern = re.compile(r'~~([^~]+)~~')

    result = []
    replaced = 0

    for line in lines:
        new_line = strikethrough_pattern.sub(r'\1', line)
        if new_line != line:
            replaced += 1
        result.append(new_line)

    return result, replaced


def collapse_blank_lines(lines: List[str]) -> Tuple[List[str], int]:
    """
    Rule 7: Replace 3+ consecutive blank lines with exactly 2 blank lines.
    """
    result = []
    blank_count = 0
    collapsed = 0

    for line in lines:
        if line.strip() == '':
            blank_count += 1
        else:
            # Output accumulated blanks
            if blank_count > 0:
                output_blanks = min(blank_count, 2) if blank_count >= 3 else blank_count
                if blank_count >= 3:
                    collapsed += blank_count - 2
                result.extend([''] * output_blanks)
                blank_count = 0
            result.append(line)

    # Handle trailing blanks
    if blank_count > 0:
        output_blanks = min(blank_count, 2) if blank_count >= 3 else blank_count
        if blank_count >= 3:
            collapsed += blank_count - 2
        result.extend([''] * output_blanks)

    return result, collapsed


def clean_markdown_file(file_path: Path) -> dict:
    """
    Clean a single markdown file by applying all 7 rules in order.
    Returns statistics about the cleaning process.
    """
    # Read file
    with open(file_path, 'r', encoding='utf-8') as f:
        content = f.read()

    lines = content.splitlines()
    original_line_count = len(lines)

    stats = {
        'original_lines': original_line_count,
        'removed_by_rule': {}
    }

    # Rule 1: Copyright boilerplate
    lines, removed = remove_copyright_boilerplate(lines)
    stats['removed_by_rule']['1_copyright_boilerplate'] = removed

    # Extract document title for use in rule 2
    doc_title = extract_document_title(lines)

    # Rule 2: Page boundaries
    lines, removed = remove_page_boundaries(lines, doc_title)
    stats['removed_by_rule']['2_page_boundaries'] = removed

    # Rule 3: TOC section
    lines, removed = remove_toc_section(lines)
    stats['removed_by_rule']['3_toc_section'] = removed

    # Rule 4: Chapter headings
    lines, removed = remove_chapter_headings(lines)
    stats['removed_by_rule']['4_chapter_headings'] = removed

    # Rule 5: Bold bullets
    lines, replaced = replace_bold_bullets(lines)
    stats['removed_by_rule']['5_bold_bullets_replaced'] = replaced

    # Rule 6: Strikethrough
    lines, replaced = remove_strikethrough(lines)
    stats['removed_by_rule']['6_strikethrough_removed'] = replaced

    # Rule 7: Collapse blank lines
    lines, collapsed = collapse_blank_lines(lines)
    stats['removed_by_rule']['7_blank_lines_collapsed'] = collapsed

    stats['final_lines'] = len(lines)
    stats['total_removed'] = original_line_count - len(lines)

    # Create backup
    backup_path = file_path.with_suffix(file_path.suffix + '.bak')
    with open(backup_path, 'w', encoding='utf-8') as f:
        f.write(content)

    # Write cleaned content
    with open(file_path, 'w', encoding='utf-8') as f:
        f.write('\n'.join(lines))
        if lines:  # Add final newline if file is not empty
            f.write('\n')

    return stats


def process_path(path: Path) -> List[Tuple[Path, dict]]:
    """
    Process a file or directory.
    Returns list of (file_path, stats) tuples.
    """
    results = []

    if path.is_file():
        if path.suffix == '.md':
            stats = clean_markdown_file(path)
            results.append((path, stats))
        else:
            print(f"Skipping non-markdown file: {path}", file=sys.stderr)
    elif path.is_dir():
        md_files = sorted(path.glob('*.md'))
        if not md_files:
            print(f"No .md files found in {path}", file=sys.stderr)
        for md_file in md_files:
            stats = clean_markdown_file(md_file)
            results.append((md_file, stats))
    else:
        print(f"Path does not exist: {path}", file=sys.stderr)
        sys.exit(1)

    return results


def main():
    parser = argparse.ArgumentParser(
        description='Clean PDF-converted markdown files by removing conversion artifacts.'
    )
    parser.add_argument(
        'path',
        type=Path,
        help='Path to a markdown file or directory containing markdown files'
    )

    args = parser.parse_args()

    results = process_path(args.path)

    if not results:
        print("No files processed.", file=sys.stderr)
        sys.exit(1)

    # Print summary
    print(f"\nProcessed {len(results)} file(s):\n")

    for file_path, stats in results:
        print(f"{file_path.name}:")
        print(f"  Original lines: {stats['original_lines']}")
        print(f"  Final lines: {stats['final_lines']}")
        print(f"  Total removed: {stats['total_removed']}")
        print(f"  Details:")
        for rule, count in stats['removed_by_rule'].items():
            if count > 0:
                print(f"    {rule}: {count}")
        print()


if __name__ == '__main__':
    main()
