use std::fmt;
use std::io;

/// All errors produced by encoding/decoding frames.
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Image(image::ImageError),
    /// Payload does not fit in the frame.
    Overflow { capacity: usize, len: usize },
    /// Frame dimensions are unusable (too small, zero, or not divisible by scale).
    BadDimensions(String),
    /// Header magic/mode/length is invalid - not a frame from this tool, or badly damaged.
    BadHeader(String),
    /// One or more columns failed their CRC-24 check.
    ColumnCrc(Vec<u32>),
    /// Payload CRC-32 mismatch (data recovered but does not match what was stored).
    PayloadCrc { expected: u32, actual: u32 },
    /// A Reed-Solomon block had more damage than its parity can repair.
    Uncorrectable { block: usize },
    /// Invalid encoding parameter (e.g. ECC size).
    InvalidParameter(String),
    /// Unknown scaling algorithm or invalid scale factor.
    Scale(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Image(e) => write!(f, "image error: {e}"),
            Error::Overflow { capacity, len } => {
                write!(f, "data too large: {len} bytes, frame capacity is {capacity} bytes")
            }
            Error::BadDimensions(m) => write!(f, "bad dimensions: {m}"),
            Error::BadHeader(m) => write!(f, "bad header: {m}"),
            Error::ColumnCrc(cols) => write!(f, "CRC-24 failed on {} column(s): {:?}", cols.len(), cols),
            Error::PayloadCrc { expected, actual } => {
                write!(f, "payload CRC-32 mismatch: expected {expected:08X}, got {actual:08X}")
            }
            Error::Uncorrectable { block } => {
                write!(f, "too much damage: Reed-Solomon block {block} could not be corrected")
            }
            Error::InvalidParameter(m) => write!(f, "invalid parameter: {m}"),
            Error::Scale(m) => write!(f, "scaling error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<image::ImageError> for Error {
    fn from(e: image::ImageError) -> Self {
        Error::Image(e)
    }
}
