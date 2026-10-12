use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Multi-line text editor with cursor movement, an anchor-based selection,
/// snapshot undo/redo and word-boundary helpers for autocompletion.
#[derive(Debug, Clone, Default)]
pub struct Editor {
    /// Lines of text. Always contains at least one line.
    lines: Vec<String>,
    /// Cursor position: (line index, byte offset within the line).
    pub cursor: (usize, usize),
    /// Selection anchor; the selection spans anchor..cursor in either order.
    anchor: Option<(usize, usize)>,
    undo_stack: Vec<Snapshot>,
    redo_stack: Vec<Snapshot>,
    /// Kind and resulting cursor of the last recorded edit, used to merge
    /// runs of typing / deleting into a single undo step.
    last_edit: Option<(EditKind, (usize, usize))>,
}

#[derive(Debug, Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor: (usize, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

const MAX_UNDO: usize = 500;

impl Editor {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            ..Self::default()
        }
    }

    pub fn from_text(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(|l| l.to_string()).collect()
        };
        Self {
            lines,
            ..Self::default()
        }
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Current line text.
    fn cur_line(&self) -> &String {
        &self.lines[self.cursor.0]
    }

    /// Clamp byte offset to a char boundary within line `li`.
    fn clamp_offset(&self, li: usize, off: usize) -> usize {
        let line = &self.lines[li];
        if off >= line.len() {
            return line.len();
        }
        // snap to char boundary
        let mut o = off;
        while o < line.len() && !line.is_char_boundary(o) {
            o += 1;
        }
        o
    }

    // ---- undo / redo ----

    /// Snapshot the document before an edit. Consecutive inserts (or deletes)
    /// that continue from the previous edit's cursor share one snapshot.
    fn record(&mut self, kind: EditKind) {
        self.redo_stack.clear();
        let continues = kind != EditKind::Other
            && self.anchor.is_none()
            && self.last_edit == Some((kind, self.cursor));
        if !continues {
            self.undo_stack.push(Snapshot {
                lines: self.lines.clone(),
                cursor: self.cursor,
            });
            if self.undo_stack.len() > MAX_UNDO {
                self.undo_stack.remove(0);
            }
        }
    }

    fn finish_edit(&mut self, kind: EditKind) {
        self.last_edit = Some((kind, self.cursor));
    }

    fn restore(&mut self, snap: Snapshot) -> Snapshot {
        let current = Snapshot {
            lines: std::mem::replace(&mut self.lines, snap.lines),
            cursor: self.cursor,
        };
        self.cursor = snap.cursor;
        self.anchor = None;
        self.last_edit = None;
        current
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo_stack.pop() else {
            return false;
        };
        let current = self.restore(snap);
        self.redo_stack.push(current);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo_stack.pop() else {
            return false;
        };
        let current = self.restore(snap);
        self.undo_stack.push(current);
        true
    }

    // ---- selection ----

    /// Anchor a selection at the cursor unless one is already active.
    pub fn begin_selection(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        self.move_doc_end();
    }

    /// Ordered (start, end) of the selection; `None` when empty.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        if a == self.cursor {
            return None;
        }
        Some(if a < self.cursor {
            (a, self.cursor)
        } else {
            (self.cursor, a)
        })
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selection()?;
        if s.0 == e.0 {
            return Some(self.lines[s.0][s.1..e.1].to_string());
        }
        let mut out = self.lines[s.0][s.1..].to_string();
        for li in s.0 + 1..e.0 {
            out.push('\n');
            out.push_str(&self.lines[li]);
        }
        out.push('\n');
        out.push_str(&self.lines[e.0][..e.1]);
        Some(out)
    }

    /// Remove the selected text without touching undo state.
    fn remove_selection_raw(&mut self) -> bool {
        let Some((s, e)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let tail = self.lines[e.0].split_off(e.1);
        self.lines[s.0].truncate(s.1);
        self.lines[s.0].push_str(&tail);
        if e.0 > s.0 {
            self.lines.drain(s.0 + 1..=e.0);
        }
        self.cursor = s;
        self.anchor = None;
        true
    }

    /// Delete the selection as one undo step. Returns false if none was active.
    pub fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            self.anchor = None;
            return false;
        }
        self.record(EditKind::Other);
        self.remove_selection_raw();
        self.finish_edit(EditKind::Other);
        true
    }

    // ---- editing ----

    pub fn insert_char(&mut self, c: char) {
        self.record(EditKind::Insert);
        self.remove_selection_raw();
        let (li, off) = self.cursor;
        self.lines[li].insert(off, c);
        self.cursor.1 = off + c.len_utf8();
        self.finish_edit(EditKind::Insert);
    }

    /// Insert text at cursor, handling embedded newlines.
    pub fn insert_text(&mut self, text: &str) {
        self.record(EditKind::Other);
        self.remove_selection_raw();
        for c in text.chars() {
            if c == '\n' {
                self.newline_raw();
            } else {
                let (li, off) = self.cursor;
                self.lines[li].insert(off, c);
                self.cursor.1 = off + c.len_utf8();
            }
        }
        self.finish_edit(EditKind::Other);
    }

    fn newline_raw(&mut self) {
        let (li, off) = self.cursor;
        let rest = self.lines[li].split_off(off);
        self.lines.insert(li + 1, rest);
        self.cursor = (li + 1, 0);
    }

    pub fn newline(&mut self) {
        self.record(EditKind::Other);
        self.remove_selection_raw();
        self.newline_raw();
        self.finish_edit(EditKind::Other);
    }

    /// Delete char before cursor (backspace). Returns true if anything was deleted.
    pub fn backspace(&mut self) -> bool {
        if self.has_selection() {
            return self.delete_selection();
        }
        self.anchor = None;
        let (li, off) = self.cursor;
        if off == 0 && li == 0 {
            return false;
        }
        self.record(EditKind::Delete);
        if off > 0 {
            let line = &mut self.lines[li];
            let mut cut = off - 1;
            while cut > 0 && !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.replace_range(cut..off, "");
            self.cursor.1 = cut;
        } else {
            let line = self.lines.remove(li);
            let prev_len = self.lines[li - 1].len();
            self.lines[li - 1].push_str(&line);
            self.cursor = (li - 1, prev_len);
        }
        self.finish_edit(EditKind::Delete);
        true
    }

    /// Delete char at cursor (delete key).
    pub fn delete(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        self.anchor = None;
        let (li, off) = self.cursor;
        let line_len = self.lines[li].len();
        if off >= line_len && li + 1 >= self.lines.len() {
            return;
        }
        self.record(EditKind::Delete);
        if off < line_len {
            let line = &mut self.lines[li];
            let mut end = off + 1;
            while end < line.len() && !line.is_char_boundary(end) {
                end += 1;
            }
            line.replace_range(off..end, "");
        } else {
            let next = self.lines.remove(li + 1);
            self.lines[li].push_str(&next);
        }
        self.finish_edit(EditKind::Delete);
    }

    // ---- movement ----

    pub fn move_left(&mut self) {
        let (li, off) = self.cursor;
        if off > 0 {
            self.cursor.1 = {
                let line = &self.lines[li];
                let mut o = off - 1;
                while o > 0 && !line.is_char_boundary(o) {
                    o -= 1;
                }
                o
            };
        } else if li > 0 {
            self.cursor = (li - 1, self.lines[li - 1].len());
        }
    }

    pub fn move_right(&mut self) {
        let (li, off) = self.cursor;
        if off < self.lines[li].len() {
            self.cursor.1 = self.clamp_offset(li, off + 1);
        } else if li + 1 < self.lines.len() {
            self.cursor = (li + 1, 0);
        }
    }

    pub fn move_up(&mut self) -> bool {
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
            self.cursor.1 = self.clamp_offset(self.cursor.0, self.cursor.1);
            true
        } else {
            false
        }
    }

    pub fn move_down(&mut self) -> bool {
        if self.cursor.0 + 1 < self.lines.len() {
            self.cursor.0 += 1;
            self.cursor.1 = self.clamp_offset(self.cursor.0, self.cursor.1);
            true
        } else {
            false
        }
    }

    pub fn move_home(&mut self) {
        self.cursor.1 = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor.1 = self.lines[self.cursor.0].len();
    }

    /// Move to start of document.
    pub fn move_doc_start(&mut self) {
        self.cursor = (0, 0);
    }

    /// Move to end of document.
    pub fn move_doc_end(&mut self) {
        let li = self.lines.len() - 1;
        self.cursor = (li, self.lines[li].len());
    }

    // ---- autocompletion helpers ----

    /// The word (identifier) immediately before the cursor on the current line.
    /// Returns the byte offset where the word starts, plus the word itself.
    pub fn word_before_cursor(&self) -> (usize, String) {
        let line = self.cur_line();
        let off = self.cursor.1;
        let bytes = line.as_bytes();
        let mut start = off;
        while start > 0 {
            let b = bytes[start - 1];
            if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'`' {
                start -= 1;
            } else {
                break;
            }
        }
        (start, line[start..off].to_string())
    }

    /// Text on the current line before the cursor (for statement detection).
    pub fn line_before_cursor(&self) -> &str {
        &self.cur_line()[..self.cursor.1]
    }

    /// Full text before the cursor across all lines (last statement detection).
    pub fn text_before_cursor(&self) -> String {
        let (li, off) = self.cursor;
        let mut text = self.lines[..li].join("\n");
        if li > 0 {
            text.push('\n');
        }
        text.push_str(&self.cur_line()[..off]);
        text
    }

    // ---- rendering ----

    /// Render lines with lightweight SQL syntax highlighting, applying a
    /// horizontal scroll `scroll_x` (in columns) and returning styled lines.
    /// The active selection is drawn with reversed colors.
    pub fn render_lines(&self, scroll_x: usize) -> Vec<Line<'static>> {
        let sel = self.selection();
        let mut in_block_comment = false;
        self.lines
            .iter()
            .enumerate()
            .map(|(li, l)| {
                let mut line = highlight_line(l, scroll_x, in_block_comment);
                in_block_comment = block_comment_open(l, in_block_comment);
                if let Some((s, e)) = sel
                    && li >= s.0
                    && li <= e.0
                {
                    let start_col = if li == s.0 { l[..s.1].chars().count() } else { 0 };
                    let (end_col, through_eol) = if li == e.0 {
                        (l[..e.1].chars().count(), false)
                    } else {
                        (l.chars().count(), true)
                    };
                    line = apply_selection(
                        line,
                        start_col.saturating_sub(scroll_x),
                        end_col.saturating_sub(scroll_x),
                        through_eol,
                    );
                }
                line
            })
            .collect()
    }
}

