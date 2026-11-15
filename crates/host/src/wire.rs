//! Length-prefixed norn.bin v1. Only the types used by the UI protocol
//! cross this pipe. The desktop host has no dependency on the Norn VM.

use norn_ui_engine::Data;
use num_bigint::{BigInt, Sign};
use num_rational::BigRational;
use num_traits::ToPrimitive;
use std::io::{self, Read, Write};

const HEADER: &[u8] = b"NRNB\x01";
const MAX_FRAME: usize = 128 * 1024 * 1024;
const MAX_DEPTH: usize = 512;

const NIL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const INT: u8 = 3;
const BIG: u8 = 4;
const FLOAT: u8 = 5;
const RATIO: u8 = 6;
const CHAR: u8 = 7;
const STRING: u8 = 8;
const KEYWORD: u8 = 9;
const SYMBOL: u8 = 10;
const LIST: u8 = 11;
const VECTOR: u8 = 12;
const MAP: u8 = 13;
const SET: u8 = 14;
const SORTED_MAP: u8 = 15;
const SORTED_SET: u8 = 16;
const QUEUE: u8 = 17;
const BYTES: u8 = 18;
const REF: u8 = 21;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0; 4];
    if input.read(&mut length[..1])? == 0 {
        return Ok(None);
    }
    input.read_exact(&mut length[1..])?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(invalid("UI message exceeds the frame limit"));
    }
    let mut frame = vec![0; length];
    input.read_exact(&mut frame)?;
    Ok(Some(frame))
}

pub fn decode(frame: &[u8]) -> io::Result<Data> {
    let bytes = frame
        .strip_prefix(HEADER)
        .ok_or_else(|| invalid("expected norn.bin version 1"))?;
    let mut decoder = Decoder {
        bytes,
        names: Vec::new(),
    };
    let data = decoder.value(0)?;
    if !decoder.bytes.is_empty() {
        return Err(invalid("extra data after UI message"));
    }
    Ok(data)
}

struct Name {
    namespace: Option<String>,
    name: String,
}

struct Decoder<'a> {
    bytes: &'a [u8],
    names: Vec<Data>,
}

impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let (head, tail) = self
            .bytes
            .split_at_checked(n)
            .ok_or_else(|| invalid("truncated UI message"))?;
        self.bytes = tail;
        Ok(head)
    }

    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> io::Result<u64> {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            if shift == 63 && b > 1 {
                return Err(invalid("invalid varint"));
            }
            value |= u64::from(b & 127) << shift;
            if b < 128 {
                return Ok(value);
            }
        }
        Err(invalid("invalid varint"))
    }

    fn length(&mut self, item_size: usize) -> io::Result<usize> {
        let count = self.varint()?;
        if count > (self.bytes.len() / item_size) as u64 {
            return Err(invalid("invalid UI collection length"));
        }
        Ok(count as usize)
    }

    fn blob(&mut self) -> io::Result<&'a [u8]> {
        let len = self.length(1)?;
        self.take(len)
    }

    fn utf8(bytes: &[u8]) -> io::Result<String> {
        String::from_utf8(bytes.to_vec()).map_err(|_| invalid("invalid UTF-8"))
    }

    fn integer(&mut self, tag: u8) -> io::Result<BigInt> {
        match tag {
            INT => Ok(self.zigzag()?.into()),
            BIG => {
                let sign = match self.byte()? {
                    0 => Sign::Plus,
                    1 => Sign::Minus,
                    _ => return Err(invalid("invalid integer sign")),
                };
                Ok(BigInt::from_bytes_le(sign, self.blob()?))
            }
            _ => Err(invalid("expected an integer")),
        }
    }

    fn zigzag(&mut self) -> io::Result<i64> {
        let n = self.varint()?;
        Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
    }

    fn name(&mut self) -> io::Result<Name> {
        let len = self.varint()?;
        let namespace = if len == 0 {
            None
        } else {
            let len = usize::try_from(len - 1).map_err(|_| invalid("invalid namespace length"))?;
            Some(Self::utf8(self.take(len)?)?)
        };
        Ok(Name {
            namespace,
            name: Self::utf8(self.blob()?)?,
        })
    }

    fn value(&mut self, depth: usize) -> io::Result<Data> {
        if depth >= MAX_DEPTH {
            return Err(invalid("UI message is nested too deeply"));
        }
        let tag = self.byte()?;
        Ok(match tag {
            NIL => Data::Nil,
            FALSE => Data::Bool(false),
            TRUE => Data::Bool(true),
            INT => Data::Num(self.zigzag()? as f64),
            BIG => Data::Num(self.integer(tag)?.to_f64().unwrap_or(f64::NAN)),
            FLOAT => Data::Num(f64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            RATIO => {
                let numerator_tag = self.byte()?;
                let numerator = self.integer(numerator_tag)?;
                let denominator_tag = self.byte()?;
                let denominator = self.integer(denominator_tag)?;
                if denominator.sign() != Sign::Plus {
                    return Err(invalid("invalid ratio denominator"));
                }
                Data::Num(
                    BigRational::new(numerator, denominator)
                        .to_f64()
                        .unwrap_or(f64::NAN),
                )
            }
            CHAR => {
                let ch = u32::try_from(self.varint()?).ok().and_then(char::from_u32);
                Data::Str(ch.ok_or_else(|| invalid("invalid character"))?.to_string())
            }
            STRING => Data::Str(Self::utf8(self.blob()?)?),
            KEYWORD | SYMBOL => {
                let Name { namespace, name } = self.name()?;
                let value = if tag == KEYWORD {
                    Data::Key(match namespace {
                        Some(ns) => format!("{ns}/{name}"),
                        None => name,
                    })
                } else {
                    Data::Str(name)
                };
                self.names.push(value.clone());
                value
            }
            REF => {
                let index = self.varint()?;
                self.names
                    .get(index as usize)
                    .cloned()
                    .ok_or_else(|| invalid("invalid name reference"))?
            }
            LIST | VECTOR | SET | SORTED_SET | QUEUE => {
                let count = self.length(1)?;
                let items = (0..count)
                    .map(|_| self.value(depth + 1))
                    .collect::<io::Result<_>>()?;
                match tag {
                    SET | SORTED_SET => Data::Set(items),
                    _ => Data::List(items),
                }
            }
            MAP | SORTED_MAP => {
                let count = self.length(2)?;
                let pairs = (0..count)
                    .map(|_| Ok((self.value(depth + 1)?, self.value(depth + 1)?)))
                    .collect::<io::Result<_>>()?;
                Data::Map(pairs)
            }
            BYTES => Data::Bytes(self.blob()?.to_vec().into()),
            _ => return Err(invalid("unsupported type in UI message")),
        })
    }
}

