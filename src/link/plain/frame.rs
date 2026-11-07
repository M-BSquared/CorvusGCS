//! MAVLink 1 and 2 frames: encoding with CRC_EXTRA, the stream parser, and
//! MAVLink 2 signing.
//!
//! ```text
//! v1: FE len seq sys comp msgid                 payload crc16
//! v2: FD len incompat compat seq sys comp msgid(3) payload crc16 [link_id ts(6) sig(6)]
//! ```
//! The CRC is CRC-16/MCRF4XX over everything after the magic byte plus the
//! message's CRC_EXTRA, little endian. A signed frame carries 13 more bytes
//! (incompat flag bit 0): the link id, a 48 bit timestamp and the first six
//! bytes of SHA-256 over the secret key, the frame up to its CRC, the link id
//! and the timestamp.

use sha2::{Digest, Sha256};

use super::codec::{pack_payload, unpack_payload, CodecError};
use super::crc::accumulate;
use super::message::Message;
use crate::wire::defs::{message_by_id, Wire};

pub const MAGIC_V1: u8 = 0xFE;
pub const MAGIC_V2: u8 = 0xFD;
pub const FLAG_SIGNED: u8 = 0x01;
pub const SIGNATURE_LEN: usize = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    V1,
    V2,
}

impl Proto {
    pub fn wire(self) -> Wire {
        match self {
            Proto::V1 => Wire::V1,
            Proto::V2 => Wire::V2,
        }
    }
}

/// The key and counters a link signs and checks frames with.
#[derive(Debug, Clone)]
pub struct Signing {
    pub secret_key: [u8; 32],
    pub link_id: u8,
    /// The next timestamp to use, in 10 microsecond units since 2015-01-01.
    pub timestamp: u64,
}

impl Signing {
    pub fn new(secret_key: [u8; 32], link_id: u8, timestamp: u64) -> Signing {
        Signing {
            secret_key,
            link_id,
            timestamp,
        }
    }

    fn signature(&self, signed_part: &[u8], link_id: u8, ts: &[u8]) -> [u8; 6] {
        let mut h = Sha256::new();
        h.update(self.secret_key);
        h.update(signed_part);
        h.update([link_id]);
        h.update(ts);
        let d = h.finalize();
        [d[0], d[1], d[2], d[3], d[4], d[5]]
    }

    /// Appends link id, timestamp and signature to a MAVLink 2 frame that has
    /// its signed flag set, and advances the timestamp.
    fn sign(&mut self, frame: &mut Vec<u8>) {
        let ts = &self.timestamp.to_le_bytes()[..6];
        let sig = self.signature(frame, self.link_id, ts);
        frame.push(self.link_id);
        frame.extend_from_slice(ts);
        frame.extend_from_slice(&sig);
        self.timestamp += 1;
    }

    /// Whether `frame` carries this key's signature.
    pub fn verify(&self, frame: &Frame) -> bool {
        let Some(sig) = frame.signature else { return false };
        let body = &frame.raw[..frame.raw.len() - SIGNATURE_LEN];
        self.signature(body, sig[0], &sig[1..7]) == sig[7..13]
    }
}

/// The bytes of `msg` as a frame. MAVLink 2 trims the payload's trailing
/// zeros and signs when `signing` is given; MAVLink 1 sends the base fields
/// and cannot be signed.
pub fn encode(
    proto: Proto,
    seq: u8,
    sysid: u8,
    compid: u8,
    msg: &Message,
    signing: Option<&mut Signing>,
) -> Result<Vec<u8>, CodecError> {
    let payload = pack_payload(msg, proto == Proto::V2)?;
    let id = msg.id();
    let mut out = Vec::with_capacity(payload.len() + 25);
    match proto {
        Proto::V1 => out.extend_from_slice(&[MAGIC_V1, payload.len() as u8, seq, sysid, compid, id as u8]),
        Proto::V2 => {
            let flags = if signing.is_some() { FLAG_SIGNED } else { 0 };
            out.extend_from_slice(&[MAGIC_V2, payload.len() as u8, flags, 0, seq, sysid, compid]);
            out.extend_from_slice(&id.to_le_bytes()[..3]);
        }
    }
    out.extend_from_slice(&payload);
    let crc = accumulate(accumulate(0xFFFF, &out[1..]), &[msg.def().crc_extra]);
    out.extend_from_slice(&crc.to_le_bytes());
    if let (Proto::V2, Some(s)) = (proto, signing) {
        s.sign(&mut out);
    }
    Ok(out)
}

