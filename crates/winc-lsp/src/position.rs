//! winc `char` 偏移与 LSP UTF-16 位置之间的无歧义转换。

use crate::protocol::Position;

/// 把 `char` 偏移转换为 UTF-16 code unit 偏移。
///
/// `offset` 必须落在 Unicode `char` 边界；否则返回 `None`。每个非代理对字符计 1，
/// 每个补充平面字符（例如 emoji）计 2。
pub fn char_to_utf16(text: &str, offset: usize) -> Option<usize> {
    let byte_offset = if offset == 0 {
        0
    } else {
        text.char_indices()
            .nth(offset)
            .or_else(|| (offset == text.chars().count()).then_some((text.len(), '\0')))
            .map(|(index, _)| index)?
    };
    Some(text[..byte_offset].encode_utf16().count())
}

/// 把 UTF-16 code unit 偏移转换为 `char` 偏移。
///
/// 目标落在代理对中间时返回 `None`，不能悄悄取前一个或后一个字符。
pub fn utf16_to_char(text: &str, offset: usize) -> Option<usize> {
    let mut units = 0;
    let mut characters = 0;
    for character in text.chars() {
        let width = character.len_utf16();
        if offset == units {
            return Some(characters);
        }
        if offset < units + width {
            return None;
        }
        units += width;
        characters += 1;
    }
    (offset == units).then_some(characters)
}

/// 把 UTF-16 code unit 偏移转换为 UTF-8 字节偏移。
///
/// 目标落在代理对中间时返回 `None`。
pub fn utf16_to_byte_offset(text: &str, offset: usize) -> Option<usize> {
    let character = utf16_to_char(text, offset)?;
    Some(
        text.char_indices()
            .nth(character)
            .map_or(text.len(), |(index, _)| index),
    )
}

/// 把文本中的 `char` 偏移转换为 LSP [`Position`]。
///
/// 识别 LF、CRLF 和 CR；CRLF 计为一个换行。`offset` 超出文本时返回 `None`。
pub fn char_position(text: &str, offset: usize) -> Option<Position> {
    let byte_offset = if offset == 0 {
        0
    } else {
        text.char_indices()
            .nth(offset)
            .or_else(|| (offset == text.chars().count()).then_some((text.len(), '\0')))
            .map(|(index, _)| index)?
    };
    Some(char_position_at_byte_offset(text, byte_offset))
}

fn char_position_at_byte_offset(text: &str, offset: usize) -> Position {
    let mut line = 0_u32;
    let mut line_start = 0_usize;
    let mut cursor = 0_usize;
    while cursor < offset {
        let character = text[cursor..].chars().next().expect("字节偏移来自字符边界");
        match character {
            '\r' => {
                line += 1;
                cursor += 1;
                line_start = cursor;
                if cursor < offset && text.as_bytes().get(cursor) == Some(&b'\n') {
                    cursor += 1;
                    line_start = cursor;
                }
            }
            '\n' => {
                line += 1;
                cursor += 1;
                line_start = cursor;
            }
            _ => cursor += character.len_utf8(),
        }
    }
    Position::new(line, text[line_start..offset].encode_utf16().count() as u32)
}

/// 把 LSP [`Position`] 转换为文本中的 `char` 偏移。
///
/// 行号或 UTF-16 列超出文本范围、列落在代理对中间时返回 `None`。
pub fn utf16_position(text: &str, line: u32, character: u32) -> Option<usize> {
    let byte_offset = utf16_position_byte_offset(text, line, character)?;
    Some(text[..byte_offset].chars().count())
}

