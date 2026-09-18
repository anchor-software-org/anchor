//! Fixed binary framing for screen/camera video paths.
//!
//! Control metadata remains protobuf on the capability control stream. Encoded
//! access units use this small header so receivers can discard stale or
//! mismatched fragments without parsing codec bytes first. The same ANFR
//! records can be carried in QUIC datagrams or in the length-prefixed reliable
//! stream framing below.

use thiserror::Error;

pub const FRAME_MAGIC: [u8; 4] = *b"ANFR";
pub const FRAME_VERSION: u8 = 1;
pub const FRAME_KIND_SCREEN: u8 = 1;
pub const FRAME_KIND_CAMERA: u8 = 2;
pub const FLAG_KEYFRAME: u16 = 1;
pub const FLAG_CODEC_CONFIG: u16 = 1 << 1;
pub const FRAME_HEADER_BYTES: usize = 52;
// Keep headroom below the IPv6/Tailscale path MTU. Quinn and MsQuic add
// different packet protection overheads, so using the common 1,100-byte
// application datagram avoids path-MTU drops on otherwise healthy links.
pub const FRAME_DATAGRAM_BYTES: usize = 1100;
pub const FRAME_PAYLOAD_BYTES: usize = FRAME_DATAGRAM_BYTES - FRAME_HEADER_BYTES;
pub const MAX_FRAME_FRAGMENTS: usize = u16::MAX as usize;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
/// Reliable streams are byte-oriented, so each existing ANFR packet gets a
/// little-endian length prefix. This keeps the frame record identical on the
/// wire while making arbitrary QUIC stream read boundaries harmless.
pub const STREAM_PACKET_LENGTH_BYTES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: u8,
    pub flags: u16,
    pub capability_session_id: u64,
    pub flow_id: u64,
    pub sequence: u64,
    pub fragment_index: u16,
    pub fragment_count: u16,
    pub presentation_time_us: u64,
    pub codec_config_id: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame datagram is shorter than its fixed header")]
    Truncated,
    #[error("frame datagram has an invalid magic")]
    InvalidMagic,
    #[error("unsupported frame header version {0}")]
    UnsupportedVersion(u8),
    #[error("frame datagram has an invalid kind {0}")]
    InvalidKind(u8),
    #[error("frame datagram has an invalid fragment index/count")]
    InvalidFragments,
    #[error("frame exceeds the {0}-byte limit")]
    TooLarge(usize),
}

impl FrameHeader {
    pub fn encode(self, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        if !matches!(self.kind, FRAME_KIND_SCREEN | FRAME_KIND_CAMERA) {
            return Err(FrameError::InvalidKind(self.kind));
        }
        if self.fragment_count == 0 || self.fragment_index >= self.fragment_count {
            return Err(FrameError::InvalidFragments);
        }
        if payload.len() > FRAME_PAYLOAD_BYTES {
            return Err(FrameError::TooLarge(FRAME_PAYLOAD_BYTES));
        }
        let mut bytes = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
        bytes.extend_from_slice(&FRAME_MAGIC);
        bytes.push(FRAME_VERSION);
        bytes.push(self.kind);
        bytes.extend_from_slice(&self.flags.to_le_bytes());
        bytes.extend_from_slice(&self.capability_session_id.to_le_bytes());
        bytes.extend_from_slice(&self.flow_id.to_le_bytes());
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.fragment_index.to_le_bytes());
        bytes.extend_from_slice(&self.fragment_count.to_le_bytes());
        bytes.extend_from_slice(&self.presentation_time_us.to_le_bytes());
        bytes.extend_from_slice(&self.codec_config_id.to_le_bytes());
        bytes.extend_from_slice(payload);
        Ok(bytes)
    }

    pub fn decode(datagram: &[u8]) -> Result<(Self, &[u8]), FrameError> {
        if datagram.len() < FRAME_HEADER_BYTES {
            return Err(FrameError::Truncated);
        }
        if datagram[..4] != FRAME_MAGIC {
            return Err(FrameError::InvalidMagic);
        }
        if datagram[4] != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion(datagram[4]));
        }
        let kind = datagram[5];
        if !matches!(kind, FRAME_KIND_SCREEN | FRAME_KIND_CAMERA) {
            return Err(FrameError::InvalidKind(kind));
        }
        let flags = u16::from_le_bytes([datagram[6], datagram[7]]);
        let capability_session_id = u64::from_le_bytes(datagram[8..16].try_into().unwrap());
        let flow_id = u64::from_le_bytes(datagram[16..24].try_into().unwrap());
        let sequence = u64::from_le_bytes(datagram[24..32].try_into().unwrap());
        let fragment_index = u16::from_le_bytes([datagram[32], datagram[33]]);
        let fragment_count = u16::from_le_bytes([datagram[34], datagram[35]]);
        if fragment_count == 0 || fragment_index >= fragment_count {
            return Err(FrameError::InvalidFragments);
        }
        let presentation_time_us = u64::from_le_bytes(datagram[36..44].try_into().unwrap());
        let codec_config_id = u64::from_le_bytes(datagram[44..52].try_into().unwrap());
        Ok((
            Self {
                kind,
                flags,
                capability_session_id,
                flow_id,
                sequence,
                fragment_index,
                fragment_count,
                presentation_time_us,
                codec_config_id,
            },
            &datagram[FRAME_HEADER_BYTES..],
        ))
    }
}

