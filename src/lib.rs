#![forbid(unsafe_code)]

//! Protobuf: wire-format bytes without their schema. A shape built as a
//! message walks the top-level fields and hands back one part per field,
//! named by the field number, with the field's value bytes and the wire
//! type as the media type's suffix — `application/protobuf+varint`,
//! `+i64`, `+len`, `+i32`, `+group`; a repeated field is repeated parts of
//! one name, in order. A shape built as delimited sections a
//! length-delimited stream, one part per message named by its number, each
//! message walked to be sure it is one. No type is announced unless the
//! message — or the first message of the stream — opens with an
//! `Any`-shaped first field: field 1 a length-delimited `type_url` of the
//! form `<prefix>/<full.Name>`, then field 2 the value; the announced type
//! is the name after the last `/` (ADR-0047).
//!
//! The two framings are a choice the caller makes, because the format
//! cannot tell them apart: a length prefix reads as a tag and often as a
//! plausible one. Nothing is decoded — a varint is handed back as its
//! bytes, a `len` field as its bytes — because without the schema nothing
//! says what a field is; a contract with the schema does. What cannot be
//! sectioned — a field number outside the range, a wire type the format
//! does not have, a message that ends inside a field, a stream that ends
//! inside a message — is refused with the byte where the walk stopped.

mod wire;

pub use wire::{Field, WireType};

use message::{Part, Shape, ShapeError, Shaped};
use std::ops::Range;
use stream::Stream;

/// How the bytes frame their messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Framing {
    /// The bytes are one message; a part per top-level field.
    #[default]
    Message,
    /// The bytes are messages each behind a varint length; a part per
    /// message.
    Delimited,
}

/// The protobuf shape.
#[derive(Clone, Copy, Debug, Default)]
pub struct Protobuf {
    framing: Framing,
}

impl Protobuf {
    /// The bytes are one message.
    #[must_use]
    pub const fn message() -> Self {
        Self {
            framing: Framing::Message,
        }
    }

    /// The bytes are a length-delimited stream of messages.
    #[must_use]
    pub const fn delimited() -> Self {
        Self {
            framing: Framing::Delimited,
        }
    }

    #[must_use]
    pub const fn framing(self) -> Framing {
        self.framing
    }
}

/// The name a `type_url` in an `Any`-shaped first field announces.
fn announced(bytes: &[u8], fields: &[Field]) -> Option<String> {
    let first = fields.first()?;
    if first.number != 1 || first.wire != WireType::Len {
        return None;
    }
    let second_is_value = fields
        .get(1)
        .is_none_or(|second| second.number == 2 && second.wire == WireType::Len);
    if !second_is_value {
        return None;
    }
    let url = std::str::from_utf8(&bytes[first.value.clone()]).ok()?;
    let (prefix, name) = url.rsplit_once('/')?;
    let sound = !prefix.is_empty()
        && !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    sound.then(|| name.to_string())
}

impl Shape for Protobuf {
    fn technology(&self) -> &'static str {
        "protobuf"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/protobuf",
            "application/x-protobuf",
            "application/vnd.google.protobuf",
        ]
    }

    fn recognises(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        match self.framing {
            Framing::Message => wire::fields(bytes, 0..bytes.len()).is_ok(),
            Framing::Delimited => wire::delimited(bytes).is_ok_and(|messages| {
                messages
                    .into_iter()
                    .all(|message| wire::fields(bytes, message).is_ok())
            }),
        }
    }

    fn shape(&self, stream: &Stream) -> Result<Shaped, ShapeError> {
        let bytes = stream.bytes();
        let refused = |stop| ShapeError::refused("protobuf", stop);
        if bytes.is_empty() {
            return Err(refused(("no bytes", 0)));
        }
        match self.framing {
            Framing::Message => {
                let fields = wire::fields(bytes, 0..bytes.len()).map_err(refused)?;
                let message_type = announced(bytes, &fields);
                let parts = fields
                    .into_iter()
                    .map(|field| {
                        let media = format!("application/protobuf+{}", field.wire.suffix());
                        Part::new(
                            Some(field.number.to_string()),
                            &bytes[field.value],
                            Some(media),
                        )
                    })
                    .collect();
                Ok(Shaped {
                    parts,
                    message_type,
                })
            }
            Framing::Delimited => {
                let messages = wire::delimited(bytes).map_err(refused)?;
                let mut message_type = None;
                let mut parts = Vec::with_capacity(messages.len());
                for (index, message) in messages.into_iter().enumerate() {
                    let fields = wire::fields(bytes, message.clone()).map_err(refused)?;
                    if index == 0 {
                        message_type = announced(bytes, &fields);
                    }
                    parts.push(part_of(bytes, index + 1, message));
                }
                Ok(Shaped {
                    parts,
                    message_type,
                })
            }
        }
    }
}

