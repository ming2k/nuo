//! QoderEncoding body codec: base64 alphabet substitution + outer-thirds swap.

use base64::Engine as _;
use nuo_provider::BodyCodecPhase;
use nuo_provider::ProviderError;

const QODER_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STANDARD_B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard-base64 byte → Qoder-alphabet byte, identity everywhere else.
///
/// A table, not a search: the substitution becomes a total lookup, so there is no
/// "base64 char not in the alphabet" case left to assert on.
const QODER_REMAP: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = i as u8;
        i += 1;
    }
    let mut i = 0;
    while i < STANDARD_B64.len() {
        table[STANDARD_B64[i] as usize] = QODER_ALPHABET[i];
        i += 1;
    }
    table
};

/// Encode raw JSON bytes using QoderEncoding.
pub fn encode_body(raw: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let remapped: String = b64
        .bytes()
        .map(|c| match c {
            b'=' => '$',
            other => QODER_REMAP[other as usize] as char,
        })
        .collect();
    outer_third_swap(&remapped)
}

/// Decode a QoderEncoding body.
#[allow(dead_code)]
pub fn decode_body(encoded: &str) -> Option<Vec<u8>> {
    let restored = outer_third_swap(encoded);
    let mut b64 = String::with_capacity(restored.len());
    for c in restored.bytes() {
        b64.push(match c {
            b'$' => '=',
            _ => {
                let idx = QODER_ALPHABET.iter().position(|&a| a == c)?;
                STANDARD_B64[idx] as char
            }
        });
    }
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

/// Swap outer thirds: `A‖B‖C` → `C‖B‖A` where `k = len/3`.
///
/// Permutes characters rather than bytes, so it cannot split a UTF-8 sequence;
/// on the pure-ASCII alphabets this codec handles the result is byte-identical
/// to a byte permutation, but no input can make it fail.
pub fn outer_third_swap(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let k = chars.len() / 3;
    let (a, rest) = chars.split_at(k);
    let (b, c) = rest.split_at(rest.len() - k);
    let mut out = String::with_capacity(s.len());
    out.extend(c);
    out.extend(b);
    out.extend(a);
    out
}

/// BodyCodecPhase implementation for Qoder.
#[derive(Debug, Clone, Default)]
pub struct QoderBodyCodec;

impl BodyCodecPhase for QoderBodyCodec {
    fn encode_body(&self, body_bytes: &[u8]) -> Result<Vec<u8>, ProviderError> {
        Ok(encode_body(body_bytes).into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qoder_encoding_roundtrips() {
        let input =
            b"{\"model\":\"qfmodel\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}";
        let encoded = encode_body(input);
        let decoded = decode_body(&encoded).expect("valid decode");
        assert_eq!(input.as_slice(), decoded.as_slice());
    }
}
