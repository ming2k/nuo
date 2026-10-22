# nuo-inference-wire

Multi-vendor wire protocol serialization, SSE stream demuxing, and Session IR projection for AI model APIs.

## Overview

`nuo-inference-wire` acts as a bidirectional compiler and codec between internal canonical request structures and external vendor HTTP/SSE wire formats. It is completely independent of cognitive agent loops, making it suitable for standalone CLI tools, proxies, and client SDKs.

## Supported Protocols

### Generative Chat & Thinking Protocols (System Two)

- **OpenAI**: Chat Completions and Responses API.
- **Anthropic**: Messages API, Extended Thinking blocks, and Tool Use framing.
- **Google Gemini**: `generateContent` and Function Declarations.
- **DeepSeek**: Reasoning content and Chat Completions.
- **OpenAI-Compatible Local Engines**: Ollama, vLLM, and LocalAI.

### Calibrated Discriminative & Scoring Protocols (System One)

- **TypeSafe AI (Jev)**: Strongly typed discriminative evaluation (`POST /v1/systemone` / `POST /api/v1/decide`).
  - Supports `Noul` (calibrated boolean / yes-no probability), `Choice` (multi-class routing and confidence distributions), and `Score` (ordinal rubric evaluations).
  - Machine-native structured discriminative representations with zero hallucination and calibrated confidence estimates.

## Responsibilities

- **Dual-Lane Architecture**: Decouples generative multi-turn chat/thinking loops (`generative::*`) from deterministic, machine-native probabilistic discriminative models (`discriminative::*`) while sharing connection pooling, authentication, and HTTP transport.
- **Wire Codec**: Serializes canonical block IR into vendor API payloads.
- **Discriminative Engine**: Serializes `DiscriminativeRequest` and parses typed, calibrated probability distributions into `DiscriminativeResponse`.
- **SSE Stream Demuxer**: Parses raw Server-Sent Event (SSE) byte streams into normalized streaming events (`StreamDelta`, `StreamEnd`, `ThinkingDelta`).
- **Balanced JSON Framing**: Extracts balanced JSON tool arguments from partial streaming tokens without waiting for full stream completion.
- **Zero Domain Leakage**: Knows nothing about SQLite databases, user permissions, or agent personas.