/// Split one encoded access unit into bounded QUIC datagrams.
pub fn fragment_frame(
    kind: u8,
    capability_session_id: u64,
    flow_id: u64,
    sequence: u64,
    presentation_time_us: u64,
    codec_config_id: u64,
    frame: &[u8],
) -> Result<Vec<Vec<u8>>, FrameError> {
    fragment_frame_with_flags(
        kind,
        0,
        capability_session_id,
        flow_id,
        sequence,
        presentation_time_us,
        codec_config_id,
        frame,
    )
}

/// Split one encoded access unit into bounded QUIC datagrams, preserving
/// access-unit flags in every fragment. Flags are repeated deliberately: a
/// receiver can make its decode decision after reassembly without depending
/// on a particular fragment arriving first.
#[allow(clippy::too_many_arguments)]
pub fn fragment_frame_with_flags(
    kind: u8,
    flags: u16,
    capability_session_id: u64,
    flow_id: u64,
    sequence: u64,
    presentation_time_us: u64,
    codec_config_id: u64,
    frame: &[u8],
) -> Result<Vec<Vec<u8>>, FrameError> {
    if frame.is_empty() {
        return Err(FrameError::Truncated);
    }
    if frame.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(MAX_FRAME_BYTES));
    }
    let count = frame.len().div_ceil(FRAME_PAYLOAD_BYTES);
    if count > MAX_FRAME_FRAGMENTS {
        return Err(FrameError::TooLarge(
            MAX_FRAME_FRAGMENTS * FRAME_PAYLOAD_BYTES,
        ));
    }
    let count = count as u16;
    frame
        .chunks(FRAME_PAYLOAD_BYTES)
        .enumerate()
        .map(|(index, payload)| {
            FrameHeader {
                kind,
                flags,
                capability_session_id,
                flow_id,
                sequence,
                fragment_index: index as u16,
                fragment_count: count,
                presentation_time_us,
                codec_config_id,
            }
            .encode(payload)
        })
        .collect()
}

/// Prefix one ANFR packet for a reliable QUIC stream.
pub fn encode_stream_packet(packet: &[u8]) -> Result<Vec<u8>, FrameError> {
    if packet.is_empty() || packet.len() > FRAME_DATAGRAM_BYTES {
        return Err(FrameError::TooLarge(FRAME_DATAGRAM_BYTES));
    }
    let length =
        u32::try_from(packet.len()).map_err(|_| FrameError::TooLarge(u32::MAX as usize))?;
    let mut bytes = Vec::with_capacity(STREAM_PACKET_LENGTH_BYTES + packet.len());
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(packet);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip_preserves_all_fields() {
        let header = FrameHeader {
            kind: FRAME_KIND_SCREEN,
            flags: 3,
            capability_session_id: 7,
            flow_id: 11,
            sequence: 13,
            fragment_index: 1,
            fragment_count: 2,
            presentation_time_us: 17,
            codec_config_id: 19,
        };
        let encoded = header.encode(&[1, 2, 3]).unwrap();
        let (decoded, payload) = FrameHeader::decode(&encoded).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(payload, &[1, 2, 3]);
    }

    #[test]
    fn fragmentation_reassembles_in_order() {
        let frame = vec![42u8; FRAME_PAYLOAD_BYTES * 2 + 7];
        let packets = fragment_frame(FRAME_KIND_CAMERA, 1, 2, 3, 4, 5, &frame).unwrap();
        assert_eq!(packets.len(), 3);
        let mut rebuilt = Vec::new();
        for packet in packets {
            let (header, payload) = FrameHeader::decode(&packet).unwrap();
            assert_eq!(header.fragment_count, 3);
            rebuilt.extend_from_slice(payload);
        }
        assert_eq!(rebuilt, frame);
    }

    #[test]
    fn stream_packet_prefixes_existing_frame_record() {
        let packet = vec![7u8; FRAME_DATAGRAM_BYTES];
        let encoded = encode_stream_packet(&packet).unwrap();
        assert_eq!(
            u32::from_le_bytes(encoded[..4].try_into().unwrap()) as usize,
            packet.len()
        );
        assert_eq!(&encoded[4..], packet.as_slice());
    }

    #[test]
    fn flagged_fragmentation_repeats_flags_on_every_fragment() {
        let packets = fragment_frame_with_flags(
            FRAME_KIND_SCREEN,
            FLAG_KEYFRAME,
            1,
            2,
            3,
            4,
            5,
            &vec![7u8; FRAME_PAYLOAD_BYTES * 2 + 1],
        )
        .unwrap();
        assert!(
            packets
                .iter()
                .all(|packet| { FrameHeader::decode(packet).unwrap().0.flags == FLAG_KEYFRAME })
        );
    }

    #[test]
    fn malformed_header_is_rejected() {
        assert_eq!(FrameHeader::decode(b"ANFR"), Err(FrameError::Truncated));
        let mut packet = vec![0u8; FRAME_HEADER_BYTES];
        packet[..4].copy_from_slice(b"NOPE");
        assert_eq!(FrameHeader::decode(&packet), Err(FrameError::InvalidMagic));
    }
}
