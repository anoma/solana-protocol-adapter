//! Minimal word-based serializer compatible with `risc0_zkvm::serde::to_vec`.
//!
//! The zkVM uses a Serde serializer that emits `u32` words (little-endian when
//! converted to bytes). For on-chain verification we need to re-create the same
//! word stream to compute the journal digest that the prover committed to.

use alloc::vec::Vec;

use serde::ser::{self, Serialize};

const WORD_SIZE: usize = 4;

#[derive(Debug)]
pub enum Error {
    NotSupported,
    Message(&'static str),
}

pub type Result<T> = core::result::Result<T, Error>;

impl ser::Error for Error {
    fn custom<T: core::fmt::Display>(_msg: T) -> Self {
        Error::Message("custom error")
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotSupported => write!(f, "not supported"),
            Error::Message(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// A writer for writing streams preferring word-based data.
pub trait WordWrite {
    fn write_words(&mut self, words: &[u32]) -> Result<()>;
    fn write_padded_bytes(&mut self, bytes: &[u8]) -> Result<()>;
}

/// Counts how many u32 words a value serializes into using this serializer.
#[derive(Default)]
pub struct WordCounter {
    words: usize,
}

impl WordCounter {
    pub fn words(&self) -> usize {
        self.words
    }
}

impl WordWrite for WordCounter {
    fn write_words(&mut self, words: &[u32]) -> Result<()> {
        self.words = self.words.saturating_add(words.len());
        Ok(())
    }

    fn write_padded_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let padded_words = bytes.len().div_ceil(WORD_SIZE);
        self.words = self.words.saturating_add(padded_words);
        Ok(())
    }
}

impl WordWrite for Vec<u32> {
    fn write_words(&mut self, words: &[u32]) -> Result<()> {
        self.extend_from_slice(words);
        Ok(())
    }

    fn write_padded_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let chunks = bytes.chunks_exact(WORD_SIZE);
        let last_word = chunks.remainder();
        self.extend(chunks.map(|word_bytes| u32::from_le_bytes(word_bytes.try_into().unwrap())));
        if !last_word.is_empty() {
            let mut last_word_bytes = [0u8; WORD_SIZE];
            last_word_bytes[..last_word.len()].clone_from_slice(last_word);
            self.push(u32::from_le_bytes(last_word_bytes));
        }
        Ok(())
    }
}

impl<W: WordWrite + ?Sized> WordWrite for &mut W {
    fn write_words(&mut self, words: &[u32]) -> Result<()> {
        (**self).write_words(words)
    }

    fn write_padded_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        (**self).write_padded_bytes(bytes)
    }
}

pub fn to_vec<T>(value: &T) -> Result<Vec<u32>>
where
    T: Serialize + ?Sized,
{
    let mut vec: Vec<u32> = Vec::with_capacity(core::mem::size_of_val(value));
    let mut serializer = Serializer::new(&mut vec);
    value.serialize(&mut serializer)?;
    Ok(vec)
}

pub fn count_words<T>(value: &T) -> Result<usize>
where
    T: Serialize + ?Sized,
{
    let mut counter = WordCounter::default();
    let mut serializer = Serializer::new(&mut counter);
    value.serialize(&mut serializer)?;
    Ok(counter.words())
}

pub struct Serializer<W: WordWrite> {
    stream: W,
}

impl<W: WordWrite> Serializer<W> {
    pub fn new(stream: W) -> Self {
        Serializer { stream }
    }
}

impl<W: WordWrite> ser::Serializer for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn is_human_readable(&self) -> bool {
        false
    }

    fn serialize_bool(self, v: bool) -> Result<()> {
        self.serialize_u8(if v { 1 } else { 0 })
    }

    fn serialize_i8(self, v: i8) -> Result<()> {
        self.serialize_i32(v as i32)
    }

    fn serialize_i16(self, v: i16) -> Result<()> {
        self.serialize_i32(v as i32)
    }

    fn serialize_i32(self, v: i32) -> Result<()> {
        self.serialize_u32(v as u32)
    }

    fn serialize_i64(self, v: i64) -> Result<()> {
        self.serialize_u64(v as u64)
    }

    fn serialize_i128(self, v: i128) -> Result<()> {
        self.serialize_u128(v as u128)
    }

    fn serialize_u8(self, v: u8) -> Result<()> {
        self.serialize_u32(v as u32)
    }

    fn serialize_u16(self, v: u16) -> Result<()> {
        self.serialize_u32(v as u32)
    }

    fn serialize_u32(self, v: u32) -> Result<()> {
        self.stream.write_words(&[v])
    }

    fn serialize_u64(self, v: u64) -> Result<()> {
        self.serialize_u32((v & 0xFFFF_FFFF) as u32)?;
        self.serialize_u32(((v >> 32) & 0xFFFF_FFFF) as u32)
    }

    fn serialize_u128(self, v: u128) -> Result<()> {
        self.stream.write_padded_bytes(&v.to_le_bytes())
    }

    fn serialize_f32(self, v: f32) -> Result<()> {
        self.serialize_u32(v.to_bits())
    }

    fn serialize_f64(self, v: f64) -> Result<()> {
        self.serialize_u64(f64::to_bits(v))
    }

    fn serialize_char(self, v: char) -> Result<()> {
        self.serialize_u32(v as u32)
    }

    fn serialize_str(self, v: &str) -> Result<()> {
        let bytes = v.as_bytes();
        self.serialize_u32(bytes.len() as u32)?;
        self.stream.write_padded_bytes(bytes)
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<()> {
        self.serialize_u32(v.len() as u32)?;
        self.stream.write_padded_bytes(v)
    }

    fn serialize_none(self) -> Result<()> {
        self.serialize_u32(0)
    }

    fn serialize_some<T>(self, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        self.serialize_u32(1)?;
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<()> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
    ) -> Result<()> {
        self.serialize_u32(variant_index)
    }

    fn serialize_newtype_struct<T>(self, _name: &'static str, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        self.serialize_u32(variant_index)?;
        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq> {
        match len {
            Some(val) => {
                self.serialize_u32(val.try_into().unwrap())?;
                Ok(self)
            }
            None => Err(Error::NotSupported),
        }
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple> {
        Ok(self)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct> {
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant> {
        self.serialize_u32(variant_index)?;
        Ok(self)
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap> {
        match len {
            Some(val) => {
                self.serialize_u32(val.try_into().unwrap())?;
                Ok(self)
            }
            None => Err(Error::NotSupported),
        }
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct> {
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant> {
        self.serialize_u32(variant_index)?;
        Ok(self)
    }
}

impl<W: WordWrite> ser::SerializeSeq for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: WordWrite> ser::SerializeTuple for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T>(&mut self, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: WordWrite> ser::SerializeTupleStruct for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: WordWrite> ser::SerializeTupleVariant for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: WordWrite> ser::SerializeMap for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T>(&mut self, key: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        key.serialize(&mut **self)
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: WordWrite> ser::SerializeStruct for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, _key: &'static str, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}

impl<W: WordWrite> ser::SerializeStructVariant for &'_ mut Serializer<W> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T>(&mut self, _key: &'static str, value: &T) -> Result<()>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<()> {
        Ok(())
    }
}
