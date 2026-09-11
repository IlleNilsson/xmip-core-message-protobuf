//! The protobuf wire format, as far as a shape reads it: a message is a
//! run of fields, each a varint tag — the field number and the wire type —
//! followed by as many bytes as the wire type says; a length-delimited
//! stream is messages each behind a varint length. The walk finds where
//! every top-level field's value lies and what wire type carries it; no
//! value is decoded, because without a schema there is nothing to decode
//! it into, and a shape only sections.

use std::ops::Range;

/// Where the walk stopped and why.
pub type Stop = (&'static str, usize);

/// The largest field number the format allows.
const LARGEST_FIELD: u64 = (1 << 29) - 1;

/// Groups nested deeper than this are refused rather than the stack.
const DEEPEST: usize = 64;

/// How a field's value is carried, named as the specification names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireType {
    /// A varint: `int32`, `int64`, `uint`, `sint`, `bool`, an enum.
    Varint,
    /// Eight bytes: `fixed64`, `sfixed64`, `double`.
    I64,
    /// A length then that many bytes: a string, bytes, an embedded message,
    /// a packed repeated field.
    Len,
    /// A start-group tag to its end-group tag; proto2's legacy nesting.
    Group,
    /// Four bytes: `fixed32`, `sfixed32`, `float`.
    I32,
}

impl WireType {
    /// The media type suffix a part of this wire type carries.
    #[must_use]
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Varint => "varint",
            Self::I64 => "i64",
            Self::Len => "len",
            Self::Group => "group",
            Self::I32 => "i32",
        }
    }
}

/// One top-level field: its number, its wire type and where its value lies
/// — the varint's bytes, the eight or four bytes, the bytes behind a
/// length, or the bytes between a group's two tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub number: u32,
    pub wire: WireType,
    pub value: Range<usize>,
}

/// The varint at `at`: its value and the byte after it.
///
/// # Errors
/// The bytes end inside the varint, or it runs past ten bytes.
pub fn varint(bytes: &[u8], at: usize) -> Result<(u64, usize), Stop> {
    let mut value: u64 = 0;
    for (index, byte) in bytes.get(at..).unwrap_or(&[]).iter().enumerate() {
        if index >= 10 {
            return Err(("a varint runs past ten bytes", at));
        }
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok((value, at + index + 1));
        }
    }
    Err(("the message ends inside a varint", bytes.len()))
}

/// The top-level fields of the message that is `bytes[range]`.
///
/// # Errors
/// A field number outside the format's range, a wire type the format does
/// not have, a group that never ends or ends without starting, or a message
/// that ends inside a field.
pub fn fields(bytes: &[u8], range: Range<usize>) -> Result<Vec<Field>, Stop> {
    let (fields, _, _) = walk(&bytes[..range.end], range.start, None, 0)?;
    Ok(fields)
}

/// The messages of a length-delimited stream: each behind its varint
/// length, the last ending exactly where the bytes do.
///
/// # Errors
/// The stream ends inside a message's length or inside the message.
pub fn delimited(bytes: &[u8]) -> Result<Vec<Range<usize>>, Stop> {
    let mut at = 0;
    let mut messages = Vec::new();
    while at < bytes.len() {
        let (length, start) = varint(bytes, at)?;
        let end = usize::try_from(length)
            .ok()
            .and_then(|length| start.checked_add(length))
            .filter(|end| *end <= bytes.len())
            .ok_or(("the stream ends inside a message", bytes.len()))?;
        messages.push(start..end);
        at = end;
    }
    Ok(messages)
}

/// The fields from `at` to the end of `bytes` or, inside `group`, to that
/// group's end tag. The byte the fields end at and the byte after that —
/// which differ only by the end tag of a group.
fn walk(
    bytes: &[u8],
    at: usize,
    group: Option<u32>,
    depth: usize,
) -> Result<(Vec<Field>, usize, usize), Stop> {
    let end = bytes.len();
    let mut fields = Vec::new();
    let mut at = at;
    while at < end {
        let tag_at = at;
        let (tag, next) = varint(bytes, at)?;
        let number = match tag >> 3 {
            n @ 1..=LARGEST_FIELD => {
                u32::try_from(n).map_err(|_| ("a field number too large", tag_at))?
            }
            _ => return Err(("a field number outside the format's range", tag_at)),
        };
        at = next;
        let (wire, value) = match tag & 7 {
            0 => (WireType::Varint, at..varint(bytes, at)?.1),
            1 => (WireType::I64, at..at + 8),
            2 => {
                let (length, start) = varint(bytes, at)?;
                let length = usize::try_from(length).map_err(|_| ("a length too large", at))?;
                (WireType::Len, start..start.saturating_add(length))
            }
            3 => {
                if depth >= DEEPEST {
                    return Err(("groups nested too deep", tag_at));
                }
                let (_, close, after) = walk(bytes, at, Some(number), depth + 1)?;
                let field = Field {
                    number,
                    wire: WireType::Group,
                    value: at..close,
                };
                fields.push(field);
                at = after;
                continue;
            }
            4 if group == Some(number) => return Ok((fields, tag_at, at)),
            4 => return Err(("an end group without its start", tag_at)),
            5 => (WireType::I32, at..at + 4),
            _ => return Err(("a wire type the format does not have", tag_at)),
        };
        if value.end > end {
            return Err(("the message ends inside a field", end));
        }
        at = value.end;
        fields.push(Field {
            number,
            wire,
            value,
        });
    }
    if group.is_some() {
        return Err(("a group never ends", end));
    }
    Ok((fields, end, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_varint_is_base_128_little_endian_and_stops_where_it_cannot_end() {
        assert_eq!(varint(&[0x96, 0x01], 0), Ok((150, 2)));
        assert_eq!(varint(&[0x00], 0), Ok((0, 1)));
        let largest = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01];
        assert_eq!(varint(&largest, 0), Ok((u64::MAX, 10)));
        assert_eq!(
            varint(&[0x80], 0),
            Err(("the message ends inside a varint", 1))
        );
        assert_eq!(
            varint(&[0x80; 11], 0),
            Err(("a varint runs past ten bytes", 0))
        );
    }

    #[test]
    fn a_group_is_walked_to_its_end_tag_and_nesting_is_bounded() {
        let bytes = b"\x0b\x08\x01\x13\x10\x02\x14\x0c\x18\x03";
        let fields = fields(bytes, 0..bytes.len()).expect("walks");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].wire, WireType::Group);
        assert_eq!(fields[0].value, 1..7);
        assert_eq!(fields[1].number, 3);
        assert_eq!(fields[1].value, 9..10);

        let deep: Vec<u8> = std::iter::repeat_n(0x0b, DEEPEST + 1).collect();
        assert_eq!(
            super::fields(&deep, 0..deep.len()),
            Err(("groups nested too deep", DEEPEST))
        );
        assert_eq!(
            super::fields(b"\x0b\x08\x01", 0..3),
            Err(("a group never ends", 3))
        );
        assert_eq!(
            super::fields(b"\x0b\x14", 0..2),
            Err(("an end group without its start", 1))
        );
    }
}
