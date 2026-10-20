#!/usr/bin/env python3
"""Retire unresolvable legacy ADR citations across the repository.

`docs/adr/` holds the canonical series (0001..0083). Code and prose still cite
numbers from the pre-ingestion log (`ADR-0085` .. `ADR-0304`), which cannot
resolve in this repository and are therefore forbidden by
`docs/dev/standards/adr.md` §4 ("Reference Resolution").

Scope is deliberately narrow, because two classes of text must never change:

* string literals — shipped migration SQL is pinned by a SHA-256 catalog
  fingerprint test (editing it in place strands stamped databases), and
  prompt/assertion strings are behavioural;
* fenced blocks in Markdown and boxed/table diagrams, where the citation sits
  inside alignment-sensitive art.

Only comments in `.rs` files and prose in `.md` files are rewritten. Anything
left behind is reported for a human pass.

    python3 scripts/retire_legacy_adr_refs.py          # dry run + report
    python3 scripts/retire_legacy_adr_refs.py --write  # rewrite in place
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys

ADR_DIR = "docs/adr"
TOKEN = re.compile(r"ADR-(\d{4})")

# One citation, with every shape the corpus uses: `§3`, `§3–§4`, `'s`, and the
# shorthand list `ADR-0163/0186/0187`.
SECTION = (
    r'(?:\s*§\s*(?:"[^"]*"|[A-Za-z]?\d+(?:\.\d+)?)'
    r'(?:\s*/\s*(?:§\s*)?(?:"[^"]*"|[A-Za-z]?\d+))?'
    r'(?:\s*[–—-]\s*(?:§\s*)?[A-Za-z]?\d+)?)?'
)
CITE = rf"ADR-(\d{{4}}){SECTION}(?:\s*/\s*ADR-\d{{4}}{SECTION})*('s)?"
CITE_RE = re.compile(CITE)
CONNECTIVE_RE = re.compile(r"\b(?:per|since|see|by|from|under)\s+" + CITE, re.I)
LEADING_LABEL_RE = re.compile(r"^(\s*(?://[/!]?|\*+|--)\s*)" + CITE + r"\s*[:,-]?\s+")
PAREN_RE = re.compile(r"\(([^()]*)\)")
PURE_JUNK = re.compile(r"^[\s.,;:—–§/()\[\]'\"-]*$")
FENCE = re.compile(r"^\s*(?:///?!?\s*)?```")
FILLER_LINE = re.compile(
    r"^\s*([Ll]ocally|[Hh]ere|[Oo]nly|[Bb]oth|[Aa]lso|[Pp]reviously|[Nn]ow)\s*[.,;:]?\s*$"
)


def canonical_numbers() -> set[str]:
    have = set()
    for name in os.listdir(ADR_DIR):
        m = re.match(r"(\d{4})-", name)
        if m:
            have.add(m.group(1))
    return have


HAVE = canonical_numbers()


def dangling(text: str) -> bool:
    return any(m.group(1) not in HAVE for m in TOKEN.finditer(text))


def strip_citations(fragment: str) -> str:
    def repl(m: re.Match[str]) -> str:
        numbers = re.findall(r"(\d{4})", m.group(0))
        if all(n in HAVE for n in numbers):
            return m.group(0)
        if m.group(1) in HAVE:
            # `ADR-0163/0186` — keep the resolvable head, drop the tail.
            return m.group(1).join(["ADR-", ""])
        return ""

    return CITE_RE.sub(repl, fragment)


def clean_parentheticals(text: str) -> str:
    def repl(match: re.Match[str]) -> str:
        content = match.group(1)
        if not dangling(content):
            return match.group(0)
        items = [strip_citations(part).strip(" ,;:.") for part in content.split(",")]
        kept = [item for item in items if item and not PURE_JUNK.match(item)]
        if not kept:
            return ""
        if len(kept) == 1 and kept[0].startswith('"') and kept[0].endswith('"'):
            return kept[0]
        return "(" + ", ".join(kept) + ")"

    return PAREN_RE.sub(repl, text)


def tidy(text: str) -> str:
    text = re.sub(r"^[\s.,;:—–/]+", "", text)
    # A multi-line parenthetical can leave its closer holding an orphan `;`.
    text = re.sub(r"([.,;:—–/])\s*\)", ")", text)
    text = re.sub(r"\(\s*[.,;:—–/]\s*", "(", text)
    text = re.sub(r"\(\s*[.,;:—–/()\[\]\s-]*\)", "", text)
    text = re.sub(r"\(\s+", "(", text)
    text = re.sub(r"\s+\)", ")", text)
    text = re.sub(r",\s*,", ",", text)
    text = re.sub(r",\s*\)", ")", text)
    text = re.sub(r"\.\s*,", ".", text)
    text = re.sub(r"\s+([.,;:!?])", r"\1", text)
    text = re.sub(r"[ \t]{2,}", " ", text)
    return text.rstrip()


def transform_comment(comment: str) -> str | None:
    """Rewrite one comment fragment, or None if it carried only the pointer."""
    m = re.match(r"^(\s*(?://[/!]?|\*+|--)\s*)(.*)$", comment, re.S)
    marker, body = (m.group(1), m.group(2)) if m else ("", comment)

    body = clean_parentheticals(body)

    while dangling(body):
        m = LEADING_LABEL_RE.match(marker + body)
        if not m:
            break
        rest = (marker + body)[m.end() :]
        rest = rest[:1].upper() + rest[1:] if rest else rest
        marker, body = m.group(1), rest

    body = CONNECTIVE_RE.sub(
        lambda c: "" if any(n not in HAVE for n in re.findall(r"(\d{4})", c.group(0))) else c.group(0),
        body,
    )
    body = strip_citations(body)
    body = tidy(body)
    if not body or PURE_JUNK.match(body) or FILLER_LINE.match(body):
        return None
    return marker + body


# --- Rust lexical scan: split each line into (code, comment) -------------------

SCAN = re.compile(r'r(#*)"|"|//|/\*|\*/')


def split_rust_line(line: str, state: tuple) -> tuple[tuple, str, str]:
    kind, extra = state
    code_end = len(line)
    comment = ""
    i = 0
    while i < len(line):
        if kind == "raw":
            close = '"' + "#" * extra
            pos = line.find(close, i)
            if pos == -1:
                return (kind, extra), line[:code_end], comment
            kind, i = "code", pos + len(close)
            continue
        if kind == "str":
            j = i
            closed = False
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
        else:  # `*/` outside a block comment: treat as code
            i = m.end()
    return (kind, extra), line[:code_end], comment


def rewrite_rust(text: str, path: str, spill: list[str]) -> str:
    out: list[str] = []
    state: tuple = ("code", 0)
    for number, line in enumerate(text.split("\n"), 1):
        state, code, comment = split_rust_line(line, state)
        if not dangling(line):
            out.append(line)
            continue
        if dangling(code):
            spill.append(f"{path}:{number}: {line.strip()[:120]}")
            out.append(line)
            continue
        if comment and dangling(comment):
            new_comment = transform_comment(comment)
            out.append(code + new_comment if new_comment is not None else code.rstrip() or code)
            continue
        out.append(line)
    return "\n".join(out)


def rewrite_markdown(text: str, path: str, spill: list[str]) -> str:
    out: list[str] = []
    in_fence = False
    for number, line in enumerate(text.split("\n"), 1):
        if FENCE.match(line):
            in_fence = not in_fence
            out.append(line)
            continue
        if in_fence or not dangling(line):
            out.append(line)
            continue
        if "│" in line or re.search(r"\|\s*:?-{3,}", line):
            spill.append(f"{path}:{number}: (table/diagram) {line.strip()[:110]}")
            out.append(line)
            continue
        new = clean_parentheticals(line)
        while dangling(new):
            m = LEADING_LABEL_RE.match(new)
            if not m:
                break
            rest = new[m.end() :]
            new = m.group(1) + (rest[:1].upper() + rest[1:] if rest else rest)
        new = CONNECTIVE_RE.sub(
            lambda c: "" if any(n not in HAVE for n in re.findall(r"(\d{4})", c.group(0))) else c.group(0),
            new,
        )
        new = tidy(strip_citations(new))
        out.append(new)
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", action="store_true")
    args = ap.parse_args()

    files = subprocess.run(
        ["git", "ls-files", "*.rs", "*.md"], capture_output=True, text=True
    ).stdout.split()
    spill: list[str] = []
    changed = 0
    for path in files:
        try:
            text = open(path, encoding="utf-8").read()
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        if not dangling(text):
            continue
        new = (
            rewrite_markdown(text, path, spill)
            if path.endswith(".md")
            else rewrite_rust(text, path, spill)
        )
        if args.write and new != text:
            open(path, "w", encoding="utf-8").write(new)
            changed += 1

    if args.write:
        print(f"rewrote {changed} files")
    print(f"left for a human pass: {len(spill)}")
    for entry in spill:
        print("  " + entry)
    return 0


if __name__ == "__main__":
    sys.exit(main())
