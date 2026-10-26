#!/usr/bin/env python3
"""Workspace topology and presentation firewall — enforce the concentric dependency
direction and the tiered crate layout (ADR-0005, ADR-0058, ADR-0103, ADR-0105,
ADR-0106, ADR-0107, ADR-0109).

The headless server and the pure presentation layers are only as clean as the dependency
graph says they are. This checker makes the graph's shape a mechanical gate
rather than a convention:

  R1  Server purity / client-server anti-inversion.
      No crate outside the presentation layer may depend on the terminal /
      presentation substrate (`nuotc`, `crossterm`, `nuo-ui-primitives`, `nuo-view`,
      `nuo-present`, `nuo-ui`, `nuo-shell`, `nuo-tui`). In particular the headless
      `nuo-server` must never link a terminal presentation library (AGENTS.md "Server Purity").

  R2  Single public binary fidelity / retired crate isolation.
      Only the presentation layer (`nuo-view`, `nuo`) may depend on `nuo-ui-primitives`,
      and only the `nuo` host binary may depend on `nuo-view`. No library crate outside
      presentation may depend on presentation crates. Depending on any retired crate
      (`nuo-present`, `nuo-ui`, `nuo-shell`, `nuo-tui`, `nuo-egress`, `nuo-oauth`)
      fails outright.

  R3  Zero-IO presentation substrates (`[INV-ARCH-UI-ZERO-IO]`, `[INV-ARCH-VIEW-ZERO-IO]`).
      Neither `crates/nuo-ui-primitives` nor `crates/nuo-view` may declare network,
      subprocess-supervision, OS-socket, clipboard, or terminal-raw dependencies.
      Every presentation projection renders `f(Props, State, Theme) -> Frame` in pure memory.

  R4  UI Primitives Purity (`[INV-ARCH-UI-PRIMITIVES-PURITY]`).
      `crates/nuo-ui-primitives` must never depend on domain views (`nuo-view`),
      agent runtimes (`nuo-harness`), or server components.

  R5  Pure tiered workspace topology (ADR-0055, ADR-0109 `[INV-ARCH-TIER-01]`).
      The physical layout is itself an invariant, so it is checked, not described:
        - `[INV-ARCH-TIER-01]` exactly three flat tiers exist: `crates/*`, `tools/*`, `providers/*`;
        - all core crates (Layers 1-4, including the single public application binary
          `crates/nuo`) reside in `crates/`;
        - `providers/` and `tools/` contain only their respective leaf extensions;
        - zero crate manifests may reside directly at the workspace root;
        - the root manifest's `members` is strictly the wildcard set
          `["crates/*", "tools/*", "providers/*"]`;
        - exactly one public binary entry point (`crates/nuo`) exists workspace-wide.

      Without this gate the layout is a convention: a crate dropped outside a tier would be
      invisible to every rule above, because they all enumerate the workspace by glob.
"""

from __future__ import annotations

import glob
import os
import re
import sys

# The sanctioned crate tiers (ADR-0109). Membership is exclusive; order is presentational.
#   crates/     Layers 1-4: contracts, runtime, presentation, and the host binary (`crates/nuo`).
#   providers/  Layer-3 provider channel adapters (leaves; no vendor-to-vendor edges).
#   tools/      Layer-3 tool plugins (leaves).
TIERS = ("crates", "providers", "tools")

# The sole application host binary manifest.
HOST_PATH = "crates/nuo/Cargo.toml"

# The sanctioned wildcard member set, checked literally.
SANCTIONED_MEMBERS = ["crates/*", "tools/*", "providers/*"]

# Per-tier namespace rule: every crate directory must match its tier pattern.
# `crates/` hosts `nuo` (the application) and `nuo-*` (the component libraries).
TIER_PREFIX = {"providers": "nuo-provider-", "tools": "nuo-tool-"}

# Names (workspace deps) that constitute the terminal / presentation substrate.
TERMINAL_SUBSTRATE = {"nuotc", "crossterm"}
PRESENTATION_CRATES = {"nuo-ui-primitives", "nuo-view"}
RETIRED_CRATES = {"nuo-egress", "nuo-oauth", "nuo-present", "nuo-ui", "nuo-shell", "nuo-tui"}
PRESENTATION_LAYER = PRESENTATION_CRATES | {"nuo"}

