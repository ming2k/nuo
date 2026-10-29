---
id: ADR-0127
title: "Workspace Trust Review Obligation: One Predicate, A Request That Can Be Answered, and A Refusal That Records Itself"
status: accepted
date: 2026-10-29
scope: host/nuo-host, persistence/nuo-persistence, protocol/nuo-tool, protocol/nuo-protocol, server/nuo-server, presentation/nuo-view, host/nuo, tui/event-loop, tui/pre-attach
superseded_by: null
negative_knowledge: true
---

# 0127. Workspace Trust Review Obligation: One Predicate, A Request That Can Be Answered, and A Refusal That Records Itself

- Status: Accepted
- Date: 2026-10-29
- Deciders: Nuo Architecture Working Group
- Consulted: Security, Server Core, Presentation, TUI Interaction
- Amends: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — the `TrustChanged` row in its layer table now splits into a condition and a failure

---

## Context and Problem Statement

The workspace trust gate existed to answer one question before a session became
useful: *may this project's authored content load?* Three defects made the
answer unreliable, and they were only visible in combination.

### 1. The review obligation had two definitions, and they disagreed

`WorkspaceTrustState` has six values. "Needs a human decision" was computed in
at least three places, each with its own list:

| Site | Accepted states |
|---|---|
| `nuo_view::trust_gate::quarantined_domains` (the gate) | `Quarantined \| Expired` |
| `nuo_server::serve::workspace_trust_notice` (the banner) | `Changed` only |
| `nuo_server::session_driver` (grant candidates) | `Quarantined \| Changed \| Expired` |

The gate and the banner lists were disjoint. Consequently `Changed` — the state
meaning *the bytes you approved are not the bytes on disk* — was the one posture
that could not gate. It escalated to a banner instead, which in the shell routes
to a transient toast (`SurfaceCapabilities::SHELL` declares no banner row), so a
user saw a warning for a few seconds on every attach and could scroll past it.

The escalation ladder was inverted. `Quarantined` (never trusted, nothing loaded
yet, so nothing has run) got a blocking full-screen decision. `Changed` (content
mutated under an existing approval — via `git pull`, a collaborator, or the agent
itself editing `AGENTS.md`) got a toast. The weakest affordance belonged to the
strongest reason to ask.

The rationale recorded in `serve.rs` was that a trust decision had already been
made for the workspace, so there was "nothing to gate on". That reasoning does
not hold: a digest-bound decision is a statement about *specific bytes*. Content
that changed after approval is exactly as unauthorized as content that was never
approved — the earlier decision simply does not transfer to it.

### 2. The request had no answer that ended it

The banner said `Run /trust to approve and reload, or /untrust to ignore.` Both
halves were wrong for the purpose:

- `/trust` (no argument) expands to `GrantAll`, silently trusting every present
  domain — not the per-domain decision the gate offers.
- `/untrust` maps to `TrustRoute::Revoke`, which calls `revoke_workspace` and
  **deletes the whole record**. The next snapshot reports `Quarantined`, so the
  full-screen gate opens, and every unrelated grant (MCP, skills, hooks,
  ex-workspace) is gone. It is the opposite of "ignore".

The mechanism that *does* mean "stop asking about this content" already existed:
`WorkspaceSecurityStore::deny_domains` records the refused digest, so the domain
reports `Denied` (settled) and re-arms as `Changed` only when the bytes move
again. It was reachable from exactly one place — the pre-attach gate's
unselected rows in `session_driver` — and from no command at all. The escalation
message therefore pointed at an exit that did not exist.

### 3. The fallback escalation was addressed to the wrong client

The banner was pushed to every attaching client. For an interactive client the
gate already carries the same snapshot through attach-sync, so the banner is a
duplicate of the dialog on screen. For an autonomous client the gate is useless —
a question parked on a channel nobody reads freezes the security decision
forever — so the banner is the only thing that can work. The server had no notion
of which case it was in; it guessed from the *state* rather than the *posture*.

### 4. The prompt could say "quit" as an answer