/// Re-style columns `[start, end)` of `line` (plus one trailing cell when
/// `through_eol`) with REVERSED so the selection is visible on any palette.
fn apply_selection(line: Line<'static>, start: usize, end: usize, through_eol: bool) -> Line<'static> {
    let mut out: Vec<Span> = Vec::new();
    let mut col = 0usize;
    for span in line.spans {
        let text: Vec<char> = span.content.chars().collect();
        let n = text.len();
        let span_start = col;
        let span_end = col + n;
        col = span_end;
        let sel_from = start.max(span_start);
        let sel_to = end.min(span_end);
        if sel_from >= sel_to {
            out.push(span);
            continue;
        }
        let (a, b) = (sel_from - span_start, sel_to - span_start);
        let piece = |r: std::ops::Range<usize>| text[r].iter().collect::<String>();
        if a > 0 {
            out.push(Span::styled(piece(0..a), span.style));
        }
        out.push(Span::styled(
            piece(a..b),
            span.style.add_modifier(Modifier::REVERSED),
        ));
        if b < n {
            out.push(Span::styled(piece(b..n), span.style));
        }
    }
    if through_eol && end >= col && start <= col {
        out.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        ));
    }
    Line::from(out)
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Tokenize a line of SQL into styled spans, skipping `scroll_x` columns.
/// `in_block_comment` indicates whether the line starts inside a /* */ comment.
fn highlight_line(line: &str, scroll_x: usize, in_block_comment: bool) -> Line<'static> {
    let bytes = line.as_bytes();
    let mut spans: Vec<Span> = Vec::new();
    let mut i = 0;
    let mut block_comment = in_block_comment;

    let push = |spans: &mut Vec<Span>, text: &str, style: Style| {
        if !text.is_empty() {
            spans.push(Span::styled(text.to_string(), style));
        }
    };

    let comment_style = Style::default().fg(Color::DarkGray);

    while i < bytes.len() {
        // inside a block comment: consume until */
        if block_comment {
            let mut j = i;
            while j < bytes.len() {
                if bytes[j] == b'*' && j + 1 < bytes.len() && bytes[j + 1] == b'/' {
                    j += 2;
                    block_comment = false;
                    break;
                }
                j += 1;
            }
            push(&mut spans, &line[i..j], comment_style);
            i = j;
            if block_comment {
                break;
            }
            continue;
        }

        let b = bytes[i];

        // line comments: -- or #
        if (b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-')
            || b == b'#'
        {
            push(&mut spans, &line[i..], comment_style);
            break;
        }

        // block comment start: /*
        if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            let mut j = i + 2;
            while j < bytes.len() {
                if bytes[j] == b'*' && j + 1 < bytes.len() && bytes[j + 1] == b'/' {
                    j += 2;
                    break;
                }
                j += 1;
            }
            if j == bytes.len() && !bytes[i..].windows(2).any(|w| w == b"*/") {
                block_comment = true;
            }
            push(&mut spans, &line[i..j], comment_style);
            i = j;
            continue;
        }

        // strings
        if b == b'\'' || b == b'"' || b == b'`' {
            let quote = b;
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == quote {
                    if quote != b'`' && j + 1 < bytes.len() && bytes[j + 1] == quote {
                        j += 2; // escaped quote ('' or "")
                        continue;
                    }
                    j += 1;
                    break;
                }
                j += 1;
            }
            let style = if quote == b'`' {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::LightGreen)
            };
            push(&mut spans, &line[i..j], style);
            i = j;
            continue;
        }
        // numbers
        if b.is_ascii_digit() {
            let mut j = i;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'.') {
                j += 1;
            }
            push(&mut spans, &line[i..j], Style::default().fg(Color::Cyan));
            i = j;
            continue;
        }
        // identifiers / keywords
        if is_ident_char(b) {
            let mut j = i;
            while j < bytes.len() && is_ident_char(bytes[j]) {
                j += 1;
            }
            let word = &line[i..j];
            let style = if is_keyword(word) {
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            push(&mut spans, word, style);
            i = j;
            continue;
        }
        // punctuation / operators
        let mut j = i;
        while j < bytes.len() && !is_ident_char(bytes[j]) {
            if bytes[j] == b'\'' || bytes[j] == b'"' || bytes[j] == b'`' {
                break;
            }
            if bytes[j] == b'-' && j + 1 < bytes.len() && bytes[j + 1] == b'-' {
                break;
            }
            if bytes[j] == b'#' {
                break;
            }
            if bytes[j] == b'/' && j + 1 < bytes.len() && bytes[j + 1] == b'*' {
                break;
            }
            j += 1;
        }
        push(&mut spans, &line[i..j], Style::default().fg(Color::Gray));
        i = j;
    }

    // apply horizontal scroll by trimming leading spans
    if scroll_x == 0 {
        return Line::from(spans);
    }
    let mut remain = scroll_x;
    let mut scrolled: Vec<Span> = Vec::new();
    for span in spans {
        if remain == 0 {
            scrolled.push(span);
            continue;
        }
        let width = span.content.chars().count();
        if width <= remain {
            remain -= width;
        } else {
            let text: String = span.content.chars().skip(remain).collect();
            remain = 0;
            scrolled.push(Span::styled(text, span.style));
        }
    }
    Line::from(scrolled)
}

