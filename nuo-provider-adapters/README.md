# nuo-provider-adapters

Concrete model provider adapters, wire transport engines, authentication flows, and live catalog resolution for Nuo (ADR-0015).

## Overview

`nuo-provider-adapters` implements the canonical contracts declared in `nuo-provider`. It acts as the technical adapter tier translating abstract model requests into vendor-specific HTTP/SSE wire calls. It isolates external LLM vendor protocols, OAuth authentication flows, and network egress away from the core execution harness.

## Subsystems

- **`protocol`**: Concrete `Provider` implementations:
  - `openai`: OpenAI Chat Completions & reasoning endpoints.
  - `anthropic`: Anthropic Messages API with extended thinking blocks and prompt caching.
  - `google`: Google Gemini API via `generateContent`.
- **`registry`**: Provider factory (`build_provider_for_channel`), template registry, and endpoint configuration.
- **`oauth`**: OAuth2 + PKCE credential acquisition supporting RFC 8628 device flows, browser loopbacks, and single-flight token refresh.
- **`list_models`**: Live remote model discovery (`GET /v1/models` and peers) implementing `CatalogDiscovery`.
- **`usage`**: Live quota and balance fetchers implementing `QuotaTracker`.
- **`prompt_cache`**: Vendor-specific prompt caching headers and breakpoint markers.
- **`vision`**: Multi-modal image format verification and payload encoding.
