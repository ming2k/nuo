# nuo-providers

Concrete model provider implementations, authentication flows, and model catalog resolution for Nuo.

## Overview

`nuo-providers` serves as the secondary adapter translating abstract model requests into vendor-specific API calls. It isolates external LLM vendor protocols, OAuth authentication flows, and image/vision transformations away from the core execution harness.

## Subsystems

- **`protocol`**: Concrete `Provider` implementations:
  - `openai`: OpenAI Chat Completions & reasoning endpoints.
  - `anthropic`: Anthropic Messages API with extended thinking blocks and prompt caching.
  - `google`: Google Gemini API via `generateContent`.
  - `mock`: In-memory mock provider for testing and deterministic offline scenarios.
- **`registry`**: Provider factory (`build_provider_for_channel`), template registry, and endpoint configuration.
- **`oauth`**: OAuth2 + PKCE credential acquisition supporting RFC 8628 device flows, browser loopbacks, and single-flight token refresh.
- **`list_models`**: Live remote model discovery (`GET /v1/models` and peers) for dynamic model catalogs.
- **`prompt_cache`**: Vendor-specific prompt caching headers and breakpoint markers.
- **`vision`**: Multi-modal image format verification and payload encoding.