/// Returns true if, after processing `line`, a block comment is still open.
fn block_comment_open(line: &str, mut in_block: bool) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_string: Option<u8> = None;

    while i < bytes.len() {
        if in_block {
            if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(q) = in_string {
            if bytes[i] == q {
                if q != b'`' && i + 1 < bytes.len() && bytes[i + 1] == q {
                    i += 2;
                } else {
                    in_string = None;
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }
        match bytes[i] {
            b'\'' | b'"' | b'`' => {
                in_string = Some(bytes[i]);
                i += 1;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => break,
            b'#' => break,
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                in_block = true;
                i += 2;
            }
            _ => i += 1,
        }
    }
    in_block
}

// Sorted alphabetically for binary_search; keep it that way when editing.
const KEYWORDS: &[&str] = &[
    "ADD",
    "ALTER",
    "ANALYZE",
    "AND",
    "ASC",
    "AUTO_INCREMENT",
    "AVG",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASE",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "COLUMNS",
    "COMMIT",
    "CONSTRAINT",
    "COUNT",
    "CREATE",
    "CROSS",
    "DATABASE",
    "DATABASES",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DESCRIBE",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "ENGINE",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FOREIGN",
    "FROM",
    "FULL",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "KILL",
    "LEFT",
    "LIKE",
    "LIMIT",
    "LOCK",
    "MAX",
    "MIN",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OPTIMIZE",
    "OR",
    "ORDER",
    "OUTER",
    "PARTITION",
    "PRIMARY",
    "PROCESSLIST",
    "REFERENCES",
    "REPLACE",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "SCHEMA",
    "SELECT",
    "SET",
    "SHOW",
    "START",
    "STATUS",
    "SUM",
    "TABLE",
    "TABLES",
    "THEN",
    "TRANSACTION",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UNLOCK",
    "UPDATE",
    "USE",
    "VALUES",
    "VARIABLES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WITH",
];

pub fn is_keyword(word: &str) -> bool {
    KEYWORDS
        .binary_search(&word.to_uppercase().as_str())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_editing() {
        let mut ed = Editor::new();
        ed.insert_text("SELECT 1");
        assert_eq!(ed.text(), "SELECT 1");
        ed.newline();
        ed.insert_text("FROM t");
        assert_eq!(ed.text(), "SELECT 1\nFROM t");
        assert_eq!(ed.cursor, (1, 6));
    }

    #[test]
    fn backspace_joins_lines() {
        let mut ed = Editor::from_text("ab\ncd");
        ed.move_doc_end(); // end of "cd"
        assert_eq!(ed.cursor, (1, 2));
        ed.backspace(); // delete 'd'
        assert_eq!(ed.text(), "ab\nc");
        ed.backspace(); // delete 'c'
        ed.backspace(); // join: "ab"
        assert_eq!(ed.text(), "ab");
        assert_eq!(ed.cursor, (1 - 1, 2));
    }

    #[test]
    fn delete_at_cursor_merges_next_line() {
        let mut ed = Editor::from_text("ab\ncd");
        ed.move_end();
        ed.delete();
        assert_eq!(ed.text(), "abcd");
    }

    #[test]
    fn movement_clamps_offset() {
        let mut ed = Editor::from_text("héllo");
        ed.move_end();
        assert_eq!(ed.cursor.1, "héllo".len());
        ed.move_left();
        assert_eq!(ed.cur_line()[..ed.cursor.1].to_string(), "héll");
        // Step back over the multibyte é without ever landing mid-char.
        ed.move_left();
        ed.move_left();
        ed.move_left();
        assert_eq!(ed.cursor.1, 1);
        ed.move_home();
        ed.move_left();
        assert_eq!(ed.cursor, (0, 0));
        ed.move_right();
        ed.move_right();
        ed.move_down();
        assert_eq!(ed.cursor.0, 0);
    }

    #[test]
    fn word_before_cursor() {
        let mut ed = Editor::from_text("SELECT id, na");
        ed.move_end();
        assert_eq!(ed.word_before_cursor(), (11, "na".to_string()));
        let mut ed = Editor::from_text("SELECT u.");
        ed.move_end();
        assert_eq!(ed.word_before_cursor(), (7, "u.".to_string()));
        let mut ed = Editor::from_text("SELECT * FROM users WHERE ");
        ed.move_end();
        assert_eq!(ed.word_before_cursor().1, "");
    }

    #[test]
    fn last_statement_and_line() {
        let mut ed = Editor::from_text("SELECT 1;\nSELECT * F");
        ed.move_doc_end();
        assert!(ed.text_before_cursor().contains("SELECT 1;\n"));
        assert!(ed.text_before_cursor().ends_with("SELECT * F"));
    }

    #[test]
    fn highlight_produces_spans() {
        let ed = Editor::from_text("SELECT `x` FROM t -- done");
        let lines = ed.render_lines(0);
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert!(spans.iter().any(|s| s.content == "SELECT"));
        assert!(spans.iter().any(|s| s.content.contains("done")));
    }

    #[test]
    fn highlight_respects_scroll() {
        let ed = Editor::from_text("SELECT id");
        let lines = ed.render_lines(7); // skip "SELECT "
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(text, "id");
    }

    #[test]
    fn highlight_hash_comment() {
        let ed = Editor::from_text("SELECT 1 # comment");
        let lines = ed.render_lines(0);
        let comment_style = Style::default().fg(Color::DarkGray);
        assert!(lines[0].spans.iter().any(|s| s.content.contains("# comment") && s.style == comment_style));
    }

    #[test]
    fn highlight_block_comment_single_line() {
        let ed = Editor::from_text("SELECT /* inline */ 1");
        let lines = ed.render_lines(0);
        let comment_style = Style::default().fg(Color::DarkGray);
        assert!(lines[0].spans.iter().any(|s| s.content.contains("/* inline */") && s.style == comment_style));
    }

    #[test]
    fn undo_coalesces_typing_and_redo_restores() {
        let mut ed = Editor::new();
        for c in "SELECT".chars() {
            ed.insert_char(c);
        }
        ed.newline();
        ed.insert_text("1");
        assert_eq!(ed.text(), "SELECT\n1");
        assert!(ed.undo()); // insert_text
        assert_eq!(ed.text(), "SELECT\n");
        assert!(ed.undo()); // newline
        assert_eq!(ed.text(), "SELECT");
        assert!(ed.undo()); // the whole typing run
        assert_eq!(ed.text(), "");
        assert!(!ed.undo());
        assert!(ed.redo());
        assert_eq!(ed.text(), "SELECT");
        assert_eq!(ed.cursor, (0, 6));
        ed.insert_char('!'); // new edit drops the redo stack
        assert!(!ed.redo());
    }

    #[test]
    fn undo_coalesces_backspace_run_but_not_after_move() {
        let mut ed = Editor::from_text("abcdef");
        ed.move_end();
        ed.backspace();
        ed.backspace();
        ed.move_left();
        ed.backspace();
        assert_eq!(ed.text(), "abd");
        ed.undo();
        assert_eq!(ed.text(), "abcd");
        ed.undo();
        assert_eq!(ed.text(), "abcdef");
    }

    #[test]
    fn selection_text_delete_and_replace() {
        let mut ed = Editor::from_text("SELECT 1\nFROM t\nWHERE x");
        assert!(!ed.has_selection());
        ed.cursor = (0, 7);
        ed.begin_selection();
        ed.cursor = (2, 5);
        assert_eq!(ed.selected_text().as_deref(), Some("1\nFROM t\nWHERE"));
        assert!(ed.delete_selection());
        assert_eq!(ed.text(), "SELECT  x");
        assert_eq!(ed.cursor, (0, 7));
        assert!(!ed.has_selection());
        ed.undo();
        assert_eq!(ed.text(), "SELECT 1\nFROM t\nWHERE x");

        // typing over a selection replaces it in one undo step
        ed.cursor = (0, 0);
        ed.begin_selection();
        ed.cursor = (0, 6);
        ed.insert_char('X');
        assert_eq!(ed.text(), "X 1\nFROM t\nWHERE x");
        ed.undo();
        assert_eq!(ed.text(), "SELECT 1\nFROM t\nWHERE x");

        // backspace with a selection only removes the selection
        ed.cursor = (1, 0);
        ed.begin_selection();
        ed.cursor = (1, 4);
        assert!(ed.backspace());
        assert_eq!(ed.text(), "SELECT 1\n t\nWHERE x");
    }

    #[test]
    fn select_all_and_reverse_selection() {
        let mut ed = Editor::from_text("ab\ncd");
        ed.select_all();
        assert_eq!(ed.selected_text().as_deref(), Some("ab\ncd"));
        ed.clear_selection();
        ed.cursor = (1, 1);
        ed.begin_selection();
        ed.cursor = (0, 1);
        assert_eq!(ed.selection(), Some(((0, 1), (1, 1))));
        assert_eq!(ed.selected_text().as_deref(), Some("b\nc"));
    }

    #[test]
    fn render_marks_selection_reversed() {
        let mut ed = Editor::from_text("SELECT id\nFROM t");
        ed.cursor = (0, 7);
        ed.begin_selection();
        ed.cursor = (1, 4);
        let lines = ed.render_lines(0);
        let rev = |s: &Span| s.style.add_modifier.contains(Modifier::REVERSED);
        let sel0: String = lines[0].spans.iter().filter(|s| rev(s)).map(|s| s.content.to_string()).collect();
        assert_eq!(sel0, "id ");
        let sel1: String = lines[1].spans.iter().filter(|s| rev(s)).map(|s| s.content.to_string()).collect();
        assert_eq!(sel1, "FROM");
        let plain: String = lines[0].spans.iter().filter(|s| !rev(s)).map(|s| s.content.to_string()).collect();
        assert_eq!(plain, "SELECT ");
        // scrolled view keeps the highlight on the right characters
        let lines = ed.render_lines(8);
        let sel0: String = lines[0].spans.iter().filter(|s| rev(s)).map(|s| s.content.to_string()).collect();
        assert_eq!(sel0, "d ");
    }

    #[test]
    fn highlight_multiline_block_comment() {
        let ed = Editor::from_text("SELECT /* start\nmiddle\nend */ 1");
        let lines = ed.render_lines(0);
        let comment_style = Style::default().fg(Color::DarkGray);
        // Line 0: "/* start" should be a comment
        assert!(lines[0].spans.iter().any(|s| s.content.contains("/* start") && s.style == comment_style));
        // Line 1: "middle" should be entirely a comment
        assert!(lines[1].spans.iter().all(|s| s.style == comment_style));
        // Line 2: "end */" should be a comment, then " 1" should not
        assert!(lines[2].spans.iter().any(|s| s.content.contains("end */") && s.style == comment_style));
    }
}
