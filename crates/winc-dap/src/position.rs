//! UTF-16 protocol positions without depending on an editor or LSP crate.

use serde::{Deserialize, Serialize};

/// A zero-based DAP/LSP position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    /// Zero-based line.
    pub line: u32,
    /// Zero-based UTF-16 code-unit column.
    pub character: u32,
}

impl Position {
    /// Creates a position.
    pub const fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// Converts a char offset to a UTF-16 code-unit offset.
pub fn char_to_utf16(text: &str, offset: usize) -> Option<usize> {
    let byte = if offset == 0 {
        0
    } else {
        text.char_indices()
            .nth(offset)
            .or_else(|| (offset == text.chars().count()).then_some((text.len(), '\0')))
            .map(|(at, _)| at)?
    };
    Some(text[..byte].encode_utf16().count())
}

/// Converts a UTF-16 code-unit offset to a char offset, rejecting surrogate interiors.
pub fn utf16_to_char(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0usize;
    for (index, ch) in text.char_indices() {
        if units == offset {
            return Some(index);
        }
        let width = ch.len_utf16();
        if offset < units + width {
            return None;
        }
        units += width;
    }
    (units == offset).then_some(text.len())
}

/// Converts a char offset to a line/column position.
pub fn char_position(text: &str, offset: usize) -> Option<Position> {
    let prefix = text.get(
        ..text
            .char_indices()
            .nth(offset)
            .map_or(text.len(), |(at, _)| at),
    )?;
    let line = prefix.bytes().filter(|&b| b == b'\n').count() as u32;
    let line_start = prefix.rfind('\n').map_or(0, |at| at + 1);
    let character =
        char_to_utf16(&prefix[line_start..], prefix[line_start..].chars().count())? as u32;
    Some(Position::new(line, character))
}

/// Converts a UTF-16 line/column position to a char offset.
pub fn utf16_position(text: &str, position: Position) -> Option<usize> {
    let line_start = (0..position.line).try_fold(0usize, |at, _| {
        text[at..].find('\n').map(|end| at + end + 1)
    })?;
    let line_end = text[line_start..]
        .find(['\r', '\n'])
        .map_or(text.len(), |end| line_start + end);
    let units = utf16_to_char(&text[line_start..line_end], position.character as usize)?;
    Some(line_start + units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_astral_characters_and_rejects_surrogate_interiors() {
        let text = "a😀b\n中";
        assert_eq!(char_to_utf16(text, 2), Some(3));
        assert_eq!(utf16_to_char(text, 2), None);
        assert_eq!(utf16_to_char(text, 3), Some(5));
        assert_eq!(char_position(text, 6), Some(Position::new(1, 1)));
        assert_eq!(utf16_position(text, Position::new(0, 3)), Some(5));
    }
}
