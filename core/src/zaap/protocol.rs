//! Hand-written Thrift TBinaryProtocol (unframed). Writes strict headers; reads strict and non-strict.
//! Decoding is incremental: [`decode_message`] returns `Ok(None)` until a whole message is buffered.
use thiserror::Error;

pub mod ttype {
    pub const STOP: u8 = 0;
    pub const BOOL: u8 = 2;
    pub const BYTE: u8 = 3;
    pub const DOUBLE: u8 = 4;
    pub const I16: u8 = 6;
    pub const I32: u8 = 8;
    pub const I64: u8 = 10;
    pub const STRING: u8 = 11;
    pub const STRUCT: u8 = 12;
    pub const MAP: u8 = 13;
    pub const SET: u8 = 14;
    pub const LIST: u8 = 15;
}

const VERSION_1: u32 = 0x8001_0000;
const VERSION_MASK: u32 = 0xffff_0000;
/// Limits on untrusted input (the peer is any local process).
pub const MAX_STRING: usize = 16 * 1024 * 1024;
pub const MAX_CONTAINER: usize = 1 << 20;
pub const MAX_DEPTH: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("bad thrift protocol version {0:#x}")]
    BadVersion(u32),
    #[error("negative size")]
    NegativeSize,
    #[error("size limit exceeded")]
    SizeLimit,
    #[error("nesting too deep")]
    TooDeep,
    #[error("invalid type id {0}")]
    InvalidType(u8),
    #[error("invalid message type {0}")]
    InvalidMessageType(u8),
    #[error("invalid utf-8 in method name")]
    InvalidUtf8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Call = 1,
    Reply = 2,
    Exception = 3,
    Oneway = 4,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TValue {
    Bool(bool),
    Byte(i8),
    Double(f64),
    I16(i16),
    I32(i32),
    I64(i64),
    /// Thrift `string`/`binary`.
    Binary(Vec<u8>),
    Struct(TStruct),
    Map {
        key_type: u8,
        value_type: u8,
        entries: Vec<(TValue, TValue)>,
    },
    Set {
        elem_type: u8,
        items: Vec<TValue>,
    },
    List {
        elem_type: u8,
        items: Vec<TValue>,
    },
}

impl TValue {
    pub fn ttype(&self) -> u8 {
        match self {
            TValue::Bool(_) => ttype::BOOL,
            TValue::Byte(_) => ttype::BYTE,
            TValue::Double(_) => ttype::DOUBLE,
            TValue::I16(_) => ttype::I16,
            TValue::I32(_) => ttype::I32,
            TValue::I64(_) => ttype::I64,
            TValue::Binary(_) => ttype::STRING,
            TValue::Struct(_) => ttype::STRUCT,
            TValue::Map { .. } => ttype::MAP,
            TValue::Set { .. } => ttype::SET,
            TValue::List { .. } => ttype::LIST,
        }
    }
    pub fn string(s: impl Into<String>) -> Self {
        TValue::Binary(s.into().into_bytes())
    }
}

/// Struct as an ordered list of `(field id, value)`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TStruct {
    pub fields: Vec<(i16, TValue)>,
}

