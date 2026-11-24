#!/usr/bin/env python3
"""Report Unicode characters for manual review; never rewrite or infer authorship."""

import argparse
import json
import sys
import unicodedata
from pathlib import Path


def inspect(text):
    """Return zero-based offsets and one-based lines/columns, not intent judgments."""
    findings = []
    line, column = 1, 1
    for offset, char in enumerate(text):
        category = unicodedata.category(char)
        # Include joiners, bidi controls, tags, selectors and unusual spaces for
        # review even when load-bearing. A finding is not permission to remove it.
        if (category == "Cf" or (category == "Zs" and char != " ")
                or 0xFE00 <= ord(char) <= 0xFE0F
                or 0xE0100 <= ord(char) <= 0xE01EF
                or (category == "Cc" and char not in "\t\r\n")):
            findings.append({
                "offset": offset,
                "line": line,
                "column": column,
                "codepoint": f"U+{ord(char):04X}",
                "name": unicodedata.name(char, "UNNAMED"),
                "category": category,
            })
        if char == "\n":
            line, column = line + 1, 1
        else:
            column += 1
    return findings


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path, help="UTF-8 text file to inspect read-only")
    args = parser.parse_args(argv)
    try:
        # Decode strictly and keep original CRLF/BOM for accurate offsets.
        data = args.path.read_bytes()
        if data.startswith((b"PK\x03\x04", b"%PDF-", b"\x89PNG", b"\xff\xd8\xff", b"GIF8")) or b"\0" in data:
            raise ValueError("binary/container input is not supported")
        text = data.decode("utf-8")
    except (OSError, UnicodeError, ValueError) as error:
        print(f"Cannot inspect {args.path}: {error}", file=sys.stderr)
        return 2
    print(json.dumps({
        "path": str(args.path),
        "notice": "Review only: these characters can be meaningful; presence or absence does not establish AI authorship or watermark status.",
        "offset_unit": "Unicode code points, zero-based; line/column are one-based, not display columns",
        "findings": inspect(text),
    }, ensure_ascii=True, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
