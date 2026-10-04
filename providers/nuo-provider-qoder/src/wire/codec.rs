//! QoderEncoding body codec: base64 alphabet substitution + outer-thirds swap.

use base64::Engine as _;
use nuo_model_codec::ProviderError;
use nuo_provider_transport::pipeline::BodyCodecPhase;

const QODER_ALPHABET: &[u8; 64] =
    b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STANDARD_B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode raw JSON bytes using QoderEncoding.
pub fn encode_body(raw: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
    let remapped: String = b64
        .bytes()
        .map(|c| match c {
            b'=' => '$',
            _ => {
                let idx = STANDARD_B64
                    .iter()
                    .position(|&s| s == c)
                    .expect("base64 char");
                QODER_ALPHABET[idx] as char
            }
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
pub fn outer_third_swap(s: &str) -> String {
    let bytes = s.as_bytes();
    let k = bytes.len() / 3;
    let (a, rest) = bytes.split_at(k);
    let (b, c) = rest.split_at(rest.len() - k);
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(c);
    out.extend_from_slice(b);
    out.extend_from_slice(a);
    String::from_utf8(out).expect("byte-level permutation of ASCII")
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
        let input = b"{\"model\":\"qfmodel\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}";
        let encoded = encode_body(input);
        let decoded = decode_body(&encoded).expect("valid decode");
        assert_eq!(input.as_slice(), decoded.as_slice());
    }
}
