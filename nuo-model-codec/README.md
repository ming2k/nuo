# nuo-model-codec

Multi-vendor wire protocol serialization, SSE stream demuxing, and Session IR projection for AI model APIs.

## Overview

`nuo-model-codec` acts as a bidirectional compiler and codec between internal canonical request structures and external vendor HTTP/SSE wire formats. It is completely independent of cognitive agent loops, making it suitable for standalone CLI tools, proxies, and client SDKs.

## Supported Protocols

### Generative Chat & Thinking Protocols (System Two)

- **OpenAI**: Chat Completions and Responses API.
- **Anthropic**: Messages API, Extended Thinking blocks, and Tool Use framing.
- **Google Gemini**: `generateContent` and Function Declarations.
- **DeepSeek**: Reasoning content and Chat Completions.
- **OpenAI-Compatible Local Engines**: Ollama, vLLM, and LocalAI.

### Calibrated Decision Protocols (System One)

- **TypeSafe AI (Jev)**: Strongly typed decision evaluation (`POST /v1/systemone` / `POST /api/v1/decide`).
  - Supports `Noul` (calibrated boolean / yes-no probability), `Choice` (multi-class routing and confidence distributions), and `Score` (ordinal rubric evaluations).
  - Machine-native structured decision representations with zero hallucination and calibrated confidence estimates.

## Responsibilities

- **Dual-Engine Architecture**: Decouples generative multi-turn chat/thinking loops from deterministic, machine-native probabilistic decision models while sharing connection pooling, authentication, and HTTP transport.
- **Wire Codec**: Serializes canonical `ModelRequest` and Session IR into vendor API payloads.
- **Decision Engine**: Serializes `DecisionRequest` and parses typed, calibrated probability distributions into `DecisionResponse`.
- **SSE Stream Demuxer**: Parses raw Server-Sent Event (SSE) byte streams into normalized streaming events (`StreamDelta`, `StreamEnd`, `ThinkingDelta`).
- **Balanced JSON Framing**: Extracts balanced JSON tool arguments from partial streaming tokens without waiting for full stream completion.
- **Zero Domain Leakage**: Knows nothing about SQLite databases, user permissions, or agent personas.