While the gate could only open on a never-trusted workspace, dismissing it
quitting the TUI was defensible: an untrusted workspace had no useful work to do.
Once `Changed` gates too, that workspace is mid-session — transcript, task list,
and a user who only wanted to say "no" to a diff. Terminating the terminal is not
an answer to that question.

### 5. Two rule lists had already drifted

`nuo_server::project::PROJECT_RULE_FILES` listed three files
(`AGENTS.md`, `.cursorrules`, `.windsurfrules`) that are concatenated into the
system prompt. `nuo_persistence::workspace_security::INSTRUCTION_PATHS` covered
only `AGENTS.md`. A trusted workspace could therefore have arbitrary prompt text
injected through the other two files — from a clone, a vendored tree, or a
dependency fixture — with no attestation whatsoever. Prompt injection for an
agent holding tools is execution-by-proxy, which is the precise hazard the trust
gate exists to prevent.

---

## Decision Drivers

- **The obligation must be one predicate.** Three lists is how a state ends up in
  the gate and not the banner, or the reverse.
- **Risk must map to affordance monotonically.** The more surprising the content,
  the stronger the interruption.
- **A security prompt must be answerable.** Every request the system makes has to
  name a decision that ends it; a prompt whose only exits are "trust everything"
  and "destroy the record" is a prompt that gets dismissed without being read.
- **A refusal is a decision, not an absence of one.** It must be recorded so the
  same content is not re-asked, and it must not destroy unrelated authority.
- **Server purity.** `nuo-server` is headless and must never link presentation
  crates ([INV-ARCH-TIER-01]), so any wording or label it composes has to come
  from a layer it is allowed to depend on.
- **One trust boundary per asset.** A file that reaches the system prompt is
  attested by the domain that governs it.

---

## Architectural Decisions

### 1. `WorkspaceTrustState::needs_review` is the single definition

The obligation moves into the domain layer (`nuo-host`), where every consumer can
read it:

```rust
pub fn needs_review(self) -> bool {
    matches!(self, Self::Quarantined | Self::Changed | Self::Expired)
}
```

`WorkspaceSecuritySnapshot` exposes the enumerations derived from it —
`domains_needing_review()` (display order, `Absent` skipped) and `needs_review()`.
Every consumer now delegates:

- `nuo_view::trust_gate::quarantined_domains` → `snapshot.domains_needing_review()`
- `nuo_server::serve` attach escalation → `snapshot.needs_review()`
- `nuo_server::session_driver` grant candidates → `state.needs_review()`
- `nuo_server::handlers_slash` `/trust deny` target set → `domains_needing_review()`

`Absent` (nothing to decide), `Trusted` (decided, digest matches), and `Denied`
(decided, refused) are settled. Re-prompting on `Denied` would recreate the
nagging loop the refusal exists to stop.

`TrustDomain::label` likewise moves into `nuo-host`, so the gate (which may link
it) and the server's banner (which may not) cannot name the same asset two
different ways.

### 2. The gate opens on the whole review obligation; the banner is the
   unanswerable-client fallback

`Changed` and `Expired` join `Quarantined` at the pre-view gate. The gate's
question states *which* decision it is — a first contact and a re-review have
identical options but are different questions, and telling a user "authorize
these assets" when they already did, and the bytes changed underneath them,
hides the one fact that makes the decision informed:

```
Previously trusted content changed on disk: AGENTS.md.
The earlier approval was for different content, so it does not carry over.
Review the changes before authorizing them again.
Unselected assets stay inactive for this session and will not be asked about
again unless their content changes.
```

The attach banner survives for exactly one case, gated on
`HumanChannelPosture::Interactive` rather than on the trust state:

```rust
let gate_owned = after_attach == HumanChannelPosture::Interactive;
if snap.needs_review() && !gate_owned { /* push the banner */ }
```

An interactive client gates; an autonomous client is told. The authority on "can
this client answer?" is the posture the client declared, not a guess.

### 3. `NoticeKind::TrustChanged` splits into a condition and a failure

