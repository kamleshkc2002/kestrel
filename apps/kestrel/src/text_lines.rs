//! Line editing independent of GTK text widgets.

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

/// One list continuation and the cursor to apply after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListContinuation {
    /// Character range replaced in the original text.
    pub range: (usize, usize),
    /// Text inserted into [`Self::range`].
    pub replacement: String,
    /// Cursor offset in the resulting text.
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Bullet(char),
    Ordered { number: u64, delimiter: char },
}

/// A Markdown list item's prefix; offsets are within its line.
#[derive(Debug, Clone, Copy)]
struct ListItem {
    indent: usize,
    marker: Marker,
    /// End of an ordered item's number.
    number_end: usize,
    task: bool,
    /// Start of the text after the marker and any task box.
    content: usize,
}

/// Continues the Markdown list around `cursor` on a new line.
///
/// A bulleted, numbered, or task item gets a sibling with the same marker
/// (the next number, or an unticked box) and the rest of the line; later
/// numbers in the list shift up by one. A line continuing an item, wrapped
/// or lazy, continues that item's list. An empty item ends the list by
/// clearing its marker. Anything else is `None`, for a plain new line.
pub fn continue_list(text: &str, cursor: usize) -> Option<ListContinuation> {
    let chars: Vec<char> = text.chars().collect();
    let lines = lines(&chars);
    let cursor = cursor.min(chars.len());
    let current = line_at(&lines, cursor);
    let line = &chars[lines[current].start..lines[current].content_end];
    if let Some(item) = list_item(line) {
        if line[item.content..]
            .iter()
            .all(|character| character.is_whitespace())
        {
            return Some(ListContinuation {
                range: (lines[current].start, lines[current].content_end),
                replacement: String::new(),
                cursor: lines[current].start,
            });
        }
        if cursor - lines[current].start < item.content {
            return None;
        }
        return Some(continue_item(
            &chars, &lines, current, current, item, cursor,
        ));
    }
    if is_blank(line) {
        return None;
    }
    let (owner, item) = (0..current)
        .rev()
        .take_while(|&index| !is_blank(line_chars(&chars, lines[index])))
        .find_map(|index| list_item(line_chars(&chars, lines[index])).map(|item| (index, item)))?;
    Some(continue_item(&chars, &lines, owner, current, item, cursor))
}

/// Inserts a sibling of the item on `owner` at `cursor` on `current`, and
/// renumbers the ordered siblings after `current`.
fn continue_item(
    chars: &[char],
    lines: &[Line],
    owner: usize,
    current: usize,
    item: ListItem,
    cursor: usize,
) -> ListContinuation {
    let mut prefix: String = chars[lines[owner].start..lines[owner].start + item.indent]
        .iter()
        .collect();
    match item.marker {
        Marker::Bullet(bullet) => prefix.push(bullet),
        Marker::Ordered { number, delimiter } => {
            prefix.push_str(&(number + 1).to_string());
            prefix.push(delimiter);
        }
    }
    prefix.push(' ');
    if item.task {
        prefix.push_str("[ ] ");
    }

    // Spaces after the cursor would double the one after the new marker.
    let tail = cursor
        + chars[cursor..lines[current].content_end]
            .iter()
            .take_while(|character| **character == ' ')
            .count();
    let renumbered = match item.marker {
        Marker::Ordered { number, delimiter } => {
            later_siblings(chars, lines, current, item.indent, delimiter)
                .into_iter()
                .zip(number + 2..)
                .collect()
        }
        Marker::Bullet(_) => Vec::new(),
    };
    let end = renumbered
        .last()
        .map_or(tail, |((_, number_end), _)| *number_end);

    let mut replacement = format!("\n{prefix}");
    let mut copied = tail;
    for ((number_start, number_end), number) in renumbered {
        replacement.extend(&chars[copied..number_start]);
        replacement.push_str(&number.to_string());
        copied = number_end;
    }
    replacement.extend(&chars[copied..end]);
    ListContinuation {
        range: (cursor, end),
        cursor: cursor + 1 + prefix.chars().count(),
        replacement,
    }
}

/// Number spans of the ordered items after `current` in the same list: same
/// indentation and delimiter, past nested lines, continuations, and blank lines.
fn later_siblings(
    chars: &[char],
    lines: &[Line],
    current: usize,
    indent: usize,
    delimiter: char,
) -> Vec<(usize, usize)> {
    let mut siblings = Vec::new();
    let mut after_blank = false;
    for line in &lines[current + 1..] {
        let text = line_chars(chars, *line);
        if is_blank(text) {
            after_blank = true;
            continue;
        }
        let leading = text
            .iter()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .count();
        match list_item(text) {
            Some(sibling) if sibling.indent == indent => match sibling.marker {
                Marker::Ordered {
                    delimiter: found, ..
                } if found == delimiter => {
                    siblings.push((line.start + indent, line.start + sibling.number_end));
                }
                _ => break,
            },
            Some(_) | None if leading > indent => {}
            None if !after_blank => {}
            _ => break,
        }
        after_blank = false;
    }
    siblings
}

