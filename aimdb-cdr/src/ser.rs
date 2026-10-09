use serde::ser::{self, Serialize};

use crate::{Error, Result};

/// Byte sink behind the serializer.
pub(crate) trait Write {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()>;
}

/// Writes into a fixed slice; overflow is [`Error::BufferTooSmall`].
pub(crate) struct SliceWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> SliceWriter<'a> {
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }
}

impl Write for SliceWriter<'_> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self.len + bytes.len();
        self.buf
            .get_mut(self.len..end)
            .ok_or(Error::BufferTooSmall)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

#[cfg(feature = "alloc")]
impl Write for &mut alloc::vec::Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// Little-endian XCDR1 body serializer. `pos` counts from the end of the
/// header, which is where CDR alignment is measured from.
pub(crate) struct Serializer<W> {
    out: W,
    pos: usize,
}

impl<W: Write> Serializer<W> {
    pub(crate) fn new(out: W) -> Self {
        Self { out, pos: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write_all(bytes)?;
        self.pos += bytes.len();
        Ok(())
    }

    fn align(&mut self, alignment: usize) -> Result<()> {
        let padding = (alignment - self.pos % alignment) % alignment;
        self.write(&[0; 8][..padding])
    }

    fn primitive<const N: usize>(&mut self, bytes: [u8; N]) -> Result<()> {
        self.align(N)?;
        self.write(&bytes)
    }

    fn length(&mut self, len: usize) -> Result<()> {
        let len = u32::try_from(len).map_err(|_| Error::LengthOverflow)?;
        self.primitive(len.to_le_bytes())
    }
}

impl<W: Write> ser::Serializer for &mut Serializer<W> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = ser::Impossible<(), Error>;
    type SerializeMap = ser::Impossible<(), Error>;
    type SerializeStruct = Self;
    type SerializeStructVariant = ser::Impossible<(), Error>;

    fn serialize_bool(self, v: bool) -> Result<()> {
        self.write(&[u8::from(v)])
    }

    fn serialize_i8(self, v: i8) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_i16(self, v: i16) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_i32(self, v: i32) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_i64(self, v: i64) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_u8(self, v: u8) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_u16(self, v: u16) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_u32(self, v: u32) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_u64(self, v: u64) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_f32(self, v: f32) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_f64(self, v: f64) -> Result<()> {
        self.primitive(v.to_le_bytes())
    }

    fn serialize_char(self, _v: char) -> Result<()> {
        Err(Error::Unsupported("char"))
    }

    fn serialize_str(self, v: &str) -> Result<()> {
        self.length(v.len() + 1)?;
        self.write(v.as_bytes())?;
        self.write(&[0])
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        self.length(v.len())?;
        self.write(v)
    }

    fn serialize_none(self) -> Result<()> {
        Err(Error::Unsupported("Option"))
    }

    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<()> {
        Err(Error::Unsupported("Option"))
    }

    fn serialize_unit(self) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_variant(self, _: &'static str, _: u32, _: &'static str) -> Result<()> {
        Err(Error::Unsupported("enum"))
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<()> {
        Err(Error::Unsupported("enum"))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self> {
        self.length(len.ok_or(Error::UnknownLength)?)?;
        Ok(self)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self> {
        Ok(self)
    }

    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        Err(Error::Unsupported("enum"))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap> {
        Err(Error::Unsupported("map"))
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant> {
        Err(Error::Unsupported("enum"))
    }

    fn collect_str<T: core::fmt::Display + ?Sized>(self, _value: &T) -> Result<()> {
        Err(Error::Unsupported("Display value"))
    }

    fn is_human_readable(&self) -> bool {
        false
    }
}

impl<W: Write> ser::SerializeSeq for &mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: Write> ser::SerializeTuple for &mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: Write> ser::SerializeTupleStruct for &mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: Write> ser::SerializeStruct for &mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<()> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}