Once the gate mounts on `Changed`, the shell could no longer intercept every
`TrustChanged` notice into the gate's error slot: a condition *is* what opened
the gate, so feeding it there would render the workspace's own state as an error.

Two opposite facts get two kinds:

| Kind | Meaning | Severity | Layer |
|---|---|---|---|
| `TrustChanged` | A condition awaiting a human decision | `Warning` | `Banner` (or the gate) |
| `TrustFailure` | A decision could not be applied | `Error` | `SurfaceLog` (raises a bubble) |

`TrustFailure` is a *receipt*: the failed decision is a past event with a cause
worth re-reading and pasting, and its `Error` severity makes it bubble and name
the notice log for free. The shell's gate interception now matches
`TrustFailure` only.

### 4. A refusal records itself and yields, instead of quitting

`PreAttachDecision::Quit` becomes `Decline { domains }`. The domains are the ones
*offered but unselected* — computed at the surface, where the offered set is
known, rather than inferred later from "whatever is still pending". Submitting an
empty selection is a `Decline`, not a zero-domain grant (which would ask the
server to do nothing and let the gate re-open).

The decline path dispatches the same control-plane request a grant does, and it
latches `trust_gate_dismissed` immediately so the snapshot the refusal is *about*
cannot re-mount the gate in the window before the server republishes. The session
continues with every affected domain unloaded.

While a decision is in flight the surface is **inert**, including for `Esc`. The
first draft let `Esc` refuse the offered set — which included the domains just
submitted — so the outcome depended on whether the grant or the refusal reached
the store first. That is a race with a security answer; the republished snapshot
(or `attestation_error`) is the only authority on what the decision did.

**The decision is carried explicitly, not inferred from the domain list.** The
first draft of this change sent the refused domains in `TrustWorkspace.domains` —
the field that means *"grant these"*. The server would have granted exactly what
the user refused: a security inversion in the one code path whose entire purpose
is to record a refusal. No type caught it, because a `Vec<TrustDomain>` does not
say which direction it points.

`AgentRequest::TrustWorkspace` therefore gained a `decision: TrustDecision`
(`Grant` | `Deny`) and the protocol version moved to 16. The field has no serde
default: every default would have to pick a direction, and the only safe direction
for a malformed trust frame is to reject it. The server has two distinct branches:
`Grant` trusts the selection and denies `candidates \ selected`; `Deny` refuses
exactly what was named and touches nothing else — crucially unlike the revoke path,
which drops every grant for the workspace.

### 5. `/trust deny [domain]` is the exit the message always promised

`TrustRoute` gains `DenyAll` and `Deny(TrustDomain)`, parsed with the **same**
domain vocabulary as a grant, and `deny_domains` becomes reachable from the
command layer:

```
/trust deny              # refuse every domain awaiting review
/trust deny instructions # refuse one (AGENTS.md, .cursorrules, .windsurfrules)
```

The handler reports what was refused, what is still pending, and states the
contract: *refused content stays inactive and will not be asked about again until
the content changes on disk.* `/untrust` keeps its meaning (revoke every grant)
and its no-argument grammar, but its rejection now names `/trust deny` so a user
looking for a per-domain refusal is not left to guess.

### 6. The rule-file list is one constant

`nuo_host::PROJECT_RULE_FILES` is the single list, consumed by both the loader
(`nuo_server::project`) and the Instructions digest
(`nuo_persistence::workspace_security::INSTRUCTION_PATHS`). The two can no longer
disagree, and adding a rule file adds it to both the prompt and the attestation.

### 7. The surface is tested by what it paints

The gate owns the entire terminal while it is up, so its painted output *is* the
user's whole view — and it had no render coverage at all. Everything asserted
above lived in `PreAttachState`; none of it was checked in the pixels. Adding that
coverage immediately found two defects that had shipped:

- **The key-hint footer was silently truncated.** It was pushed as a bare
  `Line`, and a `Line` wider than the panel is clipped rather than wrapped, so the
  rendered footer read `Enter confirm   Es`. The exit hint never reached the
  screen at any width. It now wraps, and is short enough to stay on one line at
  the panel's narrowest usable width.