impl TStruct {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with(mut self, id: i16, v: TValue) -> Self {
        self.fields.push((id, v));
        self
    }
    pub fn get(&self, id: i16) -> Option<&TValue> {
        self.fields.iter().find(|(i, _)| *i == id).map(|(_, v)| v)
    }
    /// Like Node thrift, a field with an unexpected type reads as absent.
    pub fn get_str(&self, id: i16) -> Option<&str> {
        match self.get(id) {
            Some(TValue::Binary(b)) => std::str::from_utf8(b).ok(),
            _ => None,
        }
    }
    pub fn get_i32(&self, id: i16) -> Option<i32> {
        match self.get(id) {
            Some(TValue::I32(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn get_bool(&self, id: i16) -> Option<bool> {
        match self.get(id) {
            Some(TValue::Bool(v)) => Some(*v),
            _ => None,
        }
    }
    pub fn get_struct(&self, id: i16) -> Option<&TStruct> {
        match self.get(id) {
            Some(TValue::Struct(s)) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub name: String,
    pub kind: MessageType,
    pub seqid: i32,
    pub body: TStruct,
}

// ---------------------------------------------------------------- encoding

pub fn encode_message(msg: &Message, out: &mut Vec<u8>) {
    out.extend_from_slice(&(VERSION_1 | msg.kind as u32).to_be_bytes());
    write_bytes(out, msg.name.as_bytes());
    out.extend_from_slice(&msg.seqid.to_be_bytes());
    write_struct(out, &msg.body);
}

fn write_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as i32).to_be_bytes());
    out.extend_from_slice(b);
}

pub fn write_struct(out: &mut Vec<u8>, s: &TStruct) {
    for (id, v) in &s.fields {
        out.push(v.ttype());
        out.extend_from_slice(&id.to_be_bytes());
        write_value(out, v);
    }
    out.push(ttype::STOP);
}

fn write_value(out: &mut Vec<u8>, v: &TValue) {
    match v {
        TValue::Bool(b) => out.push(u8::from(*b)),
        TValue::Byte(b) => out.push(*b as u8),
        TValue::Double(d) => out.extend_from_slice(&d.to_bits().to_be_bytes()),
        TValue::I16(i) => out.extend_from_slice(&i.to_be_bytes()),
        TValue::I32(i) => out.extend_from_slice(&i.to_be_bytes()),
        TValue::I64(i) => out.extend_from_slice(&i.to_be_bytes()),
        TValue::Binary(b) => write_bytes(out, b),
        TValue::Struct(s) => write_struct(out, s),
        TValue::Map {
            key_type,
            value_type,
            entries,
        } => {
            out.push(*key_type);
            out.push(*value_type);
            out.extend_from_slice(&(entries.len() as i32).to_be_bytes());
            for (k, v) in entries {
                write_value(out, k);
                write_value(out, v);
            }
        }
        TValue::Set { elem_type, items } | TValue::List { elem_type, items } => {
            out.push(*elem_type);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for i in items {
                write_value(out, i);
            }
        }
    }
}

// ---------------------------------------------------------------- decoding

enum DecodeError {
    Incomplete,
    Bad(ProtocolError),
}

impl From<ProtocolError> for DecodeError {
    fn from(e: ProtocolError) -> Self {
        DecodeError::Bad(e)
    }
}

type R<T> = Result<T, DecodeError>;

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        if self.buf.len() - self.pos < n {
            return Err(DecodeError::Incomplete);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> R<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> R<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> R<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn size(&mut self, limit: usize) -> R<usize> {
        let n = self.i32()?;
        if n < 0 {
            return Err(ProtocolError::NegativeSize.into());
        }
        let n = n as usize;
        if n > limit {
            return Err(ProtocolError::SizeLimit.into());
        }
        Ok(n)
    }
    fn bytes(&mut self) -> R<Vec<u8>> {
        let n = self.size(MAX_STRING)?;
        Ok(self.take(n)?.to_vec())
    }
    fn structure(&mut self, depth: usize) -> R<TStruct> {
        if depth > MAX_DEPTH {
            return Err(ProtocolError::TooDeep.into());
        }
        let mut s = TStruct::new();
        loop {
            let t = self.u8()?;
            if t == ttype::STOP {
                return Ok(s);
            }
            let id = self.i16()?;
            let v = self.value(t, depth + 1)?;
            s.fields.push((id, v));
        }
    }
    /// Count of a container, with the allocation bounded by the bytes actually present.
    fn container(&mut self, min_elem: usize) -> R<(usize, usize)> {
        let n = self.size(MAX_CONTAINER)?;
        let remaining = self.buf.len() - self.pos;
        if n.saturating_mul(min_elem) > remaining {
            return Err(DecodeError::Incomplete);
        }
        Ok((n, n.min(remaining / min_elem.max(1))))
    }
    fn value(&mut self, t: u8, depth: usize) -> R<TValue> {
        if depth > MAX_DEPTH {
            return Err(ProtocolError::TooDeep.into());
        }
        Ok(match t {
            ttype::BOOL => TValue::Bool(self.u8()? != 0),
            ttype::BYTE => TValue::Byte(self.u8()? as i8),
            ttype::DOUBLE => TValue::Double(f64::from_bits(self.i64()? as u64)),
            ttype::I16 => TValue::I16(self.i16()?),
            ttype::I32 => TValue::I32(self.i32()?),
            ttype::I64 => TValue::I64(self.i64()?),
            ttype::STRING => TValue::Binary(self.bytes()?),
            ttype::STRUCT => TValue::Struct(self.structure(depth)?),
            ttype::MAP => {
                let key_type = self.u8()?;
                let value_type = self.u8()?;
                let (n, cap) = self.container(min_size(key_type)? + min_size(value_type)?)?;
                let mut entries = Vec::with_capacity(cap);
                for _ in 0..n {
                    let k = self.value(key_type, depth + 1)?;
                    let v = self.value(value_type, depth + 1)?;
                    entries.push((k, v));
                }
                TValue::Map {
                    key_type,
                    value_type,
                    entries,
                }
            }
            ttype::SET | ttype::LIST => {
                let elem_type = self.u8()?;
                let (n, cap) = self.container(min_size(elem_type)?)?;
                let mut items = Vec::with_capacity(cap);
                for _ in 0..n {
                    items.push(self.value(elem_type, depth + 1)?);
                }
                if t == ttype::SET {
                    TValue::Set { elem_type, items }
                } else {
                    TValue::List { elem_type, items }
                }
            }
            other => return Err(ProtocolError::InvalidType(other).into()),
        })
    }
}

/// Minimum encoded size of one value of type `t`.
fn min_size(t: u8) -> Result<usize, ProtocolError> {
    Ok(match t {
        ttype::BOOL | ttype::BYTE | ttype::STRUCT => 1,
        ttype::I16 => 2,
        ttype::I32 | ttype::STRING | ttype::SET | ttype::LIST => 4,
        ttype::DOUBLE | ttype::I64 => 8,
        ttype::MAP => 6,
        other => return Err(ProtocolError::InvalidType(other)),
    })
}

fn message_type(b: u8) -> Result<MessageType, ProtocolError> {
    Ok(match b {
        1 => MessageType::Call,
        2 => MessageType::Reply,
        3 => MessageType::Exception,
        4 => MessageType::Oneway,
        other => return Err(ProtocolError::InvalidMessageType(other)),
    })
}

/// Decode one message from the front of `buf`. `Ok(None)`: need more bytes. On success returns the
/// message and the number of bytes consumed.
pub fn decode_message(buf: &[u8]) -> Result<Option<(Message, usize)>, ProtocolError> {
    let mut r = Reader { buf, pos: 0 };
    let res = (|| -> R<Message> {
        let first = r.i32()?;
        let (name, kind) = if first < 0 {
            let v = first as u32;
            if v & VERSION_MASK != VERSION_1 {
                return Err(ProtocolError::BadVersion(v).into());
            }
            let name = r.bytes()?;
            (name, message_type((v & 0xff) as u8)?)
        } else {
            // Non-strict: name length, name, type byte.
            let n = first as usize;
            if n > MAX_STRING {
                return Err(ProtocolError::SizeLimit.into());
            }
            let name = r.take(n)?.to_vec();
            (name, message_type(r.u8()?)?)
        };
        let seqid = r.i32()?;
        let body = r.structure(0)?;
        let name = String::from_utf8(name).map_err(|_| ProtocolError::InvalidUtf8)?;
        Ok(Message {
            name,
            kind,
            seqid,
            body,
        })
    })();
    match res {
        Ok(m) => Ok(Some((m, r.pos))),
        Err(DecodeError::Incomplete) => Ok(None),
        Err(DecodeError::Bad(e)) => Err(e),
    }
}

// ---------------------------------------------------------------- TApplicationException

pub mod app_exception {
    pub const UNKNOWN: i32 = 0;
    pub const UNKNOWN_METHOD: i32 = 1;
    pub const INVALID_MESSAGE_TYPE: i32 = 2;
    pub const PROTOCOL_ERROR: i32 = 7;
}

/// `TApplicationException { 1: string message, 2: i32 type }`.
pub fn application_exception(kind: i32, message: &str) -> TStruct {
    TStruct::new()
        .with(1, TValue::string(message))
        .with(2, TValue::I32(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Message {
        let nested = TStruct::new()
            .with(1, TValue::I32(25))
            .with(-3, TValue::I16(-7));
        Message {
            name: "connect".into(),
            kind: MessageType::Call,
            seqid: 42,
            body: TStruct::new()
                .with(1, TValue::string("dofus"))
                .with(2, TValue::Bool(true))
                .with(3, TValue::Byte(-1))
                .with(4, TValue::Double(1.5))
                .with(5, TValue::I64(-1 << 40))
                .with(6, TValue::Struct(nested))
                .with(
                    7,
                    TValue::List {
                        elem_type: ttype::STRING,
                        items: vec![TValue::string("a"), TValue::string("")],
                    },
                )
                .with(
                    8,
                    TValue::Set {
                        elem_type: ttype::I32,
                        items: vec![TValue::I32(1)],
                    },
                )
                .with(
                    9,
                    TValue::Map {
                        key_type: ttype::STRING,
                        value_type: ttype::LIST,
                        entries: vec![(
                            TValue::string("k"),
                            TValue::List {
                                elem_type: ttype::BOOL,
                                items: vec![],
                            },
                        )],
                    },
                ),
        }
    }

    #[test]
    fn roundtrip_all_types() {
        let m = sample();
        let mut buf = vec![];
        encode_message(&m, &mut buf);
        assert_eq!(&buf[..4], &[0x80, 0x01, 0x00, 0x01]);
        let (d, used) = decode_message(&buf).unwrap().unwrap();
        assert_eq!(used, buf.len());
        assert_eq!(d, m);
    }

    #[test]
    fn incomplete_at_every_split_and_multiple_messages() {
        let mut buf = vec![];
        encode_message(&sample(), &mut buf);
        let one = buf.len();
        for cut in 0..one {
            assert_eq!(decode_message(&buf[..cut]).unwrap(), None, "cut {cut}");
        }
        encode_message(&sample(), &mut buf);
        let (_, used) = decode_message(&buf).unwrap().unwrap();
        assert_eq!(used, one);
        let (m2, used2) = decode_message(&buf[used..]).unwrap().unwrap();
        assert_eq!((m2.seqid, used2), (42, one));
    }

    #[test]
    fn non_strict_header() {
        let mut buf = vec![];
        buf.extend_from_slice(&7i32.to_be_bytes());
        buf.extend_from_slice(b"connect");
        buf.push(1);
        buf.extend_from_slice(&9i32.to_be_bytes());
        buf.extend_from_slice(&[ttype::I32, 0, 3, 0, 0, 0, 5, ttype::STOP]);
        let (m, _) = decode_message(&buf).unwrap().unwrap();
        assert_eq!(
            (m.name.as_str(), m.kind, m.seqid, m.body.get_i32(3)),
            ("connect", MessageType::Call, 9, Some(5))
        );
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(
            decode_message(&[0x80, 0x02, 0, 1]),
            Err(ProtocolError::BadVersion(0x8002_0001))
        );
        let mut buf = vec![0x80, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        buf.extend_from_slice(&[ttype::STRING, 0, 1, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(decode_message(&buf), Err(ProtocolError::NegativeSize));
        let mut buf = vec![0x80, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        buf.extend_from_slice(&[99, 0, 1]);
        assert_eq!(decode_message(&buf), Err(ProtocolError::InvalidType(99)));
        // Huge list count without the bytes: incomplete (bounded), not an allocation.
        let mut buf = vec![0x80, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        buf.extend_from_slice(&[ttype::LIST, 0, 1, ttype::I64, 0, 0x0f, 0xff, 0xff]);
        assert_eq!(decode_message(&buf), Ok(None));
        // Deep nesting.
        let mut buf = vec![0x80, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        for _ in 0..(MAX_DEPTH + 2) {
            buf.extend_from_slice(&[ttype::STRUCT, 0, 1]);
        }
        assert_eq!(decode_message(&buf), Err(ProtocolError::TooDeep));
    }
}