# Dependencies the pure UI substrate must never declare (`[INV-ARCH-UI-ZERO-IO]`).
UI_FORBIDDEN = {
    "tokio-tungstenite",  # async websocket transport
    "netune",             # outbound egress
    "crossterm",          # terminal raw-mode driver
    "arboard",            # clipboard
    "libc",               # raw OS calls (terminal/process)
    "dirs",               # filesystem layout
    "nuo-host",           # daemon process supervision
    "nuo-server",         # headless runtime
    "nuo-harness",        # execution engine
    "nuo-provider",       # provider registry
    "nuo-mcp",            # MCP client
}

TARGET = re.compile(r'name\s*=\s*"([^"]+)"')
DEP_HEAD = re.compile(r"^\[(dependencies|dev-dependencies|build-dependencies)\]$")
KEY = re.compile(r"([A-Za-z0-9_\-]+)\s*=")


def workspace_manifests() -> list[str]:
    """Every crate manifest in the workspace.

    Enumerated by the pure tiered topology (ADR-0109): all three wildcard tiers.
    The workspace root manifest is excluded — it is the workspace, not a crate.
    """
    return [p for tier in TIERS for p in sorted(glob.glob(f"{tier}/*/Cargo.toml"))]


def root_members(text: str) -> list[str]:
    """The literal entry list of the root manifest's `[workspace] members`."""
    block = re.search(r"members\s*=\s*\[(.*?)\]", text, re.DOTALL)
    return re.findall(r'"([^"]+)"', block.group(1)) if block else []


def has_bin_target(path: str, text: str) -> bool:
    """Whether a manifest ships a binary target (`[[bin]]`, or an implicit `src/main.rs`)."""
    if "[[bin]]" in text:
        return True
    return os.path.exists(os.path.join(os.path.dirname(path), "src", "main.rs"))


def tier_violations() -> list[str]:
    """R5: gate the physical layout itself (ADR-0109 `[INV-ARCH-TIER-01]`).

    Computed independently of `workspace_manifests`, so a crate that escapes the topology is
    *reported* instead of silently vanishing from every other rule's input set.
    """
    problems: list[str] = []

    # `[INV-ARCH-TIER-01]`: the member list must be the sanctioned wildcard set, so a fourth tier
    # cannot be introduced by adding a line to the manifest.
    root_text = open("Cargo.toml", encoding="utf-8").read()
    members = root_members(root_text)
    if members != SANCTIONED_MEMBERS:
        problems.append(
            f"R5: root Cargo.toml `members` is {members}, expected {SANCTIONED_MEMBERS} "
            f"([INV-ARCH-TIER-01] wildcard admission)"
        )

    # Exactly one application host binary exists workspace-wide (`crates/nuo`).
    host_binaries = []
    for path in sorted(glob.glob("**/Cargo.toml", recursive=True)):
        parts = os.path.normpath(path).split(os.sep)
        if parts[0] == "target":
            continue
        text = open(path, encoding="utf-8").read()
        if has_bin_target(path, text):
            host_binaries.append(path)

        depth = len(parts) - 1  # directory components above the manifest
        if depth == 0:
            continue  # the workspace root manifest
        if depth == 1:
            problems.append(
                f"R5: crate manifest '{path}' sits at the workspace root; all crates must "
                f"reside within a sanctioned tier ({' | '.join(TIERS)}) ([INV-ARCH-TIER-01])"
            )
            continue
        if depth > 2:
            problems.append(
                f"R5: crate manifest '{path}' nests deeper than one tier; tiers are flat "
                f"([INV-ARCH-TIER-01])"
            )
            continue
        tier, crate_dir = parts[0], parts[1]
        if tier not in TIERS:
            problems.append(
                f"R5: crate manifest '{path}' is under unsanctioned tier '{tier}' "
                f"({' | '.join(TIERS)}) ([INV-ARCH-TIER-01])"
            )
            continue
        if tier == "crates":
            if crate_dir != "nuo" and not crate_dir.startswith("nuo-"):
                problems.append(
                    f"R5: crate directory 'crates/{crate_dir}' must be 'nuo' (host binary) or "
                    f"carry prefix 'nuo-' ([INV-ARCH-TIER-01])"
                )
        elif tier in TIER_PREFIX:
            prefix = TIER_PREFIX[tier]
            if not crate_dir.startswith(prefix):
                problems.append(
                    f"R5: crate directory '{tier}/{crate_dir}' must carry the tier namespace "
                    f"prefix '{prefix}' ([INV-ARCH-TIER-01])"
                )

    expected_binaries = [HOST_PATH]
    if host_binaries != expected_binaries:
        problems.append(
            f"R5: binary targets found in {host_binaries}, expected exactly {expected_binaries} "
            f"([INV-ARCH-TIER-01] single public binary, [INV-CLI-01])"
        )
    return problems

    expected_binaries = [HOST_PATH]
    if host_binaries != expected_binaries:
        problems.append(
            f"R5: binary targets found in {host_binaries}, expected exactly {expected_binaries} "
            f"([INV-TOPO-01] single public binary, [INV-CLI-01])"
        )
    return problems


