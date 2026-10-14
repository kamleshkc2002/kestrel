//! Line movement independent of GTK text widgets.

/// Direction in which a block of lines moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineDirection {
    Up,
    Down,
}

/// One line movement and the selection to apply after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMove {
    /// Character range replaced in the original text.
    pub range: (usize, usize),
    /// Text inserted into [`Self::range`].
    pub replacement: String,
    /// Selection range in the resulting text.
    pub selection: (usize, usize),
}

#[derive(Clone, Copy)]
struct Line {
    start: usize,
    content_end: usize,
    end: usize,
}

/// Moves the whole lines touched by `selection` one line in `direction`.
///
/// Offsets are character offsets. A selection ending at the start of a line
/// leaves that line out of the moved block, except for an empty cursor line.
pub fn move_lines(
    text: &str,
    selection: (usize, usize),
    direction: LineDirection,
) -> Option<LineMove> {
    let chars: Vec<char> = text.chars().collect();
    let lines = lines(&chars);
    let (start, end) = (selection.0.min(chars.len()), selection.1.min(chars.len()));
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let first = line_at(&lines, start);
    let end_line = line_at(&lines, end);
    let last = if end > start && end == lines[end_line].start && end_line > first {
        end_line - 1
    } else {
        end_line
    };
    let count = last - first + 1;

    let (neighbor, range_start, range_end) = match direction {
        LineDirection::Up if first > 0 => (first - 1, lines[first - 1].start, lines[last].end),
        LineDirection::Down if last + 1 < lines.len() => {
            (last + 1, lines[first].start, lines[last + 1].end)
        }
        _ => return None,
    };

    let ordered: Vec<usize> = match direction {
        LineDirection::Up => (first..=last).chain(std::iter::once(neighbor)).collect(),
        LineDirection::Down => std::iter::once(neighbor).chain(first..=last).collect(),
    };
    let final_newline = range_end < chars.len() || text.ends_with('\n');
    let mut replacement = String::new();
    for (index, &line) in ordered.iter().enumerate() {
        append_chars(
            &mut replacement,
            &chars,
            lines[line].start,
            lines[line].content_end,
        );
        if index + 1 < ordered.len() || final_newline {
            replacement.push('\n');
        }
    }

    let output_line_start = |output_index: usize| {
        range_start
            + ordered
                .iter()
                .take(output_index)
                .map(|&line| lines[line].content_end - lines[line].start + 1)
                .sum::<usize>()
    };
    let destination_output_index = match direction {
        LineDirection::Up => 0,
        LineDirection::Down => 1,
    };
    let map_position = |offset: usize, line: usize| {
        let column = offset - lines[line].start;
        output_line_start(destination_output_index + line - first) + column
    };
    let new_start = map_position(start, first);
    let new_end = if end == lines[last].end && last + 1 < lines.len() {
        // A whole-line selection ends at the following line's start.
        output_line_start(destination_output_index + count)
    } else {
        map_position(end, last)
    };

    Some(LineMove {
        range: (range_start, range_end),
        replacement,
        selection: (new_start, new_end),
    })
}

fn append_chars(output: &mut String, chars: &[char], start: usize, end: usize) {
    output.extend(chars[start..end].iter());
}

fn lines(chars: &[char]) -> Vec<Line> {
    let mut result = Vec::new();
    let mut start = 0;
    for (index, &character) in chars.iter().enumerate() {
        if character == '\n' {
            result.push(Line {
                start,
                content_end: index,
                end: index + 1,
            });
            start = index + 1;
        }
    }
    result.push(Line {
        start,
        content_end: chars.len(),
        end: chars.len(),
    });
    result
}

fn line_at(lines: &[Line], offset: usize) -> usize {
    lines
        .partition_point(|line| line.start <= offset)
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::{LineDirection, move_lines};

    #[test]
    fn cursor_moves_single_line_up() {
        let result = move_lines("one\ntwo\nthree", (5, 5), LineDirection::Up).unwrap();
        assert_eq!(result.range, (0, 8));
        assert_eq!(result.replacement, "two\none\n");
        assert_eq!(result.selection, (1, 1));
    }

    #[test]
    fn cursor_moves_single_line_down() {
        let result = move_lines("one\ntwo\nthree", (1, 1), LineDirection::Down).unwrap();
        assert_eq!(result.range, (0, 8));
        assert_eq!(result.replacement, "two\none\n");
        assert_eq!(result.selection, (5, 5));
    }

    #[test]
    fn multi_line_selection_moves_down() {
        let result = move_lines("one\ntwo\nthree\nfour", (0, 8), LineDirection::Down).unwrap();
        assert_eq!(result.range, (0, 14));
        assert_eq!(result.replacement, "three\none\ntwo\n");
        assert_eq!(result.selection, (6, 14));
    }

    #[test]
    fn multi_line_partial_selection_preserves_columns() {
        let result = move_lines("one\ntwo\nthree", (1, 7), LineDirection::Down).unwrap();
        assert_eq!(result.replacement, "three\none\ntwo");
        assert_eq!(result.selection, (7, 13));
    }

    #[test]
    fn boundaries_return_none() {
        assert!(move_lines("one\ntwo", (0, 0), LineDirection::Up).is_none());
        assert!(move_lines("one\ntwo", (4, 4), LineDirection::Down).is_none());
    }

    #[test]
    fn final_line_without_newline_moves_both_directions() {
        let up = move_lines("one\ntwo", (5, 5), LineDirection::Up).unwrap();
        assert_eq!(up.replacement, "two\none");
        assert_eq!(up.selection, (1, 1));

        let down = move_lines("one\ntwo", (1, 1), LineDirection::Down).unwrap();
        assert_eq!(down.replacement, "two\none");
        assert_eq!(down.selection, (5, 5));
    }

    #[test]
    fn uses_character_offsets_for_non_ascii_text() {
        let result = move_lines("éé\n猫", (3, 3), LineDirection::Up).unwrap();
        assert_eq!(result.range, (0, 4));
        assert_eq!(result.replacement, "猫\néé");
        assert_eq!(result.selection, (0, 0));
    }
}
