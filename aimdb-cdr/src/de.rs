use serde::de::{self, DeserializeSeed, Visitor};

use crate::{Error, Result};

/// XCDR1 body deserializer. `pos` counts from the end of the header.
pub(crate) struct Deserializer<'de> {
    input: &'de [u8],
    pos: usize,
    big_endian: bool,
}

macro_rules! read_primitive {
    ($name:ident, $ty:ty) => {
        fn $name(&mut self) -> Result<$ty> {
            let bytes = self.primitive::<{ core::mem::size_of::<$ty>() }>()?;
            Ok(if self.big_endian {
                <$ty>::from_be_bytes(bytes)
            } else {
                <$ty>::from_le_bytes(bytes)
            })
        }
    };
}

impl<'de> Deserializer<'de> {
    pub(crate) fn new(input: &'de [u8], big_endian: bool) -> Self {
        Self {
            input,
            pos: 0,
            big_endian,
        }
    }

    fn remaining(&self) -> usize {
        self.input.len() - self.pos
    }

    fn take(&mut self, len: usize) -> Result<&'de [u8]> {
        let end = self.pos.checked_add(len).ok_or(Error::UnexpectedEof)?;
        let bytes = self.input.get(self.pos..end).ok_or(Error::UnexpectedEof)?;
        self.pos = end;
        Ok(bytes)
    }

    fn align(&mut self, alignment: usize) -> Result<()> {
        let padding = (alignment - self.pos % alignment) % alignment;
        self.take(padding).map(drop)
    }

    fn primitive<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.align(N)?;
        let mut bytes = [0; N];
        bytes.copy_from_slice(self.take(N)?);
        Ok(bytes)
    }

    read_primitive!(read_u16, u16);
    read_primitive!(read_u32, u32);
    read_primitive!(read_u64, u64);
    read_primitive!(read_i16, i16);
    read_primitive!(read_i32, i32);
    read_primitive!(read_i64, i64);
    read_primitive!(read_f32, f32);
    read_primitive!(read_f64, f64);

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// A `u32` length prefix. Every ROS element takes at least one byte, so a
    /// count beyond the remaining input is rejected before anything allocates.
    fn read_length(&mut self) -> Result<usize> {
        let len = usize::try_from(self.read_u32()?).map_err(|_| Error::UnexpectedEof)?;
        if len > self.remaining() {
            return Err(Error::UnexpectedEof);
        }
        Ok(len)
    }

    fn read_str(&mut self) -> Result<&'de str> {
        let len = self.read_length()?;
        if len == 0 {
            return Ok("");
        }
        let (text, nul) = self.take(len)?.split_at(len - 1);
        if nul != [0] {
            return Err(Error::MissingNul);
        }
        core::str::from_utf8(text).map_err(|_| Error::InvalidUtf8)
    }
}

impl<'de> de::Deserializer<'de> for &mut Deserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(Error::Unsupported("self-describing deserialize"))
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.read_u8()? {
            0 => visitor.visit_bool(false),
            1 => visitor.visit_bool(true),
            other => Err(Error::InvalidBool(other)),
        }
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_i8(i8::from_le_bytes([self.read_u8()?]))
    }

    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_i16(self.read_i16()?)
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_i32(self.read_i32()?)
    }

    fn deserialize_i64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_i64(self.read_i64()?)
    }

    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_u8(self.read_u8()?)
    }

    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_u16(self.read_u16()?)
    }

    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_u32(self.read_u32()?)
    }

    fn deserialize_u64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_u64(self.read_u64()?)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f32(self.read_f32()?)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_f64(self.read_f64()?)
    }

    fn deserialize_char<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(Error::Unsupported("char"))
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_borrowed_str(self.read_str()?)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.read_length()?;
        visitor.visit_borrowed_bytes(self.take(len)?)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(Error::Unsupported("Option"))
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        let len = self.read_length()?;
        visitor.visit_seq(Elements {
            de: self,
            left: len,
        })
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value> {
        visitor.visit_seq(Elements {
            de: self,
            left: len,
        })
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(Error::Unsupported("map"))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value> {
        Err(Error::Unsupported("enum"))
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(Error::Unsupported("identifier"))
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(Error::Unsupported("ignored field"))
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

/// Fixed-count access for sequences, arrays and struct fields.
struct Elements<'a, 'de> {
    de: &'a mut Deserializer<'de>,
    left: usize,
}

impl<'de> de::SeqAccess<'de> for Elements<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        seed.deserialize(&mut *self.de).map(Some)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.left.min(self.de.remaining()))
    }
}
