---
id: ADR-0021
title: "Daemon Image Content Identity and Idle-Gated Self-Heal for Rebuilt Binaries"
status: accepted
date: 2026-10-05
scope: runtime/daemon-lifecycle, client/discovery, process/identity, dev-loop
superseded_by: null
negative_knowledge: true
---

# 0021. Daemon Image Content Identity and Idle-Gated Self-Heal for Rebuilt Binaries

- Status: Accepted
- Date: 2026-10-05
- Deciders: Nuo Architecture Working Group
- Consulted: Runtime, Client SDK, and Developer-Experience maintainers
- Informed: System Architects
- Amends: the **same-version, same-protocol** refusal path of the daemon version gate (ADR-0100 rule 4) and the wire-protocol gate (ADR-0134)

---

## Context and Problem Statement

During local development the daily loop is: edit source, `cargo build`, run `nuo`. The `nuo` binary is a unified coordinator (ADR-0005): the foreground client finds the long-lived per-user daemon through a discovery record (`daemon.json`) and, when no compatible daemon is live, spawns one. The spawn target is the sibling source-built binary, pointed at by `NUO_BIN` in the dev environment (`nuo-tui::runner::ensure_dev_environment`).

Because the daemon is long-lived and detached, a rebuild replaces the on-disk binary **under a still-running daemon of the same version**. Version checks cannot see this: `CARGO_PKG_VERSION` is unchanged (a dev loop does not bump it), and the wire protocol number is unchanged (no protocol-affecting edit). Every coarse signal says "compatible", yet the client is about to talk to a process running a stale image. The pre-existing detection was a Linux-only inode comparison between `/proc/<pid>/exe` and the installed path — a signal that is correct on Linux but produces the recurring, confusing failure:

```text
Error: "client/daemon binary mismatch: running daemon (pid 3960990, version 0.0.2)
executable differs from the installed nuo core image (rebuilt binary).
Stop it with `nuo stop` and rerun — the daemon restarts on demand."
```

Two problems compound:

1. **The signal is not portable and can false-positive.** The inode probe is `#[cfg(target_os = "linux")]`; on macOS and Windows the check unconditionally returns "current", so dev-drift is *entirely* undetected there. Conversely, reinstalling an *identical* binary at the same path (a `cp`, a `cargo install` no-op) yields a new inode and a spurious "rebuilt binary" refusal — the inode moved even though the bytes did not.
2. **The remedy is manual and recurring.** The client refuses and tells the operator to run `nuo stop`, even when the daemon is hosting nothing. This is friction with zero safety benefit in the common case; a natural request was to *automatically* retire the stale daemon, perhaps keyed on "the most recent commit".

This ADR ratifies a content-based detection signal and an idle-gated self-heal, and — under [INV-AGENT-01] — records why the proposed commit/timestamp schemes were rejected.

---

## Decision Drivers

- **Correctness of the drift signal**: must never mislabel a healthy production daemon, and must be right precisely in the dev loop (the same-version, same-protocol, different-byte case).
- **Bounded cost, no bespoke metadata**: a constant-cost content signal, with no `build.rs`, git plumbing, or CI-injected provenance.
- **No silent work loss**: reclaiming a daemon must never interrupt in-flight agent rounds or rehosted services.
- **Legibility**: the comparison must be visible (`nuo status --diagnostic`), not an inode inference.
- **Backward compatibility**: pre-existing daemons (records without the new field) must keep working, on the old probe, without a hard upgrade.
- **Low cost**: no build-system plumbing (no `build.rs`, no CI-injected metadata) for a signal the file content already provides.

---

## Considered Options

- **Option 1 (Chosen)**: Publish the daemon's executable **content identity** (bounded digest + exact length) in the discovery record; the client compares it with the installed image; reclaim an idle daemon automatically, refuse with an enriched message when busy.
- **Option 2**: Keep the inode probe; add only automatic restart.
- **Option 3**: Embed the **git commit hash** and a **build timestamp** in the binary; order daemons by commit recency and auto-retire the older one.
- **Option 4**: Diagnostics/message-only — no automatic action.

---

## Decision Outcome

Chosen option: **"Option 1"**, because content identity is the only signal that answers the actual question ("is the process running the file that now sits at this path?") portably, with no false positives on identical reinstall, and it doubles as legible operator evidence.

### Mechanism