- **The panel was 16% of the terminal, not the documented 60%.** The constant was
  `6` while its comment claimed "roughly 60%" and the code divided by it, giving
  an 80-column terminal a 13-column panel. That is what forced mid-word breaks in
  the security prompt itself (`…different conten` / `t, so…`) — the sentence the
  decision depends on. The geometry now means what it says, and the question body
  dropped a redundant 2-column indent that was eating wrap width inside margins
  the panel already provides.

### Normative Invariants Established

- `[INV-TRUST-01] One review obligation`: "needs a human decision" is
  `WorkspaceTrustState::needs_review`, and every gate/banner/command path reads
  it. Enforced by `nuo_host::security::tests` (exhaustive over all six states)
  and by the gate/banner integration tests.
- `[INV-TRUST-02] Risk maps to affordance`: any state in `needs_review` opens the
  blocking gate for an interactive client. Enforced by
  `trust_gate::tests::changed_domains_gate_exactly_like_never_trusted_domains`
  and `serve_integration::changed_workspace_gates_on_attach_instead_of_bannering`.
- `[INV-TRUST-03] Every request is answerable`: a review request must name a
  decision that ends it without destroying unrelated authority —
  `/trust deny`. Enforced by `handlers_slash::tests::deny_is_a_real_subcommand_with_the_grant_grammar`.
- `[INV-TRUST-04] A refusal is durable and content-bound`: refusing records the
  refused digest, so the same content is never re-asked and changed content is.
  Enforced by `pre_attach_gate::a_denied_snapshot_retires_the_gate_and_never_re_opens`
  and `pre_attach_gate::changed_content_after_a_refusal_is_asked_again`.
- `[INV-TRUST-05] One trust boundary per prompt file`: every file
  `load_project_rules` reads is covered by the Instructions digest. Enforced by
  `workspace_security::tests::every_project_rule_file_is_attested_by_the_instructions_domain`
  and `the_attested_rule_list_is_the_loaded_rule_list`.
- `[INV-TRUST-06] Condition and failure are distinct`: a trust *condition* and a
  trust *application failure* are different notice kinds with different layers.
  Enforced by `nuo_view::model::layer::tests::a_trust_failure_is_a_receipt_while_a_trust_change_is_a_condition`.
- `[INV-TRUST-07] A decision is never inferred from a domain list`: the intent to
  grant and the intent to refuse travel as distinct wire values, and a frame that
  omits the intent is rejected rather than defaulted. Enforced by
  `nuo_protocol::events::tests::a_trust_decision_is_carried_explicitly_not_inferred_from_the_domains`
  and `workspace_security::tests::grant_then_refuse_leaves_each_domain_with_its_own_decision`.
- `[INV-TRUST-08] An in-flight decision is not re-answered`: once submitted, the
  surface is inert until the server republishes. Enforced by
  `pre_attach::tests::navigate_then_submit_trust_all_emits_a_grant`.
- `[INV-TRUST-09] The surface's claims are tested in pixels`: what the gate
  *says* is asserted against the painted grid, not against its state object —
  because the gate is the whole screen. Enforced by
  `shell::snapshot_tests::pre_attach_surface::*`, which cover the exit hint
  (present and unclipped at every supported width), the re-review wording, the
  first-contact wording, and the absence of mid-word breaks in the prompt.

---

## Consequences

- **Positive**: The escalation ladder is monotonic — the more surprising the
  content, the stronger the interruption. The full-screen decision is answerable
  in one keystroke that does not revoke anything else, and the answer is durable,
  so the "warning on every launch" symptom is structurally impossible: either the
  user decided (settled), or the content changed again (a genuinely new
  question). The server and the client can no longer disagree about what needs
  review, because there is one predicate and one domain-label function.
- **Positive**: A previously unattested prompt-injection path
  (`.cursorrules`/`.windsurfrules`) is closed, and the two lists that allowed it
  are now one constant.
