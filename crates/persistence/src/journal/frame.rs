//! Length-prefixed, CRC-protected frames for the session journal (ADR-0236).
//!
//! A frame is the only durable unit of the journal:
//!
//! ```text
//! magic  version  kind  flags  seq(u64)  payload_len(u32)  crc32c(u32)  payload
//!  4B      2B      1B    1B      8B          4B              4B         N B
//! ```
//!
//! The CRC covers the payload only; the fixed-size header is validated by
//! magic/version/kind and a bounded length before any allocation. A torn tail
//! therefore decodes as [`Decoded::Incomplete`], never as a partial commit.

/// File/format magic. Bumped only by a format break (ADR-0236 §5).
pub(crate) const MAGIC: [u8; 4] = *b"NJ01";
/// Frame format version. The journal refuses an unknown version outright.
pub(crate) const VERSION: u16 = 1;
/// Fixed header size in bytes.
pub(crate) const HEADER_LEN: usize = 24;
/// Defensive cap on one frame payload (one `ThreadData` delta/snapshot).
///
/// The cap is far above any real turn: a full 37 MiB transcript plus metadata
/// still fits. It exists so a corrupt length field cannot make the reader
/// allocate gigabytes before the CRC check rejects it.
pub(crate) const MAX_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;

/// The commit kind carried by the frame. Mirrors the SQLite save modes so the
/// journal and the derived projection can never disagree about what a frame
/// means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Metadata,
    Delta,
    Full,
    /// Terminal tombstone: the session was deleted as of this seq.
    Deleted,
    /// Append-only command act ledger entry (ADR-0249).
    CommandAct,
    /// Durable workspace effect lifecycle event (ADR-0249).
    WorkspaceEffect,
}

impl FrameKind {
    pub(crate) const fn from_save_mode(mode: crate::db::SaveMode) -> Self {
        match mode {
            crate::db::SaveMode::MetadataOnly => Self::Metadata,
            crate::db::SaveMode::Delta => Self::Delta,
            crate::db::SaveMode::FullRewrite => Self::Full,
        }
    }

    const fn as_u8(self) -> u8 {
        match self {
            Self::Metadata => 1,
            Self::Delta => 2,
            Self::Full => 3,
            Self::Deleted => 4,
            Self::CommandAct => 5,
            Self::WorkspaceEffect => 6,
        }
    }

    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Metadata),
            2 => Some(Self::Delta),
            3 => Some(Self::Full),
            4 => Some(Self::Deleted),
            5 => Some(Self::CommandAct),
            6 => Some(Self::WorkspaceEffect),
            _ => None,
        }
    }
}

/// Encode one frame. `payload` is copied into the returned buffer so the
/// caller can hand it to a lane thread without borrowing.
pub(crate) fn encode(seq: u64, kind: FrameKind, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.push(kind.as_u8());
    out.push(0); // flags: reserved, must be zero
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32c::crc32c(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// One decoded frame, borrowing its payload from the caller's buffer.
#[derive(Debug)]
pub(crate) struct DecodedFrame<'a> {
    pub(crate) seq: u64,
    pub(crate) kind: FrameKind,
    pub(crate) payload: &'a [u8],
}

/// Result of decoding the next frame from a byte slice.
#[cfg(test)]
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) enum Decoded<'a> {
    /// A complete, CRC-valid frame.
    Frame(DecodedFrame<'a>),
    /// Fewer bytes than the header or payload declares; the tail is torn.
    Incomplete,
    /// A complete header whose magic/version/kind/length/CRC is invalid.
    Corrupt(String),
}

/// One validated fixed-size header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Header {
    pub(crate) seq: u64,
    pub(crate) kind: FrameKind,
    pub(crate) payload_len: usize,
    pub(crate) crc: u32,
}

/// Validate and decode the 24-byte fixed header. The payload-length cap is
/// enforced here, before any reader allocates.
pub(crate) fn decode_header(bytes: &[u8]) -> Result<Header, String> {
    if bytes.len() < HEADER_LEN {
        return Err("journal frame header is incomplete".to_string());
    }
    if bytes[0..4] != MAGIC {
        return Err("journal magic mismatch".to_string());
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err(format!(
            "journal frame version {version} is not supported (expected {VERSION})"
        ));
    }
    let Some(kind) = FrameKind::from_u8(bytes[6]) else {
        return Err(format!("unknown journal frame kind {}", bytes[6]));
    };
    if bytes[7] != 0 {
        return Err("journal frame flags are not zero".to_string());
    }
    let seq = u64::from_le_bytes(bytes[8..16].try_into().unwrap_or([0; 8]));
    let payload_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap_or([0; 4])) as usize;
    if payload_len > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "journal frame payload length {payload_len} exceeds the {MAX_PAYLOAD_BYTES}-byte cap"
        ));
    }
    let crc = u32::from_le_bytes(bytes[20..24].try_into().unwrap_or([0; 4]));
    Ok(Header {
        seq,
        kind,
        payload_len,
        crc,
    })
}

/// Validate a header against its already-read payload.
pub(crate) fn verify_payload<'a>(
    header: Header,
    payload: &'a [u8],
) -> Result<DecodedFrame<'a>, String> {
    if payload.len() != header.payload_len {
        return Err(format!(
            "journal frame seq {} payload length {} does not match the header ({})",
            header.seq,
            payload.len(),
            header.payload_len
        ));
    }
    let actual = crc32c::crc32c(payload);
    if actual != header.crc {
        return Err(format!(
            "journal frame seq {} CRC mismatch ({actual:#010x} != {:#010x})",
            header.seq, header.crc
        ));
    }
    Ok(DecodedFrame {
        seq: header.seq,
        kind: header.kind,
        payload,
    })
}

/// Decode the frame at the front of `bytes`.
#[cfg(test)]
pub(crate) fn decode(bytes: &[u8]) -> Decoded<'_> {
    if bytes.len() < HEADER_LEN {
        return Decoded::Incomplete;
    }
    let header = match decode_header(bytes) {
        Ok(header) => header,
        Err(error) => return Decoded::Corrupt(error),
    };
    let total = HEADER_LEN.saturating_add(header.payload_len);
    if bytes.len() < total {
        return Decoded::Incomplete;
    }
    match verify_payload(header, &bytes[HEADER_LEN..total]) {
        Ok(frame) => Decoded::Frame(frame),
        Err(error) => Decoded::Corrupt(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_frame() {
        let bytes = encode(7, FrameKind::Delta, b"hello");
        match decode(&bytes) {
            Decoded::Frame(frame) => {
                assert_eq!(frame.seq, 7);
                assert_eq!(frame.kind, FrameKind::Delta);
                assert_eq!(frame.payload, b"hello");
            }
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    #[test]
    fn a_truncated_payload_is_incomplete_not_corrupt() {
        let bytes = encode(1, FrameKind::Full, b"abcdef");
        assert!(matches!(
            decode(&bytes[..HEADER_LEN + 2]),
            Decoded::Incomplete
        ));
        assert!(matches!(decode(&bytes[..3]), Decoded::Incomplete));
    }

    #[test]
    fn a_flipped_payload_byte_is_corrupt() {
        let mut bytes = encode(1, FrameKind::Full, b"abcdef");
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        assert!(matches!(decode(&bytes), Decoded::Corrupt(_)));
    }

    #[test]
    fn a_bogus_length_is_rejected_before_allocation() {
        let mut bytes = encode(1, FrameKind::Full, b"abc");
        bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(decode(&bytes), Decoded::Corrupt(_)));
    }
}