- **Daemon side**: at boot, before the image can be replaced, the daemon computes a **bounded content digest** of *the image it holds in memory* and publishes `image_digest` / `image_len` in the discovery record. Publication is best-effort: an unreadable image simply omits the field.
- **Client side**: `daemon_image_is_current(info)` resolves the installed image it would spawn and compares content. The comparison is length-first (cheap rejection), then digest. A record carrying **no** published digest (a pre-ADR-0021 daemon) falls back to the historical inode probe, so mixed-version fleets keep their existing behavior; the fallback never *adds* a refusal.
- **Bounded digest ([INV-IMG-06])**: the digest is the exact byte length folded with a positional sample of at most eight 64 KiB windows (head and tail always included). It is deliberately **not** a full-file hash: a 472 MB debug binary is digested on every daemon boot and, in the drift-eligible case, on every client start, so the cost must be constant (measured: ~0.4 ms vs ~1 s for the whole file). A relink rewrites the ELF head (build-id note) and tail (section headers), and any size change is caught outright, so sampled detection is strong in practice.
- **Fingerprint-source asymmetry ([INV-IMG-07])**: the two sides fingerprint *different objects on purpose*. The **client** fingerprints by **path** — the file it would exec. The **daemon** fingerprints the **held image** — `/proc/self/exe` on Linux, a magic link that resolves to the actually-loaded inode (and to `… (deleted)` once the on-disk file is replaced). It must *not* re-open `current_exe()`'s path string: on Linux `current_exe()` readlinks `/proc/self/exe` and returns the *dereferenced* path, so opening it follows the path to any newly dropped file — the exact dev-rebuild race this feature catches. On macOS/Windows there is no reliable handle to the held image, so the daemon publishes no digest and clients keep the inode/`None` path rather than a digest that might describe the wrong file.
- **Drift predicate**: dev-drift is the narrow conjunction *in-window protocol* ∧ *same product version* ∧ *image differs*. An out-of-window protocol is skew (not drift); a different product version is an upgrade leftover and remains deliberately served (the daemon retires on idle exit). Only the drift case is eligible for self-heal.
- **Idle-gated self-heal**: when `ensure_daemon` observes drift, it probes the daemon's live activity over the monitor protocol (which a drift daemon, by definition, still speaks). Reclaim is permitted only on `Idle` (zero active sessions, zero daemon tasks); the existing budget-aware `stop` pipeline (tiered graceful drain, ADR-0116) is reused, then the normal spawn path starts the freshly built image. `Busy` and `Unreachable` both refuse.

### Invariants & Behavioral Boundaries

- **[INV-IMG-01] Content is the authority when published**: when the discovery record carries `image_digest`, the drift verdict MUST be decided by executable content (length gate, then digest) — never by inode, path string, or any wall-clock/git ordering.
- **[INV-IMG-02] Absence of evidence is not drift**: an unreadable installed image, an unreadable daemon record, or a record with no published digest MUST resolve to "current" for the content path, preserving the pre-existing "never disturb a healthy production daemon" posture.
- **[INV-IMG-03] Reclaim only provably-idle daemons**: automatic self-heal MUST be gated on a positive idle probe (zero active sessions and zero daemon tasks). A busy or unreachable daemon MUST be refused with a message naming what would be lost.
- **[INV-IMG-04] No build-system coupling**: the signal MUST NOT depend on a `build.rs`, git metadata, or CI-injected provenance. Release and installed builds, which have no repository, MUST behave identically to dev builds.
- **[INV-IMG-05] Self-heal reuses the canonical stop pipeline**: reclaim MUST go through the identity-checked, budget-coordinated `stop` (ADR-0116), never an ad-hoc kill.
- **[INV-IMG-06] Constant-cost image digest**: the image digest MUST be bounded (a fixed number of sampled windows plus the exact length), never a whole-file hash, so daemon boot and client start never scale with binary size.
- **[INV-IMG-07] Fingerprint the right object**: the client MUST fingerprint the image at the resolved path (what it would spawn); the daemon MUST fingerprint the image it *holds* (`/proc/self/exe` on Linux), never re-resolving its own executable by path — re-resolving would attribute a freshly dropped file to a process that never loaded it.

### Positive Consequences

- On Linux the recurring manual `nuo stop` in the local loop disappears when the daemon is idle; the content signal replaces the inode probe at no extra cost.
- Identical-binary reinstall no longer false-positives, because equal bytes digest equal (in contrast to the inode comparison, which moved).
- `nuo status --diagnostic` names both images and their short digests, turning an opaque inode verdict into legible evidence; the "rebuilt-binary drift" diagnosis leads the report.
- No new build infrastructure; the signal is derived from a file that already exists.
- Backward compatible: an old daemon (no published digest) keeps the inode path; an old client ignores the new JSON fields (serde ignores unknown fields) and falls back as before.

### Negative Consequences & Trade-offs

