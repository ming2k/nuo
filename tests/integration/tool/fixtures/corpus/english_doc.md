# Token Accounting and Context Management Architecture

## Abstract

This document specifies the layered token accounting model used across the agent runtime.
Token estimation directly drives prompt compaction, steering admission, and context window headroom protection.

## Architectural Layers

1. **Local Tokenizer Layer**:
   - The native Byte-Pair Encoding (BPE) tokenizer implements OpenAI's `cl100k_base` vocabulary.
   - Pre-tokenization regular expressions split input strings into distinct lexical classes before iterative rank-based merging.
   - Total encoding guarantees that every byte sequence can be mapped to valid tokens without loss.

2. **Context Pressure Projection**:
   - The context pressure monitor evaluates active message history against the designated model's maximum context length.
   - Soft thresholds (e.g. 70%) trigger progressive compaction strategies such as tool observation folding.
   - Hard thresholds (e.g. 90%) enforce immediate branch summarization and history pruning.

3. **Telemetry and Cross-Session Ledger**:
   - Request-level performance metrics track pre-request token projections versus provider-reported actual token usage.
   - Durable ledger records survive session termination to provide cross-session cost aggregation and analytics.