def parse(path: str) -> tuple[str, set[str]]:
    """Return (package name, shipped dependencies).

    `[dev-dependencies]` are deliberately excluded: they are test-only and never
    part of the linked library or binary surface, so a test-time composition
    root does not violate server purity or the zero-IO substrate.
    """
    text = open(path, encoding="utf-8").read()
    name = "?"
    for line in text.splitlines():
        m = TARGET.match(line.strip())
        if m:
            name = m.group(1)
            break
    deps: set[str] = set()
    section = ""
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("["):
            section = s.strip("[]")
            continue
        if section in ("dependencies", "build-dependencies") and not s.startswith("#"):
            m = KEY.match(s)
            if m:
                deps.add(m.group(1))
    return name, deps


def main() -> int:
    if "--strict-ui" in sys.argv[1:]:
        pass
    paths = workspace_manifests()
    manifests = {name: (path, deps) for path, (name, deps) in
                 ((p, parse(p)) for p in paths)}

    problems: list[str] = tier_violations()

    for name, (path, deps) in sorted(manifests.items()):
        if name == "nuo-server":
            for bad in sorted(deps & (TERMINAL_SUBSTRATE | PRESENTATION_CRATES | RETIRED_CRATES)):
                problems.append(
                    f"R1: nuo-server ({path}) depends on presentation substrate "
                    f"'{bad}' (server purity)"
                )
        for bad in sorted(deps & RETIRED_CRATES):
            problems.append(
                f"R2: '{name}' ({path}) depends on retired presentation crate '{bad}'"
            )
        if name not in PRESENTATION_LAYER:
            for bad in sorted(deps & TERMINAL_SUBSTRATE):
                problems.append(
                    f"R2: '{name}' ({path}) depends on terminal substrate '{bad}'; "
                    f"only the presentation layer may"
                )
            for bad in sorted(deps & PRESENTATION_CRATES):
                problems.append(
                    f"R2: '{name}' ({path}) depends on presentation crate '{bad}'; "
                    f"only the presentation layer or `nuo` host binary may (client-server anti-inversion)"
                )

    # R3: Check zero-IO on both presentation crates.
    for target in ("nuo-ui-primitives", "nuo-view"):
        pkg = manifests.get(target)
        if pkg is None:
            problems.append(f"R3: crates/{target} is absent (presentation substrate)")
        else:
            path, deps = pkg
            for bad in sorted(deps & UI_FORBIDDEN):
                problems.append(
                    f"R3: {target} ({path}) declares forbidden IO dependency "
                    f"'{bad}' ([INV-ARCH-UI-ZERO-IO])"
                )

    # R4: Check primitives purity.
    primitives = manifests.get("nuo-ui-primitives")
    if primitives is not None:
        path, deps = primitives
        if "nuo-view" in deps:
            problems.append(
                f"R4: nuo-ui-primitives ({path}) depends on domain views 'nuo-view' ([INV-ARCH-UI-PRIMITIVES-PURITY])"
            )

    for retired in sorted(RETIRED_CRATES):
        if retired in manifests:
            problems.append(
                f"R2: crates/{retired} is retired and must not be present in the workspace "
                "(superseded by nuo-ui-primitives, nuo-view, and nuo::shell)"
            )

    if problems:
        print("presentation firewall: FAILED")
        for p in problems:
            print("  " + p)
        return 1
    print(
        f"presentation firewall: clean ({len(TIERS)} tiers, "
        f"{len(manifests)} manifests; R1/R2/R3/R4/R5 enforced)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
