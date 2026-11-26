//! Content-Length 消息分帧：支持任意分片、粘包和多个连续消息。

use std::io::{self, Read, Write};

use serde_json::Value;

/// 默认允许的单条消息大小，避免损坏或恶意的服务器无限占用内存。
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// 解析 Content-Length 头或消息体时发生的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// 头部超过限制。
    HeadersTooLarge,
    /// 头部格式非法。
    InvalidHeader,
    /// 缺少 Content-Length。
    MissingContentLength,
    /// Content-Length 不是十进制整数。
    InvalidContentLength,
    /// Content-Length 为零。
    EmptyMessage,
    /// 消息体超过限制。
    MessageTooLarge,
    /// 输入在不完整的头部或消息体处结束。
    UnexpectedEof,
    /// 消息体不是合法 JSON。
    Json(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::HeadersTooLarge => "LSP 消息头超过限制",
            Self::InvalidHeader => "LSP 消息头格式错误",
            Self::MissingContentLength => "LSP 消息缺少 Content-Length",
            Self::InvalidContentLength => "LSP Content-Length 不是十进制整数",
            Self::EmptyMessage => "LSP 消息体为空",
            Self::MessageTooLarge => "LSP 消息体超过限制",
            Self::UnexpectedEof => "LSP 消息在完整传输前结束",
            Self::Json(message) => return write!(f, "LSP 消息不是合法 JSON：{message}"),
        };
        f.write_str(message)
    }
}

impl std::error::Error for DecodeError {}

/// 持续从输入读取 Content-Length 消息。
///
/// 内部缓冲保存尚未消费的分片或粘包数据，因此 [`read`](Self::read) 可以处理任意网络/管道分片。
pub struct FramedReader<R> {
    reader: R,
    buffer: Vec<u8>,
}

impl<R: Read> FramedReader<R> {
    /// 包装一个字节输入。
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
        }
    }

    /// 读取下一条消息。
    ///
    /// # Errors
    ///
    /// 输入结束、消息格式错误、消息过大或底层读取失败时返回错误。输入结束时若还有不完整消息，
    /// 返回 [`DecodeError::UnexpectedEof`]。
    pub fn read(&mut self) -> Result<Value, DecodeError> {
        loop {
            if let Some((body_start, body_len)) = Self::header(&self.buffer)? {
                let message_end = body_start
                    .checked_add(body_len)
                    .ok_or(DecodeError::MessageTooLarge)?;
                if self.buffer.len() >= message_end {
                    let body = self.buffer[body_start..message_end].to_vec();
                    self.buffer.drain(..message_end);
                    return serde_json::from_slice(&body)
                        .map_err(|error| DecodeError::Json(error.to_string()));
                }
            }

            let mut chunk = [0_u8; 8192];
            let read = self.reader.read(&mut chunk).map_err(|error| {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    DecodeError::UnexpectedEof
                } else {
                    DecodeError::Json(error.to_string())
                }
            })?;
            if read == 0 {
                return Err(DecodeError::UnexpectedEof);
            }
            self.buffer.extend_from_slice(&chunk[..read]);
            if self.buffer.len() > MAX_MESSAGE_BYTES {
                return Err(DecodeError::MessageTooLarge);
            }
        }
    }

    fn header(buffer: &[u8]) -> Result<Option<(usize, usize)>, DecodeError> {
        let Some(header_end) = find_header_end(buffer) else {
            if buffer.len() > 16 * 1024 {
                return Err(DecodeError::HeadersTooLarge);
            }
            return Ok(None);
        };
        let mut content_length = None;
        let mut cursor = 0;
        while cursor < header_end {
            let line_end = buffer[cursor..header_end]
                .windows(2)
                .position(|pair| pair == b"\r\n")
                .unwrap_or(header_end - cursor);
            let line = &buffer[cursor..cursor + line_end];
            if line.is_empty() {
                break;
            }
            let Some(colon) = line.iter().position(|byte| *byte == b':') else {
                return Err(DecodeError::InvalidHeader);
            };
            let name = trim_ascii(&line[..colon]);
            if name.eq_ignore_ascii_case(b"content-length") {
                let value = trim_ascii(&line[colon + 1..]);
                if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
                    return Err(DecodeError::InvalidContentLength);
                }
                let text =
                    std::str::from_utf8(value).map_err(|_| DecodeError::InvalidContentLength)?;
                let length = text
                    .parse::<usize>()
                    .map_err(|_| DecodeError::InvalidContentLength)?;
                if length == 0 {
                    return Err(DecodeError::EmptyMessage);
                }
                if length > MAX_MESSAGE_BYTES {
                    return Err(DecodeError::MessageTooLarge);
                }
                content_length = Some(length);
            }
            cursor += line_end + 2;
        }
        content_length
            .map(|length| Ok(Some((header_end + 4, length))))
            .unwrap_or(Err(DecodeError::MissingContentLength))
    }
}