- The digest is sampled, not exhaustive: two images of identical length and identical head/tail windows but different middles would compare equal. **Mitigation**: accepted as astronomically unlikely for a rebuilt binary (a relink rewrites both ends), and this traded a measured ~1 s whole-file hash per daemon boot for a constant ~0.4 ms — the correctness/speed tradeoff favours the bounded digest at every call site.
- **The daemon-side attestation is Linux-only today.** Reading the *held* image ([INV-IMG-07]) has a clean primitive only on Linux (`/proc/self/exe`); macOS and Windows have no reliable equivalent, so there the daemon publishes nothing and drift detection stays inert (as it is today). **Mitigation**: the content path activates everywhere a daemon can attest to its image; a future platform-native held-image handle (e.g. a Windows section-object query) would close the gap without a schema change.
- A daemon whose image is unreadable at boot publishes no digest and reverts to the Linux-only inode path on that host. **Mitigation**: accepted — this is strictly no worse than the prior behavior, and the daemon startup log remains the diagnostic surface.
- Two identity concepts now coexist during migration (published digest vs. legacy inode). **Mitigation**: the fallback is one branch, covered by tests, and can be deleted once no pre-ADR-0021 daemon remains in the field.

---

## Rejected Alternatives & Negative Knowledge

### Option 3 (Rejected): Git commit hash + build timestamp, ordered by recency
- **Why considered**: the operator's instinct — "compare commit and time, keep the newest, retire the older daemon" — is intuitive and would also cover the version-skew case.
- **Why rejected** (three distinct failures):
  1. **Wrong on the actual loop.** A dev rebuild frequently does **not** change the commit (uncommitted edits, `cargo build` at the same `HEAD`). Commit equality would report "same, keep it" precisely when the binary has changed — the exact case we must catch. Commit identity answers a different question than binary identity.
  2. **Ordering by time is unsafe and unreliable.** Deciding which daemon is "newer" from an embedded build timestamp requires trusting wall-clock ordering across processes and machines; clock skew, `cargo` mtime semantics, and cross-host installs all break it. Worse, "retire the older one" is a *blind* rule: it will destroy a daemon hosting live agent rounds for no wire-level reason — violating [INV-IMG-03].
  3. **Provenance cost.** It requires a `build.rs` (`git rev-parse`), an environment passthrough, and a no-repo fallback for every release/installed build — new build-system surface for a signal the file content already provides, violating [INV-IMG-04] in spirit. Retained here as negative knowledge so the approach is not resurrected ([INV-AGENT-02]).

### Option 2 (Rejected): Keep the inode probe, add only auto-restart
- **Why considered**: the smallest possible change — no record schema change, no digest.
- **Why rejected**: it inherits every defect of the inode probe. On macOS and Windows the check is inert, so auto-restart would simply never trigger there (no protection); and on Linux an identical reinstall would spuriously *reclaim a healthy daemon*. Fixing the trigger without fixing the signal builds an automatic action on top of a wrong predicate.

### Option 4 (Rejected): Diagnostics/message-only, no automatic action
- **Why considered**: zero risk of interrupting work; smallest behavioral change.
- **Why rejected**: it leaves the recurring friction this ADR exists to remove. The idle gate already makes the automatic path safe (zero active sessions and zero tasks), so refusing to act when the daemon is provably idle trades away all of the benefit for no safety gain. Message-only remains the behavior for the *busy* case, which is where preservation actually matters.

### Full-file SHA-256 (Rejected during implementation)
- **Why considered**: an exhaustive whole-file hash has no sampling residual — a matching digest proves byte-identical images, an ideal signal.
- **Why rejected**: measured against the real artifact, the debug test binary is 472 MB and hashing it whole costs ~1 s per daemon boot. Applied to every daemon start (and every client start on a drift-eligible record) it added ~17 s to the integration suite and widened a shared-instance-lock race between concurrently-started daemons, turning lifecycle tests flaky. The cost scales with binary size, violating the constant-cost requirement now recorded as [INV-IMG-06]. The bounded digest (length + ≤8 sampled windows) recovers the same drift signal at ~0.4 ms; the residual is negligible for the relink case that matters.

---

## Links

- Implementation: `nuo-client` (detection, drift predicate, idle probe, self-heal), `nuo-server` (record publication at boot), `nuo-host` (`image_digest_len` / `current_exe_digest_len`), `nuo` (`nuo status --diagnostic` rendering).
- Related ADRs: ADR-0100 (version gate rule 4, amended here for the same-version case), ADR-0116 (budget-coordinated stop reused for reclaim), ADR-0096 (global daemon discovery).
- Related docs: [Release & Versioning](../../dev/release-and-versioning.md).