/// One message of a stream as a part.
fn part_of(bytes: &[u8], number: usize, message: Range<usize>) -> Part {
    Part::new(
        Some(number.to_string()),
        &bytes[message],
        Some("application/protobuf".to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcore::StreamId;

    fn stream(bytes: &[u8], media: Option<&str>) -> Stream {
        Stream::new(StreamId::new(1), bytes.to_vec(), media.map(str::to_string))
    }

    /// Field 1 varint 150, field 2 "abc", field 3 fixed32, field 4 fixed64,
    /// field 5 a group of field 1 varint 1, field 2 "d" again.
    const MESSAGE: &[u8] = b"\x08\x96\x01\x12\x03abc\x1d\x01\x02\x03\x04\
\x21\x01\x02\x03\x04\x05\x06\x07\x08\x2b\x08\x01\x2c\x12\x01d";

    #[test]
    fn a_message_is_one_part_per_field_named_by_number_with_the_wire_type_as_suffix() {
        let shaped = Protobuf::message()
            .shape(&stream(MESSAGE, None))
            .expect("a message");
        let names: Vec<&str> = shaped
            .parts
            .iter()
            .filter_map(|p| p.name.as_deref())
            .collect();
        assert_eq!(names, ["1", "2", "3", "4", "5", "2"]);
        let media: Vec<&str> = shaped
            .parts
            .iter()
            .filter_map(|p| p.media_type.as_deref())
            .collect();
        assert_eq!(
            media,
            [
                "application/protobuf+varint",
                "application/protobuf+len",
                "application/protobuf+i32",
                "application/protobuf+i64",
                "application/protobuf+group",
                "application/protobuf+len",
            ]
        );
        assert_eq!(shaped.parts[0].bytes, b"\x96\x01");
        assert_eq!(shaped.parts[1].bytes, b"abc");
        assert_eq!(shaped.parts[2].bytes, b"\x01\x02\x03\x04");
        assert_eq!(shaped.parts[4].bytes, b"\x08\x01");
        assert_eq!(shaped.parts[5].bytes, b"d");
        assert_eq!(shaped.message_type, None);
    }

    #[test]
    fn an_any_shaped_first_field_announces_the_type_behind_its_url() {
        let any = b"\x0a\x25type.googleapis.com/com.example.Order\x12\x02\x08\x01";
        let shaped = Protobuf::message()
            .shape(&stream(any, None))
            .expect("a message");
        assert_eq!(shaped.message_type.as_deref(), Some("com.example.Order"));
        assert_eq!(shaped.parts.len(), 2);

        let plain = Protobuf::message()
            .shape(&stream(b"\x0a\x05hello", None))
            .expect("a message");
        assert_eq!(plain.message_type, None);
        let wrong_second = b"\x0a\x03a/B\x10\x01";
        let shaped = Protobuf::message()
            .shape(&stream(wrong_second, None))
            .expect("a message");
        assert_eq!(shaped.message_type, None);
    }

    #[test]
    fn a_delimited_stream_is_one_part_per_message_each_walked() {
        let bytes = b"\x02\x08\x01\x03\x12\x01a\x00";
        let shaped = Protobuf::delimited()
            .shape(&stream(bytes, Some("application/protobuf")))
            .expect("a stream");
        assert_eq!(shaped.parts.len(), 3);
        assert_eq!(shaped.parts[0].name.as_deref(), Some("1"));
        assert_eq!(shaped.parts[0].bytes, b"\x08\x01");
        assert_eq!(
            shaped.parts[0].media_type.as_deref(),
            Some("application/protobuf")
        );
        assert_eq!(shaped.parts[1].bytes, b"\x12\x01a");
        assert_eq!(shaped.parts[2].bytes, b"");
        assert_eq!(shaped.message_type, None);

        let typed = b"\x07\x0a\x03x/Y\x12\x00\x02\x08\x01";
        let shaped = Protobuf::delimited()
            .shape(&stream(typed, None))
            .expect("a stream");
        assert_eq!(shaped.message_type.as_deref(), Some("Y"));
        assert_eq!(Protobuf::delimited().framing(), Framing::Delimited);
    }

    #[test]
    fn a_cut_field_a_bad_tag_a_cut_stream_and_no_bytes_are_refused_where_they_fail() {
        let cut = Protobuf::message()
            .shape(&stream(b"\x12\x05ab", None))
            .expect_err("cut inside a field");
        assert_eq!(cut.offset, Some(4));
        assert_eq!(
            cut.to_string(),
            "protobuf: the message ends inside a field at byte 4"
        );

        let zero = Protobuf::message()
            .shape(&stream(b"\x08\x01\x00", None))
            .expect_err("field number 0");
        assert_eq!(zero.reason, "a field number outside the format's range");
        assert_eq!(zero.offset, Some(2));

        let wire = Protobuf::message()
            .shape(&stream(b"\x0e", None))
            .expect_err("wire type 6");
        assert_eq!(wire.reason, "a wire type the format does not have");

        let stream_cut = Protobuf::delimited()
            .shape(&stream(b"\x05\x08\x01", None))
            .expect_err("cut inside a message");
        assert_eq!(stream_cut.reason, "the stream ends inside a message");
        assert_eq!(stream_cut.offset, Some(3));

        let not_messages = Protobuf::delimited()
            .shape(&stream(b"\x01\x00", None))
            .expect_err("a message that is not one");
        assert_eq!(not_messages.offset, Some(1));

        let empty = Protobuf::message()
            .shape(&stream(b"", None))
            .expect_err("nothing");
        assert_eq!(empty.reason, "no bytes");
    }

    #[test]
    fn the_shape_claims_protobuf_and_recognises_bytes_that_walk_as_its_framing() {
        assert_eq!(Protobuf::default().technology(), "protobuf");
        assert!(
            Protobuf::default()
                .media_types()
                .contains(&"application/protobuf")
        );
        assert!(Protobuf::message().recognises(MESSAGE));
        assert!(!Protobuf::message().recognises(b"hello world"));
        assert!(!Protobuf::message().recognises(b""));
        assert!(Protobuf::delimited().recognises(b"\x02\x08\x01"));
        assert!(!Protobuf::delimited().recognises(b"\x02\x00\x01"));
        assert!(!Protobuf::delimited().recognises(b"\x09\x08\x01"));

        let shapes: [&dyn Shape; 1] = [&Protobuf::message()];
        let by_media = message::choose(&shapes, &stream(b"x", Some("Application/X-Protobuf")));
        assert_eq!(by_media.map(Shape::technology), Some("protobuf"));
        let by_look = message::choose(&shapes, &stream(b"\x08\x01", None));
        assert_eq!(by_look.map(Shape::technology), Some("protobuf"));
        assert!(message::choose(&shapes, &stream(b"{}", None)).is_none());
    }
}