/// 带 Content-Length 头的消息写入器。
pub struct FramedWriter<W> {
    writer: W,
}

impl<W: Write> FramedWriter<W> {
    /// 包装一个字节输出。
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    /// 序列化并写入一条消息，写入后立即刷新。
    ///
    /// # Errors
    ///
    /// JSON 序列化、底层写入或刷新失败时返回错误。
    pub fn write(&mut self, message: &Value) -> io::Result<()> {
        let body = serde_json::to_vec(message)?;
        write_header(&mut self.writer, body.len())?;
        self.writer.write_all(&body)?;
        self.writer.flush()
    }
}

/// 序列化 JSON 值并编码为完整 Content-Length 帧。
///
/// # Errors
///
/// 值无法序列化时返回错误。
pub fn encode_message(message: &Value) -> Result<Vec<u8>, serde_json::Error> {
    let body = serde_json::to_vec(message)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// 从完整帧（包括 Content-Length 头）解码一条 JSON 消息。
///
/// 输入有尾部数据时返回错误；连续消息请使用 [`FramedReader`]。
///
/// # Errors
///
/// 帧格式错误、JSON 非法或尾部存在数据时返回 [`DecodeError`]。
pub fn decode_message(frame: &[u8]) -> Result<Value, DecodeError> {
    let mut reader = FramedReader::new(frame);
    let value = reader.read()?;
    if !reader.buffer.is_empty() {
        return Err(DecodeError::InvalidHeader);
    }
    Ok(value)
}

fn write_header(writer: &mut impl Write, length: usize) -> io::Result<()> {
    write!(writer, "Content-Length: {length}\r\n\r\n")
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[test]
    fn reads_one_byte_fragments() {
        let first = frame(r#"{"jsonrpc":"2.0","method":"one","params":1}"#);
        let second = frame(r#"{"jsonrpc":"2.0","method":"two","params":"中😀"}"#);
        let bytes = [first, second].concat();
        let mut reader = FramedReader::new(bytes.as_slice());
        assert_eq!(reader.read().unwrap()["method"], "one");
        assert_eq!(reader.read().unwrap()["params"], "中😀");
        assert_eq!(reader.read(), Err(DecodeError::UnexpectedEof));
    }

    #[test]
    fn reads_stuck_together_frames() {
        let bytes = [frame(r#"{"a":1}"#), frame(r#"{"b":2}"#)].concat();
        let mut reader = FramedReader::new(std::io::Cursor::new(bytes));
        assert_eq!(reader.read().unwrap(), serde_json::json!({"a": 1}));
        assert_eq!(reader.read().unwrap(), serde_json::json!({"b": 2}));
    }

    #[test]
    fn accepts_extra_headers_and_case_insensitive_name() {
        let bytes = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}";
        let mut reader = FramedReader::new(bytes.as_slice());
        assert_eq!(reader.read().unwrap(), serde_json::json!({}));
    }

    #[test]
    fn rejects_bad_frames() {
        assert_eq!(
            decode_message(b"Content-Length: nope\r\n\r\n{}"),
            Err(DecodeError::InvalidContentLength)
        );
        assert_eq!(
            decode_message(b"Content-Type: text/plain\r\n\r\n{}"),
            Err(DecodeError::MissingContentLength)
        );
        assert!(matches!(
            decode_message(&frame("not json")),
            Err(DecodeError::Json(_))
        ));
        assert_eq!(
            decode_message(b"Content-Length: 2\r\n\r\n{}tail"),
            Err(DecodeError::InvalidHeader)
        );
    }

    #[test]
    fn round_trips_framing() {
        let value = serde_json::json!({"jsonrpc": "2.0", "id": 3, "result": "中😀"});
        let encoded = encode_message(&value).unwrap();
        assert!(
            encoded.starts_with(
                format!(
                    "Content-Length: {}\r\n\r\n",
                    serde_json::to_vec(&value).unwrap().len()
                )
                .as_bytes()
            )
        );
        assert_eq!(decode_message(&encoded).unwrap(), value);
        let mut output = Vec::new();
        FramedWriter::new(&mut output).write(&value).unwrap();
        assert_eq!(decode_message(&output).unwrap(), value);
    }

    #[test]
    fn reports_incomplete_frames_at_eof() {
        assert_eq!(
            decode_message(b"Content-Length: 10\r\n\r\n{}"),
            Err(DecodeError::UnexpectedEof)
        );
        assert_eq!(
            decode_message(b"Content-Len"),
            Err(DecodeError::UnexpectedEof)
        );
    }
}
