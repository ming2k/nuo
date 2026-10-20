#!/usr/bin/env python3
"""Fail when a citation does not resolve to a canonical in-repo ADR.

Rule: `docs/dev/standards/adr.md` §4 — every `ADR-NNNN` reference in comments or
documentation must point at a file in `docs/adr/`. The pre-ingestion log
(`ADR-0085` .. `ADR-0304`) is not part of this repository, so citing it is a
dangling reference: the reader cannot verify the decision.

One class is exempt by construction: the SQL bodies of shipped migrations. Their
text is pinned by `MIGRATION_CATALOG_FINGERPRINT`
(`crates/nuo-persistence/src/db/tests.rs::migration_catalog_fingerprint_is_stable`),
because editing an applied migration's SQL in place strands databases stamped by
the old text. Those citations are frozen artifacts, not live references.

Only comments (`.rs`) and prose (`.md`) are inspected: a citation inside a string
literal is message text or pinned SQL, never documentation.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys

ADR_DIR = "docs/adr"
TOKEN = re.compile(r"ADR-(\d{4})")

# Files whose *string literals* may still carry legacy numbers, with the reason.
STRING_ALLOWLIST = {
    "crates/nuo-persistence/src/db/migrations.rs": (
        "shipped migration SQL is immutable: its text is pinned by "
        "MIGRATION_CATALOG_FINGERPRINT (see migration_catalog_fingerprint_is_stable)"
    ),
}
FENCE = re.compile(r"^\s*(?:///?!?\s*)?```")
SCAN = re.compile(r'r(#*)"|"|//|/\*|\*/')


def canonical_numbers() -> set[str]:
    have = set()
    for name in os.listdir(ADR_DIR):
        m = re.match(r"(\d{4})-", name)
        if m:
            have.add(m.group(1))
    return have


def dead(text: str, have: set[str]) -> list[str]:
    return [m.group(0) for m in TOKEN.finditer(text) if m.group(1) not in have]


def split_rust_line(line: str, state: tuple) -> tuple[tuple, str, str]:
    """Return (state, code, comment): the same lexical scan the migration used."""
    kind, extra = state
    code_end, comment, i = len(line), "", 0
    while i < len(line):
        if kind == "raw":
            close = '"' + "#" * extra
            pos = line.find(close, i)
            if pos == -1:
                return (kind, extra), line[:code_end], comment
            kind, i = "code", pos + len(close)
            continue
        if kind == "str":
            j, closed = i, False
            while j < len(line):
                if line[j] == "\\":
                    j += 2
                    continue
                if line[j] == '"':
                    kind, i, closed = "code", j + 1, True
                    break
                j += 1
            if not closed:
                return (kind, extra), line[:code_end], comment
            continue
        if kind == "block":
            pos = line.find("*/", i)
            if pos == -1:
                return (kind, extra), "", line
            kind, i = "code", pos + 2
            continue
        m = SCAN.search(line, i)
        if not m:
            break
        token = m.group(0)
        if token.startswith("r"):
            kind, extra, i = "raw", len(m.group(1)), m.end()
        elif token == '"':
            kind, i = "str", m.end()
        elif token == "//":
            code_end, comment = m.start(), line[m.start() :]
            break
        elif token == "/*":
            kind, i = "block", m.end()
        else:
            i = m.end()
    return (kind, extra), line[:code_end], comment


def main() -> int:
    have = canonical_numbers()
    files = subprocess.run(
        ["git", "ls-files", "*.rs", "*.md"], capture_output=True, text=True
    ).stdout.split()
    failures: list[str] = []

    for path in files:
        try:
            lines = open(path, encoding="utf-8").read().split("\n")
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        if path.endswith(".md"):
            in_fence = False
            for number, line in enumerate(lines, 1):
                if FENCE.match(line):
                    in_fence = not in_fence
                    continue
                if in_fence:
                    continue
                if dead(line, have):
                    failures.append(f"{path}:{number}: {line.strip()[:110]}")
            continue

        state: tuple = ("code", 0)
        allow_strings = path in STRING_ALLOWLIST
        for number, line in enumerate(lines, 1):
            state, code, comment = split_rust_line(line, state)
            if not dead(line, have):
                continue
            if dead(code, have) and not allow_strings:
                failures.append(f"{path}:{number}: (string literal) {line.strip()[:110]}")
            if dead(comment, have):
                failures.append(f"{path}:{number}: {comment.strip()[:110]}")

    if not failures:
        print(f"ADR lineage: clean ({len(have)} canonical ADRs, no dangling citations)")
        return 0
    print(f"ADR lineage: {len(failures)} dangling citation(s) — a reference must resolve")
    print(f"to a file in {ADR_DIR}/ (docs/dev/standards/adr.md §4).\n")
    for failure in failures:
        print("  " + failure)
    print(
        "\nAllowed exception: "
        + "; ".join(f"{k} ({v})" for k, v in STRING_ALLOWLIST.items())
        + "."
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
