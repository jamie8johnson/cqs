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
    """
    Converts a PDF file to Markdown format and outputs the result to a file or standard output.
    
    This function reads a PDF file specified as a command-line argument, converts it to Markdown using pymupdf4llm, and writes the output either to a specified file or prints it to stdout. It suppresses library warnings and validates that the input file exists before processing.
    
    Args:
        None. Parameters are read from sys.argv command-line arguments.
    
    Returns:
        None. Outputs converted Markdown either to a file or to stdout, and prints status messages to stderr.
    
    Raises:
        SystemExit: If fewer than 2 command-line arguments are provided (missing input PDF path), or if the specified input PDF file does not exist.
    """
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
