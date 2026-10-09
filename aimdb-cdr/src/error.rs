use core::fmt;

/// Result alias for this crate.
pub type Result<T> = core::result::Result<T, Error>;

/// Encode or decode failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The output slice is too small.
    BufferTooSmall,
    /// The input ended early, or a length prefix exceeds the input.
    UnexpectedEof,
    /// The representation identifier is not `CDR_LE` or `CDR_BE`.
    InvalidHeader([u8; 2]),
    /// A `bool` byte other than 0 or 1.
    InvalidBool(u8),
    /// A string is not UTF-8.
    InvalidUtf8,
    /// A string is not NUL-terminated.
    MissingNul,
    /// A length does not fit the `u32` prefix.
    LengthOverflow,
    /// A sequence was serialized without a known length.
    UnknownLength,
    /// A serde construct with no CDR mapping.
    Unsupported(&'static str),
    /// A serde impl reported an error (message dropped without `alloc`).
    Custom,
    /// A serde impl reported an error.
    #[cfg(feature = "alloc")]
    Message(alloc::string::String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall => f.write_str("output buffer too small"),
            Self::UnexpectedEof => f.write_str("unexpected end of input"),
            Self::InvalidHeader(id) => write!(f, "unsupported CDR representation {id:02x?}"),
            Self::InvalidBool(b) => write!(f, "invalid bool byte {b}"),
            Self::InvalidUtf8 => f.write_str("string is not UTF-8"),
            Self::MissingNul => f.write_str("string is not NUL-terminated"),
            Self::LengthOverflow => f.write_str("length exceeds u32"),
            Self::UnknownLength => f.write_str("sequence length unknown"),
            Self::Unsupported(what) => write!(f, "{what} has no CDR mapping"),
            Self::Custom => f.write_str("serde error"),
            #[cfg(feature = "alloc")]
            Self::Message(msg) => f.write_str(msg),
        }
    }
}

impl core::error::Error for Error {}

impl Error {
    fn custom<T: fmt::Display>(_msg: T) -> Self {
        #[cfg(feature = "alloc")]
        {
            Self::Message(alloc::string::ToString::to_string(&_msg))
        }
        #[cfg(not(feature = "alloc"))]
        {
            Self::Custom
        }
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::custom(msg)
    }
}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error::custom(msg)
    }
}
