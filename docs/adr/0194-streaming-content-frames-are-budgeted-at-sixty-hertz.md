---
id: ADR-0194
title: "Streaming Content Frames Are Budgeted at 60Hz"
status: accepted
date: 2026-11-15
scope: tui/event-loop, shell/translators, performance
superseded_by: null
supersedes: []
negative_knowledge: true
---

# 0194. Streaming Content Frames Are Budgeted at 60Hz

- Status: Accepted
- Date: 2026-11-15

## Context and Problem Statement

The TUI deliberately coalesced high-frequency `StreamDelta` events into a
100ms animation heartbeat. That protected the renderer from a provider that
emits one delta per token, but it also capped the visible text rate at 10Hz.
The animation heartbeat and the content frame budget are different concerns:
a spinner only needs to advance ten times a second, while streamed prose should
track the model closely enough to feel live.

The opposite failure is equally bad: waking the event loop for every delta
turns a fast stream into a repaint storm. The renderer's diff makes emission
cheap, but layout and the retained scene still cost real time, and a modal
frame with a backdrop wash is measurably more expensive.

## Architectural Decisions

### 1. Content frames have their own 16ms budget

`STREAM_FRAME_INTERVAL` is 16ms. The event loop records the last paint and
defers a content frame to the end of that budget instead of painting on every
notification. The deferred frame is not dropped: `input_redraw_pending` keeps
the dirty state alive and `next_poll_interval` wakes exactly at the remaining
budget.

### 2. The animation heartbeat remains 100ms

Spinner phases, the breathing anchor, and the empty-state carousel keep their
existing cadence. They are not coupled to text repaint. A frame that is only an
animation tick still runs at 10Hz; a frame carrying streamed text runs at up to
60Hz.

### 3. The translator schedules one trailing wake per budget

The response translator wakes immediately for the first coalescible delta,
then suppresses further wakes inside the 16ms window. It schedules exactly one
trailing wake for the remaining time using an `AtomicBool` guard, so a burst
of deltas cannot enqueue an unbounded number of timers. Non-stream events
(tool calls, completion, errors) always wake immediately.

### 4. A deferred frame is not a lost frame

`runtime.dirty` is consumed by the frame decision, so deferral re-arms
`input_redraw_pending`. No state mutation depends on a paint having happened
in the same loop iteration; the scroll clamp and staged commit paths run only
on the frame that actually paints.

## Consequences

- **Positive**: streamed text is smooth at up to 60Hz; animation stays cheap;
  a delta burst produces at most one paint per 16ms.
- **Negative & Incurred Costs**: the loop now has two cadences to reason about.
  Any future content producer must mark itself dirty rather than assuming the
  animation heartbeat will carry it.
- **Neutral**: the 16ms constant is shared by the translator and the event
  loop; it is the single definition of the content frame budget.

## Discarded Alternatives (Negative Knowledge)

- **Raise the animation heartbeat to 16ms**: repaints an unchanged spinner at
  60Hz and makes modal backdrops far more expensive.
- **Paint on every delta with no budget**: a fast provider can starve input
  handling and burn CPU on layout.
- **Drop deltas inside the translator**: loses text. The translator schedules
  a trailing wake; it never discards state.

## Links

- [ADR-0185](0185-storybook-retirement-and-single-presentation-path.md)
- [ADR-0186](0186-state-is-partitioned-and-giants-are-prohibited.md)