/// 把 LSP [`Position`] 转换为 UTF-8 字节偏移。
///
/// 行号或 UTF-16 列超出文本范围、列落在代理对中间时返回 `None`。
pub fn utf16_position_byte_offset(text: &str, line: u32, character: u32) -> Option<usize> {
    let mut current_line = 0_u32;
    let mut line_start = 0_usize;
    let mut cursor = 0_usize;
    while cursor < text.len() {
        if current_line == line {
            break;
        }
        let next = text[cursor..].chars().next()?;
        if next == '\r' {
            cursor += 1;
            if text.as_bytes().get(cursor) == Some(&b'\n') {
                cursor += 1;
            }
        } else if next == '\n' {
            cursor += 1;
        } else {
            cursor += next.len_utf8();
        }
        if matches!(next, '\r' | '\n') {
            current_line += 1;
            line_start = cursor;
        }
    }
    if current_line != line {
        return None;
    }
    let line_end = text[line_start..]
        .find(['\r', '\n'])
        .map_or(text.len(), |relative| line_start + relative);
    let offset = utf16_to_byte_offset(&text[line_start..line_end], character as usize)?;
    Some(line_start + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_chinese_and_emoji_offsets() {
        let text = "a中😀b";
        assert_eq!(char_to_utf16(text, 0), Some(0));
        assert_eq!(char_to_utf16(text, 1), Some(1));
        assert_eq!(char_to_utf16(text, 2), Some(2));
        assert_eq!(char_to_utf16(text, 3), Some(4));
        assert_eq!(char_to_utf16(text, 4), Some(5));
        assert_eq!(char_to_utf16(text, 5), None);
        assert_eq!(utf16_to_char(text, 0), Some(0));
        assert_eq!(utf16_to_char(text, 1), Some(1));
        assert_eq!(utf16_to_char(text, 2), Some(2));
        assert_eq!(utf16_to_char(text, 3), None);
        assert_eq!(utf16_to_char(text, 4), Some(3));
        assert_eq!(utf16_to_char(text, 5), Some(4));
        assert_eq!(utf16_to_char(text, 6), None);
        assert_eq!(utf16_to_byte_offset(text, 2), Some(4));
        assert_eq!(utf16_to_byte_offset(text, 4), Some(8));
        assert_eq!(utf16_to_byte_offset(text, 5), Some(9));
    }

    #[test]
    fn converts_emoji_at_line_start() {
        let text = "😀main\n";
        assert_eq!(Position::new(0, 2).to_char_offset(text), Some(1));
        assert_eq!(Position::new(0, 6).to_char_offset(text), Some(5));
        assert_eq!(Position::new(0, 2).to_byte_offset(text), Some(4));
        assert_eq!(Position::new(0, 6).to_byte_offset(text), Some(8));
        assert_eq!(Position::new(0, 1).to_char_offset(text), None);
        assert_eq!(Position::new(1, 0).to_char_offset(text), Some(6));
    }

    #[test]
    fn converts_positions_across_all_supported_newlines() {
        let text = "中😀\r\nx\ry\nz";
        let cases = [
            (0, Position::new(0, 0)),
            (1, Position::new(0, 1)),
            (2, Position::new(0, 3)),
            (4, Position::new(1, 0)),
            (5, Position::new(1, 1)),
            (6, Position::new(2, 0)),
            (8, Position::new(3, 0)),
            (9, Position::new(3, 1)),
        ];
        for (offset, position) in cases {
            assert_eq!(
                char_position(text, offset),
                Some(position),
                "offset {offset}"
            );
            assert_eq!(
                utf16_position(text, position.line, position.character),
                Some(offset),
                "position {position:?}"
            );
        }
        assert_eq!(char_position(text, 10), None);
        assert_eq!(utf16_position(text, 0, 2), None);
        assert_eq!(utf16_position(text, 0, 4), None);
        assert_eq!(utf16_position(text, 99, 0), None);
        assert_eq!(utf16_position(text, 1, u32::MAX), None);
    }

    #[test]
    fn converts_utf16_columns_after_emoji_without_skipping_to_later_lines() {
        let text = "😀main\n";
        assert_eq!(utf16_to_char(text, 0), Some(0));
        assert_eq!(utf16_to_char(text, 1), None);
        assert_eq!(utf16_to_char(text, 2), Some(1));
        assert_eq!(utf16_position(text, 0, 2), Some(1));
        assert_eq!(utf16_position(text, 0, 6), Some(5));
        assert_eq!(utf16_position(text, 1, 0), Some(6));
    }

    #[test]
    fn handles_empty_text() {
        assert_eq!(char_position("", 0), Some(Position::new(0, 0)));
        assert_eq!(utf16_position("", 0, 0), Some(0));
        assert_eq!(utf16_position("", 1, 0), None);
    }
}