pub fn send(output: &mut impl Write, data: &Data) -> io::Result<()> {
    let mut bytes = HEADER.to_vec();
    encode(&mut bytes, data);
    if bytes.len() > MAX_FRAME {
        return Err(invalid("UI message exceeds the frame limit"));
    }
    output.write_all(&(bytes.len() as u32).to_le_bytes())?;
    output.write_all(&bytes)?;
    output.flush()
}

fn varint(bytes: &mut Vec<u8>, mut n: u64) {
    while n >= 128 {
        bytes.push(n as u8 | 128);
        n >>= 7;
    }
    bytes.push(n as u8);
}

fn counted(bytes: &mut Vec<u8>, tag: u8, count: usize) {
    bytes.push(tag);
    varint(bytes, count as u64);
}

fn blob(bytes: &mut Vec<u8>, tag: u8, value: &[u8]) {
    counted(bytes, tag, value.len());
    bytes.extend_from_slice(value);
}

fn encode(bytes: &mut Vec<u8>, value: &Data) {
    match value {
        Data::Nil => bytes.push(NIL),
        Data::Bool(false) => bytes.push(FALSE),
        Data::Bool(true) => bytes.push(TRUE),
        Data::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => {
            let n = *n as i64;
            bytes.push(INT);
            varint(bytes, ((n << 1) ^ (n >> 63)) as u64);
        }
        Data::Num(n) => {
            bytes.push(FLOAT);
            bytes.extend_from_slice(&n.to_le_bytes());
        }
        Data::Str(s) => blob(bytes, STRING, s.as_bytes()),
        Data::Key(s) => {
            bytes.push(KEYWORD);
            let name = if let Some((ns, name)) = s
                .split_once('/')
                .filter(|(ns, name)| !ns.is_empty() && !name.is_empty())
            {
                varint(bytes, ns.len() as u64 + 1);
                bytes.extend_from_slice(ns.as_bytes());
                name
            } else {
                varint(bytes, 0);
                s
            };
            varint(bytes, name.len() as u64);
            bytes.extend_from_slice(name.as_bytes());
        }
        Data::List(items) | Data::Set(items) => {
            let tag = if matches!(value, Data::List(_)) {
                VECTOR
            } else {
                SET
            };
            counted(bytes, tag, items.len());
            for item in items {
                encode(bytes, item);
            }
        }
        Data::Map(pairs) => {
            counted(bytes, MAP, pairs.len());
            for (key, value) in pairs {
                encode(bytes, key);
                encode(bytes, value);
            }
        }
        Data::Bytes(data) => blob(bytes, BYTES, data),
    }
}