/// One frame as received, its checksum verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub proto: Proto,
    pub incompat_flags: u8,
    pub compat_flags: u8,
    pub seq: u8,
    pub sysid: u8,
    pub compid: u8,
    pub msgid: u32,
    /// The payload as sent (a MAVLink 2 one may have lost trailing zeros).
    pub payload: Vec<u8>,
    pub crc: u16,
    /// Link id, timestamp (6 bytes) and signature (6 bytes) of a signed frame.
    pub signature: Option<[u8; SIGNATURE_LEN]>,
    /// Every byte of the frame, for forwarding and for checking a signature.
    pub raw: Vec<u8>,
}

impl Frame {
    pub fn message(&self) -> Result<Message, CodecError> {
        unpack_payload(self.msgid, self.proto.wire(), &self.payload)
    }

    /// The signature's timestamp.
    pub fn timestamp(&self) -> Option<u64> {
        self.signature.map(|s| {
            let mut b = [0u8; 8];
            b[..6].copy_from_slice(&s[1..7]);
            u64::from_le_bytes(b)
        })
    }
}

/// What the parser found in the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Frame(Frame),
    /// Bytes skipped looking for the next frame start.
    Garbage(usize),
    /// A frame whose checksum is wrong; its first byte was dropped to resync.
    BadCrc {
        msgid: u32,
    },
    /// A frame of a message this dialect does not have, or with flags it does not support.
    Unknown {
        msgid: u32,
    },
}

/// A stream parser: feed it whatever a link delivered, in any pieces.
#[derive(Debug, Default)]
pub struct Parser {
    buf: Vec<u8>,
}

impl Parser {
    pub fn new() -> Parser {
        Parser::default()
    }

    /// Bytes held back for a frame that is not complete yet.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Event> {
        self.buf.extend_from_slice(data);
        let mut events = Vec::new();
        let mut start = 0;
        loop {
            let rest = &self.buf[start..];
            let Some(skip) = rest.iter().position(|&b| b == MAGIC_V1 || b == MAGIC_V2) else {
                if !rest.is_empty() {
                    events.push(Event::Garbage(rest.len()));
                }
                start = self.buf.len();
                break;
            };
            if skip > 0 {
                events.push(Event::Garbage(skip));
                start += skip;
            }
            match parse_one(&self.buf[start..]) {
                Step::Need => break,
                Step::Frame(frame) => {
                    start += frame.raw.len();
                    events.push(Event::Frame(frame));
                }
                Step::Drop(event) => {
                    start += 1;
                    events.push(event);
                }
            }
        }
        self.buf.drain(..start);
        events
    }
}

enum Step {
    Need,
    Frame(Frame),
    Drop(Event),
}

fn parse_one(b: &[u8]) -> Step {
    let v2 = b[0] == MAGIC_V2;
    let header = if v2 { 10 } else { 6 };
    if b.len() < header {
        return Step::Need;
    }
    let len = b[1] as usize;
    let incompat = if v2 { b[2] } else { 0 };
    let msgid = if v2 {
        u32::from_le_bytes([b[7], b[8], b[9], 0])
    } else {
        b[5] as u32
    };
    if incompat & !FLAG_SIGNED != 0 {
        return Step::Drop(Event::Unknown { msgid });
    }
    let signed = incompat & FLAG_SIGNED != 0;
    let total = header + len + 2 + if signed { SIGNATURE_LEN } else { 0 };
    if b.len() < total {
        return Step::Need;
    }
    let wire = if v2 { Wire::V2 } else { Wire::V1 };
    let Some(def) = message_by_id(msgid, wire) else {
        return Step::Drop(Event::Unknown { msgid });
    };
    let body_end = header + len;
    let want = u16::from_le_bytes([b[body_end], b[body_end + 1]]);
    let got = accumulate(accumulate(0xFFFF, &b[1..body_end]), &[def.crc_extra]);
    if got != want {
        return Step::Drop(Event::BadCrc { msgid });
    }
    let mut signature = None;
    if signed {
        let mut s = [0u8; SIGNATURE_LEN];
        s.copy_from_slice(&b[body_end + 2..total]);
        signature = Some(s);
    }
    Step::Frame(Frame {
        proto: if v2 { Proto::V2 } else { Proto::V1 },
        incompat_flags: incompat,
        compat_flags: if v2 { b[3] } else { 0 },
        seq: if v2 { b[4] } else { b[2] },
        sysid: if v2 { b[5] } else { b[3] },
        compid: if v2 { b[6] } else { b[4] },
        msgid,
        payload: b[header..body_end].to_vec(),
        crc: want,
        signature,
        raw: b[..total].to_vec(),
    })
}
