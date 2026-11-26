//! Content-Length framing for DAP's TCP stream.

use std::io::{Read, Write};

use serde_json::Value;

/// Maximum accepted message size (64 MiB).
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Invalid framing or JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Header block is incomplete or invalid.
    Header,
    /// Content-Length is missing, malformed, or too large.
    Length,
    /// Stream ended in a header or body.
    Eof,
    /// Body is not UTF-8 JSON.
    Json(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Header => f.write_str("invalid DAP header"),
            Self::Length => f.write_str("invalid DAP Content-Length"),
            Self::Eof => f.write_str("DAP stream closed mid-message"),
            Self::Json(error) => write!(f, "invalid DAP JSON: {error}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Incremental framed stream reader.
pub struct FramedReader<R> {
    reader: R,
    buffer: Vec<u8>,
}

impl<R: Read> FramedReader<R> {
    /// Creates a reader with an empty buffer.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::with_capacity(4096),
        }
    }

    /// Reads one complete message, handling fragmentation and coalescing.
    ///
    /// # Errors
    /// Returns I/O, framing, or JSON errors.
    pub fn read(&mut self) -> Result<Value, DecodeError> {
        loop {
            if let Some(end) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let header =
                    std::str::from_utf8(&self.buffer[..end]).map_err(|_| DecodeError::Header)?;
                let length = header
                    .split("\r\n")
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .filter(|&length| length <= MAX_MESSAGE_BYTES)
                    .ok_or(DecodeError::Length)?;
                let body = end + 4;
                if self.buffer.len() >= body + length {
                    let bytes = self.buffer.drain(..body + length).collect::<Vec<_>>();
                    return serde_json::from_slice(&bytes[body..])
                        .map_err(|error| DecodeError::Json(error.to_string()));
                }
            }
            if self.buffer.len() > MAX_MESSAGE_BYTES + 16 * 1024 {
                return Err(DecodeError::Length);
            }
            let mut chunk = [0u8; 8192];
            let count = self.reader.read(&mut chunk).map_err(|_| DecodeError::Eof)?;
            if count == 0 {
                return Err(DecodeError::Eof);
            }
            self.buffer.extend_from_slice(&chunk[..count]);
        }
    }
}

/// Writes Content-Length framed JSON messages.
pub struct FramedWriter<W> {
    writer: W,
}

impl<W: Write> FramedWriter<W> {
    /// Creates a writer.
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Serializes and writes one message.
    ///
    /// # Errors
    /// Returns serialization or I/O errors.
    pub fn write(
        &mut self,
        message: &Value,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::to_vec(message)?;
        write!(self.writer, "Content-Length: {}\r\n\r\n", body.len())?;
        self.writer.write_all(&body)?;
        self.writer.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_fragmented_and_coalesced_messages() {
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() || output.is_empty() {
                    return Ok(0);
                }
                output[0] = self.0[0];
                self.0 = &self.0[1..];
                Ok(1)
            }
        }
        let bytes = b"Content-Length: 2\r\n\r\n{}Content-Length: 2\r\n\r\n[]";
        let mut reader = FramedReader::new(OneByte(bytes));
        assert_eq!(reader.read().unwrap(), json!({}));
        assert_eq!(reader.read().unwrap(), json!([]));
        assert_eq!(reader.read().unwrap_err(), DecodeError::Eof);
    }
}
