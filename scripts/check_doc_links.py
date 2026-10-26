#!/usr/bin/env python3
"""Fail when a documentation link does not resolve, or breaks a layout firewall.

Rule source: `docs/dev/standards/style.md` §1 and `docs/dev/standards/taxonomy.md` §2.
Both were *declared* long before they were *mechanized* — §1.3 says "Every relative
link must resolve to a valid file. Dead links or broken anchors block publication",
but nothing blocked anything, so citations rotted silently as crates were renamed
and relocated. `docs/dev/standards/invariants.md` §1 is the authority for closing
that gap: a rule with neither a compiler proof nor an automated assertion is an
aspiration, not a contract.

Four checks, all mechanical:

1. **Target existence** (`style.md` §1.3) — every relative link/image resolves to a
   file on disk, relative to the *linking* file's directory.
2. **No absolute locators** (`style.md` §1.2) — a target must not be a rooted path
   (`/…`) or a `file://` URL; those break across mirrors, forks, and checkouts.
3. **Anchor existence** (`style.md` §1.3, §1.4) — a `#fragment` matches a heading in
   the target file under GitHub's slug rules (kebab-case, punctuation stripped,
   duplicates suffixed `-1`, `-2`, …).
4. **Audience firewall** (`taxonomy.md` §2) — public surfaces
   (`docs/{tutorials,how-to,reference,explanation}/`) must not link into the
   internal maintainer space (`docs/dev/**`).

Fenced code blocks and inline code spans are not prose and are not scanned: they
legitimately contain link *syntax* as examples (e.g. `[text](url)` illustrating what
a user may now type). Treating example syntax as a citation is the one false
positive this checker must avoid by construction.
"""

from __future__ import annotations

import os
import re
import sys

FENCE = re.compile(r"^\s*(```|~~~)")
INLINE_CODE = re.compile(r"`[^`]*`")
# `[text](target)` and `![alt](target)`; tolerates an optional <…> around the target.
LINK = re.compile(r"!?\[[^\]]*\]\(\s*<?([^)\s>]+)>?\s*\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
EXTERNAL = ("http://", "https://", "mailto:", "tel:", "data:")

PUBLIC_SURFACES = (
    "docs/tutorials/",
    "docs/how-to/",
    "docs/reference/",
    "docs/explanation/",
)
INTERNAL_SPACE = "docs/dev/"
SKIP_DIRS = {".git", "target", "node_modules"}


def github_slug(text: str) -> str:
    """GitHub's heading anchor: lowercase, code/markup stripped, punctuation dropped."""
    slug = text.strip().lower()
    slug = re.sub(r"`([^`]*)`", r"\1", slug)  # inline code keeps its text
    slug = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", slug)  # link keeps its label
    slug = re.sub(r"[^\w\s-]", "", slug, flags=re.UNICODE)
    return slug.strip().replace(" ", "-")


def heading_anchors(path: str) -> set[str]:
    """Every anchor a target file offers, including duplicate-heading suffixes."""
    anchors: set[str] = set()
    seen: dict[str, int] = {}
    in_fence = False
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            if FENCE.match(line):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            match = HEADING.match(line)
            if not match:
                continue
            base = github_slug(match.group(2))
            count = seen.get(base, 0)
            seen[base] = count + 1
            anchors.add(base if count == 0 else f"{base}-{count}")
    return anchors


def markdown_files() -> list[str]:
    """Docs tree plus the root-level Markdown (`README.md`, `CHANGELOG.md`, …)."""
    found: list[str] = []
    for dirpath, dirnames, filenames in os.walk("."):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for name in filenames:
            if not name.endswith(".md"):
                continue
            path = os.path.normpath(os.path.join(dirpath, name))
            if path.startswith("docs" + os.sep) or os.sep not in path:
                found.append(path)
    return sorted(found)


def prose_lines(path: str):
    """Yield `(line_number, text)` for prose only: code fences skipped, inline code blanked."""
    in_fence = False
    with open(path, encoding="utf-8", errors="replace") as handle:
        for number, line in enumerate(handle, 1):
            if FENCE.match(line):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            yield number, INLINE_CODE.sub("", line)


def main() -> int:
    problems: list[str] = []
    checked = 0
    cached_anchors: dict[str, set[str]] = {}

    for path in markdown_files():
        posix = path.replace(os.sep, "/").lstrip("./")
        for number, line in prose_lines(path):
            for match in LINK.finditer(line):
                target = match.group(1)
                if target.startswith(EXTERNAL) or target.startswith("#"):
                    continue
                checked += 1
                location = f"{posix}:{number}"

                if target.startswith("/") or target.startswith("file://"):
                    problems.append(
                        f"{location}: absolute locator `{target}` "
                        f"(style.md §1.2 — use a relative path)"
                    )
                    continue

                file_part, _, fragment = target.partition("#")
                if not file_part:
                    continue
                resolved = os.path.normpath(
                    os.path.join(os.path.dirname(path), file_part)
                )
                if not os.path.exists(resolved):
                    problems.append(
                        f"{location}: `{target}` does not resolve "
                        f"(style.md §1.3 — dead link)"
                    )
                    continue

                if fragment:
                    anchors = cached_anchors.get(resolved)
                    if anchors is None:
                        anchors = heading_anchors(resolved)
                        cached_anchors[resolved] = anchors
                    if fragment.lower() not in anchors:
                        problems.append(
                            f"{location}: `{target}` has no matching heading "
                            f"(style.md §1.3/§1.4 — broken anchor)"
                        )

                if posix.startswith(PUBLIC_SURFACES):
                    internal = os.path.normpath(
                        os.path.join(os.path.dirname(path), file_part)
                    ).replace(os.sep, "/")
                    if internal.startswith(INTERNAL_SPACE):
                        problems.append(
                            f"{location}: `{target}` crosses the audience firewall "
                            f"(taxonomy.md §2 — public docs must not link into {INTERNAL_SPACE})"
                        )

    for problem in problems:
        print(problem)
    print(
        f"doc links: {checked} relative targets checked, "
        f"{len(problems)} problem(s) in {len(markdown_files())} files"
    )
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
