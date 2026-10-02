# Vendored data

Compact, generated binaries embedded by workspace crates. Each entry lists
its upstream source, the generator that produced it, and why the raw upstream
form is not used directly.

## `cl100k_base.packed`

- **Upstream:** OpenAI `cl100k_base` BPE vocabulary
  <https://openaipublic.blob.core.windows.net/encodings/cl100k_base.tiktoken>
  (1 681 126 B, 100 256 ranks; base64 token + rank per line, ordered by rank).
- **Format:** little-endian, all counts unsigned:
  `u32 version = 1`, `u32 token_count`, `u64 blob_len`, then
  `token_count × u32` byte lengths, then the `blob_len` token bytes
  concatenated **in rank order** (so a token's blob offset + length is its
  tiktoken rank — no per-entry rank field).
- **Size:** 1 044 878 B (≈35% smaller than upstream; the base64/hex text and
  per-line rank text are gone).
- **Generator:** a small Python script (offline, not committed as a build
  step — regeneration is manual and rare):

  ```python
  import base64, struct
  tokens = [base64.b64decode(b) for b, _ in
            (line.split() for line in open('cl100k_base.tiktoken', 'rb') if line.strip())]
  ranks = [int(r) for _, r in (line.split() for line in open('cl100k_base.tiktoken', 'rb') if line.strip())]
  assert ranks == list(range(len(tokens)))
  blob = b''.join(tokens)
  with open('cl100k_base.packed', 'wb') as f:
      f.write(struct.pack('<IIQ', 1, len(tokens), 0))
      f.write(struct.pack('<Q', len(blob)))
      f.write(struct.pack(f'<{len(tokens)}I', *map(len, tokens)))
      f.write(blob)
  ```

- **Consumed by:** `nuo-contracts/src/tokenizer.rs`
  (`include_bytes!`), parsed lazily on first use into a rank map.
- **Upstream license note:** the vocabulary data is published by OpenAI for
  use with tiktoken (MIT-licensed implementation); the token strings are
  training-derived data distributed openly for client-side token counting.