/// Reads a list item prefix: indentation, a `-`, `*`, or `+` bullet or a
/// number of up to nine digits with `.` or `)`, a space or the line's end,
/// and an optional task box.
fn list_item(line: &[char]) -> Option<ListItem> {
    let indent = line
        .iter()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .count();
    let rest = &line[indent..];
    let (marker, marker_len) = match rest.first()? {
        bullet @ ('-' | '*' | '+') => (Marker::Bullet(*bullet), 1),
        _ => {
            let digits = rest
                .iter()
                .take_while(|character| character.is_ascii_digit())
                .count();
            let delimiter = *rest.get(digits)?;
            if !(1..=9).contains(&digits) || !matches!(delimiter, '.' | ')') {
                return None;
            }
            let number = rest[..digits].iter().collect::<String>().parse().ok()?;
            (Marker::Ordered { number, delimiter }, digits + 1)
        }
    };
    let number_end = indent + marker_len - 1;
    let mut content = indent + marker_len;
    match line.get(content) {
        None => {}
        Some(' ' | '\t') => {
            content += line[content..]
                .iter()
                .take_while(|character| matches!(character, ' ' | '\t'))
                .count();
        }
        Some(_) => return None,
    }
    let task = matches!(
        line.get(content..content + 3),
        Some(['[', ' ' | 'x' | 'X', ']'])
    ) && line
        .get(content + 3)
        .is_none_or(|character| matches!(character, ' ' | '\t'));
    if task {
        content += 3 + usize::from(line.get(content + 3).is_some());
    }
    Some(ListItem {
        indent,
        marker,
        number_end,
        task,
        content,
    })
}

fn line_chars(chars: &[char], line: Line) -> &[char] {
    &chars[line.start..line.content_end]
}

fn is_blank(line: &[char]) -> bool {
    line.iter().all(|character| character.is_whitespace())
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
    use super::{LineDirection, continue_list, move_lines};

    /// The text and cursor after Shift+Return at `cursor`, if it continues a list.
    fn continued(text: &str, cursor: usize) -> Option<(String, usize)> {
        let continuation = continue_list(text, cursor)?;
        let chars: Vec<char> = text.chars().collect();
        let mut result: String = chars[..continuation.range.0].iter().collect();
        result.push_str(&continuation.replacement);
        result.extend(&chars[continuation.range.1..]);
        Some((result, continuation.cursor))
    }

    #[test]
    fn bullets_and_tasks_continue_with_their_marker_and_an_unticked_box() {
        assert_eq!(continued("- one", 5), Some(("- one\n- ".into(), 8)));
        assert_eq!(
            continued("  * [x] done", 12),
            Some(("  * [x] done\n  * [ ] ".into(), 21))
        );
        assert_eq!(
            continued("- buy milk", 5),
            Some(("- buy\n- milk".into(), 8)),
            "the rest of the line moves into the new item"
        );
        assert_eq!(continued("- é", 3), Some(("- é\n- ".into(), 6)));
    }

    #[test]
    fn ordered_items_take_the_next_number_and_later_ones_shift() {
        assert_eq!(
            continued("1. a\n2. b\n3. c", 4),
            Some(("1. a\n2. \n3. b\n4. c".into(), 8))
        );
        assert_eq!(
            continued("1) a\n   - sub\n2) b\n\n3) c\n4. other", 4),
            Some(("1) a\n2) \n   - sub\n3) b\n\n4) c\n4. other".into(), 8)),
            "nested lines and blank lines stay in the list; another delimiter ends it"
        );
        assert_eq!(
            continued("9. [ ] last", 11),
            Some(("9. [ ] last\n10. [ ] ".into(), 20))
        );
    }

    #[test]
    fn an_empty_item_ends_the_list() {
        assert_eq!(continued("- one\n- ", 8), Some(("- one\n".into(), 6)));
        assert_eq!(
            continued("- [ ] one\n- [ ]", 15),
            Some(("- [ ] one\n".into(), 10))
        );
        assert_eq!(continued("1. one\n2.", 9), Some(("1. one\n".into(), 7)));
    }

    #[test]
    fn continuation_lines_continue_their_item_without_touching_its_box() {
        assert_eq!(
            continued("- [x] buy milk\nand bread", 24),
            Some(("- [x] buy milk\nand bread\n- [ ] ".into(), 31))
        );
        assert_eq!(
            continued("1. first\n   more\n2. second", 16),
            Some(("1. first\n   more\n2. \n3. second".into(), 20))
        );
    }

    #[test]
    fn text_outside_a_list_gets_a_plain_new_line() {
        for (text, cursor) in [
            ("plain text", 5),
            ("**bold**", 8),
            ("---", 3),
            ("-not a list", 4),
            ("- one\n\nafter", 12),
            ("", 0),
        ] {
            assert_eq!(continue_list(text, cursor), None, "{text:?}");
        }
        assert_eq!(
            continue_list("- one", 1),
            None,
            "a cursor inside the marker splits nothing"
        );
    }

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
