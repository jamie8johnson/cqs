#!/usr/bin/env python3
"""Convert a PDF file to Markdown using pymupdf4llm.

Usage:
    python scripts/pdf_to_md.py input.pdf              # prints to stdout
    python scripts/pdf_to_md.py input.pdf output.md     # writes to file

Requires: pip install pymupdf4llm
"""
import io
import sys
from pathlib import Path

# Suppress pymupdf4llm import-time warnings (e.g., "Consider using pymupdf_layout")
_real_stdout = sys.stdout
sys.stdout = io.StringIO()
try:
    import pymupdf4llm
except ImportError:
    sys.stdout = _real_stdout
    print(
        "Error: pymupdf4llm not installed. Run: pip install pymupdf4llm",
        file=sys.stderr,
    )
    sys.exit(1)
finally:
    sys.stdout = _real_stdout


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <input.pdf> [output.md]", file=sys.stderr)
        sys.exit(1)

    pdf_path = sys.argv[1]

    if not Path(pdf_path).exists():
        print(f"Error: file not found: {pdf_path}", file=sys.stderr)
        sys.exit(1)

    # Suppress pymupdf4llm warnings that leak to stdout
    sys.stdout = io.StringIO()
    try:
        md_text = pymupdf4llm.to_markdown(pdf_path)
    finally:
        sys.stdout = _real_stdout

    if len(sys.argv) >= 3:
        output_path = sys.argv[2]
        Path(output_path).parent.mkdir(parents=True, exist_ok=True)
        with open(output_path, "w", encoding="utf-8") as f:
            f.write(md_text)
        print(f"Converted {pdf_path} → {output_path}", file=sys.stderr)
    else:
        sys.stdout.write(md_text)


if __name__ == "__main__":
    main()