- **Negative & Trade-offs**: A `Changed` workspace now *blocks* on attach for an
  interactive client, where it previously let the user straight into a session
  with project rules silently unloaded. That is the intended trade — the old
  behavior let a user work in a workspace whose governance they had not approved —
  but it is a real behavior change for anyone who relied on scrolling past the
  toast. The mitigation is the decline path: one `Esc` continues the session
  untrusted, and the refusal is remembered.
- **Negative & Trade-offs**: `NoticeKind` grows by one variant, which breaks any
  exhaustive match outside this repository. Inside it, the closed-enum
  exhaustiveness is the mechanism that forced every routing decision to be made
  deliberately.

---

## Rejected Alternatives & Negative Knowledge

### Alternative A: keep `Changed` as a banner, but make the banner persistent

- **Why considered**: Smallest change — no gate lifecycle work, no `Esc`-semantics
  change, no new notice kind.
- **Why rejected**: It fixes the symptom (transience) and keeps the defect (a
  banner is not a decision surface). A persistent banner still cannot express
  "trust these two domains, refuse the third", still cannot be answered without a
  slash command, and still leaves the user to remember the exact command name.
  It also does not solve the duplicate-banner problem for interactive clients,
  because there is still no notion of which client can answer.

### Alternative B: give the gate its own wire message instead of reusing
   `TrustWorkspace`

- **Why considered**: The gate is a distinct flow; a bespoke message would let it
  carry richer semantics (per-domain decisions in one frame).
- **Why rejected**: `TrustWorkspace` already does exactly this — grant the
  selected, deny the rest, reload, republish — and it is the same path `/trust`
  uses, so persistence and the atomic live reload stay owned by one code path. A
  second message would be a second source of truth for the grant/deny split,
  which is how the server-side "infer the refusals from what is still pending"
  logic appeared in the first place.

### Alternative C: make `/untrust` mean "deny" and add `/revoke` for the old
   behavior

- **Why considered**: Keeps the banner's original wording valid with no new verb.
- **Why rejected**: `/untrust` is documented in `docs/reference/commands.md` and
  already shipped as "revoke project asset trust". Quietly redefining an existing
  verb to mean something weaker would make an existing muscle memory stop
  revoking — a security-relevant surprise. A new verb (`deny`) is additive and
  self-describing, and the old verb keeps its documented meaning.

### Alternative D: treat `Denied` as `needs_review` so the user can revisit

- **Why considered**: Symmetry — one predicate for "not trusted".
- **Why rejected**: It reintroduces the nagging loop. `Denied` exists precisely to
  record a decision that should not be re-asked; content that changes flips to
  `Changed`, which is the correct re-arm signal. A user who wants to revisit a
  refusal runs `/trust <domain>` explicitly.

### Alternative E: keep `Quit` for a first-contact workspace

- **Why considered**: Preserves the old "nothing can run here anyway" reasoning
  for the `Quarantined` case, where it was genuinely true.
- **Why rejected**: Two dismissal semantics for one surface, branching on which
  trust state mounted it, is a distinction the user cannot see and does not want
  to learn. The session can be continued untrusted in both cases, so there is no
  case left where quitting is the only honest answer. `Ctrl-C` remains the
  universal escape hatch.

---

## Links

- Amends: [ADR-0116](0116-content-residency-and-surface-layer-topology.md) — notice
  layer routing; the `TrustChanged` row splits into condition and failure
- Related: [ADR-0121](0121-notice-log-residency-retained-disclosure-record-and-bubble-as-pointer.md)
  — why a fault raises a bubble and names the log, which `TrustFailure` relies on
- Related: [ADR-0110](0110-running-state-interaction-topology-and-action-permissibility-discipline.md)
  — the pre-view gate's claim on the keyboard
- Postmortem: [2026-10-18 — Trust Gate Confirm Loop](../dev/postmortems/2026-10-18-trust-gate-loop-persistence-reader-wedge.md)
  — the earlier failure where a persist failure looped the gate silently; the
  condition/failure split is a second layer of defense against that class
