use crate::editor::Editor;
use crate::schema::SchemaCache;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionKind {
    Keyword,
    Table,
    Column,
    Database,
    Function,
}

impl SuggestionKind {
    pub fn label(&self) -> &'static str {
        match self {
            SuggestionKind::Keyword => "kw",
            SuggestionKind::Table => "tbl",
            SuggestionKind::Column => "col",
            SuggestionKind::Database => "db",
            SuggestionKind::Function => "fn",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// The full replacement text for the current word (no qualifier part).
    pub label: String,
    pub kind: SuggestionKind,
}

/// Context-aware SQL autocompletion over the editor buffer and cached schema.
#[derive(Debug, Default)]
pub struct Completer {
    schema: SchemaCache,
}

const MAX_SUGGESTIONS: usize = 50;

/// Words after which a table name is expected.
const TABLE_CONTEXTS: &[&str] = &["FROM", "JOIN", "INTO", "UPDATE", "TABLE"];
/// Words after which a database name is expected.
const DATABASE_CONTEXTS: &[&str] = &["USE", "DATABASE"];

/// SQL keywords offered as plain suggestions.
const SUGGEST_KEYWORDS: &[&str] = &[
    "SELECT",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "CREATE TABLE",
    "DROP TABLE",
    "TRUNCATE TABLE",
    "ALTER TABLE",
    "SHOW DATABASES",
    "SHOW TABLES",
    "SHOW COLUMNS FROM",
    "DESCRIBE",
    "EXPLAIN",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "INNER JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "CROSS JOIN",
    "JOIN",
    "ON",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS NOT NULL",
    "IS NULL",
    "IN",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "DISTINCT",
    "UNION ALL",
    "CASE WHEN",
    "COUNT(*)",
    "SUM(",
    "AVG(",
    "MIN(",
    "MAX(",
    "SET",
    "VALUES",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "USE",
    "ASC",
    "DESC",
    "IF EXISTS",
    "IF NOT EXISTS",
    "PRIMARY KEY",
    "AUTO_INCREMENT",
    "DEFAULT",
    "NOW()",
];

/// Multi-char snippets expanded when the selected keyword starts with the trigger.
const SNIPPETS: &[(&str, &str)] = &[
    ("SELECT", "SELECT * FROM "),
    ("INSERT", "INSERT INTO  () VALUES ()"),
    ("UPDATE", "UPDATE  SET  WHERE "),
    ("DELETE", "DELETE FROM  WHERE "),
    ("JOIN", "INNER JOIN  ON "),
];

/// Tokenize SQL-ish text into uppercase words and punctuation.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | ';'))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_uppercase())
        .collect()
}

/// Strip backquotes and an optional schema qualifier ("db"."tbl" -> "tbl").
fn unqualified(ident: &str) -> String {
    let id = ident.trim_matches('`');
    match id.rsplit_once('.') {
        Some((_, tbl)) => tbl.trim_matches('`').to_string(),
        None => id.to_string(),
    }
}

/// Build alias -> table mapping from the current statement's FROM/JOIN clauses.
/// Also maps each table name to itself so "tbl.col" works without an alias.
fn alias_map(statement: &str) -> HashMap<String, String> {
    let tokens = tokenize(statement);
    let mut map = HashMap::new();
    let is_reserved = |t: &str| {
        matches!(
            t,
            "ON" | "WHERE"
                | "SET"
                | "VALUES"
                | "GROUP"
                | "ORDER"
                | "LIMIT"
                | "LEFT"
                | "RIGHT"
                | "INNER"
                | "OUTER"
                | "CROSS"
                | "JOIN"
                | "UNION"
                | "HAVING"
                | "AS"
                | "SELECT"
                | "INSERT"
                | "UPDATE"
                | "DELETE"
                | "INTO"
                | "FROM"
                | "USING"
                | "RETURNING"
        )
    };
    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i].as_str();
        if matches!(tok, "FROM" | "JOIN" | "UPDATE" | "INTO")
            && let Some(table) = tokens.get(i + 1)
            && table
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '`')
        {
            let table = unqualified(table);
            map.insert(table.to_lowercase(), table.to_lowercase());
            let mut j = i + 2;
            if tokens.get(j).map(|t| t.as_str()) == Some("AS") {
                j += 1;
            }
            if let Some(alias) = tokens.get(j) {
                let alias = alias.trim_matches('`').to_string();
                if !alias.is_empty()
                    && !is_reserved(&alias)
                    && alias
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic())
                {
                    map.insert(alias.to_lowercase(), table.to_lowercase());
                }
            }
        }
        i += 1;
    }
    map
}

/// Extract the full statement containing the cursor (between surrounding ';'),
/// so FROM clauses typed after the cursor still provide table context.
fn statement_at(text: &str, pos: usize) -> &str {
    let start = text[..pos].rfind(';').map(|p| p + 1).unwrap_or(0);
    let end = text[pos..].find(';').map(|p| pos + p).unwrap_or(text.len());
    text[start..end].trim()
}

impl Completer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update_schema(&mut self, schema: SchemaCache) {
        self.schema = schema;
    }

    pub fn schema(&self) -> &SchemaCache {
        &self.schema
    }

    pub fn schema_mut(&mut self) -> &mut SchemaCache {
        &mut self.schema
    }

    /// Compute completion suggestions for the current cursor position.
    pub fn compute(&self, editor: &Editor) -> Vec<Suggestion> {
        let (word_start, word) = editor.word_before_cursor();
        let line_before = editor.line_before_cursor();
        let before_word = line_before[..word_start].trim_end();
        let text_before = editor.text_before_cursor();
        let text = editor.text();
        let statement = statement_at(&text, text_before.len());
        let aliases = alias_map(statement);

        // Determine prefix and optional table qualifier from the word ("u.na" -> ("u", "na")).
        let (qualifier, prefix) = match word.rsplit_once('.') {
            Some((q, p)) => (Some(q.trim_matches('`').to_lowercase()), p.to_string()),
            None => (None, word.trim_matches('`').to_string()),
        };

        // Last word before the completion point, uppercased, punctuation-stripped.
        let prev = tokenize(before_word).pop().unwrap_or_default();
        let lower_prefix = prefix.to_lowercase();

        let mut out: Vec<Suggestion> = Vec::new();

        if let Some(qual) = qualifier {
            // "alias.col|" or "table.col|"
            if let Some(table) = aliases.get(&qual) {
                if let Some(cols) = self.schema.columns_of(table) {
                    for c in cols {
                        if c.to_lowercase().starts_with(&lower_prefix) {
                            out.push(Suggestion {
                                label: c.clone(),
                                kind: SuggestionKind::Column,
                            });
                        }
                    }
                }
            } else if qual == self.schema.database.to_lowercase() {
                // "dbname.tbl|" — schema-qualified table
                for t in &self.schema.tables {
                    if t.to_lowercase().starts_with(&lower_prefix) {
                        out.push(Suggestion {
                            label: t.clone(),
                            kind: SuggestionKind::Table,
                        });
                    }
                }
            }
        } else if DATABASE_CONTEXTS.contains(&prev.as_str())
            || statement.trim_start().to_uppercase().starts_with("USE")
        {
            for db in &self.schema.databases {
                if db.to_lowercase().starts_with(&lower_prefix) {
                    out.push(Suggestion {
                        label: db.clone(),
                        kind: SuggestionKind::Database,
                    });
                }
            }
        } else if TABLE_CONTEXTS.contains(&prev.as_str()) {
            for t in &self.schema.tables {
                if t.to_lowercase().starts_with(&lower_prefix) {
                    out.push(Suggestion {
                        label: t.clone(),
                        kind: SuggestionKind::Table,
                    });
                }
            }
            if prev == "INTO" && lower_prefix.is_empty() {
                // INSERT INTO <db>.<tbl> — also allow database names to start qualification
                for db in &self.schema.databases {
                    out.push(Suggestion {
                        label: db.clone(),
                        kind: SuggestionKind::Database,
                    });
                }
            }
        } else {
            // Generic context: keywords, tables, columns (of statement tables when known).
            for k in SUGGEST_KEYWORDS {
                if k.to_lowercase().starts_with(&lower_prefix) {
                    out.push(Suggestion {
                        label: (*k).to_string(),
                        kind: SuggestionKind::Keyword,
                    });
                }
            }
            // Prefer tables already used in the statement when suggesting columns.
            let stmt_tables: Vec<String> = aliases.values().cloned().collect();
            let mut tables: Vec<&String> = self.schema.tables.iter().collect();
            tables.sort();
            if stmt_tables.is_empty() {
                for t in tables {
                    if t.to_lowercase().starts_with(&lower_prefix) {
                        out.push(Suggestion {
                            label: t.clone(),
                            kind: SuggestionKind::Table,
                        });
                    }
                }
                for c in &self.schema.all_columns {
                    if c.to_lowercase().starts_with(&lower_prefix) {
                        out.push(Suggestion {
                            label: c.clone(),
                            kind: SuggestionKind::Column,
                        });
                    }
                }
            } else {
                let mut seen_cols = std::collections::HashSet::new();
                for t in &stmt_tables {
                    if let Some(cols) = self.schema.columns_of(t) {
                        for c in cols {
                            if c.to_lowercase().starts_with(&lower_prefix)
                                && seen_cols.insert(c.clone())
                            {
                                out.push(Suggestion {
                                    label: c.clone(),
                                    kind: SuggestionKind::Column,
                                });
                            }
                        }
                    }
                }
                tables.sort();
                for t in tables {
                    if t.to_lowercase().starts_with(&lower_prefix) {
                        out.push(Suggestion {
                            label: t.clone(),
                            kind: SuggestionKind::Table,
                        });
                    }
                }
            }
            // Functions are useful in any expression context.
            for f in [
                "COUNT", "SUM", "AVG", "MIN", "MAX", "NOW", "LENGTH", "CONCAT", "UPPER", "LOWER",
            ] {
                if f.to_lowercase().starts_with(&lower_prefix) && lower_prefix.len() >= 2 {
                    out.push(Suggestion {
                        label: format!("{f}("),
                        kind: SuggestionKind::Function,
                    });
                }
            }
        }

        // Sort: shorter labels first, then alphabetically; dedupe.
        out.sort_by(|a, b| {
            a.label
                .len()
                .cmp(&b.label.len())
                .then(a.label.cmp(&b.label))
        });
        out.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
        out.truncate(MAX_SUGGESTIONS);
        out
    }

    /// Apply a suggestion to the editor, replacing the current word.
    /// Keywords with a matching snippet trigger expand into the full snippet.
    pub fn apply(editor: &mut Editor, suggestion: &Suggestion) {
        let (_, word) = editor.word_before_cursor();
        // Keep an existing qualifier ("u.col") when replacing.
        let qualifier = match word.rsplit_once('.') {
            Some((q, _)) => format!("{q}."),
            None => String::new(),
        };
        let replacement = match suggestion.kind {
            SuggestionKind::Keyword => {
                let first_word = suggestion.label.split(' ').next().unwrap_or("");
                match SNIPPETS.iter().find(|(t, _)| *t == first_word) {
                    Some((_, expansion)) => expansion.to_string(),
                    None => suggestion.label.clone(),
                }
            }
            _ => format!("{qualifier}{}", suggestion.label),
        };
        for _ in 0..word.chars().count() {
            editor.backspace();
        }
        editor.insert_text(&replacement);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SchemaCache;

    fn completer() -> Completer {
        let schema = SchemaCache::test_new(
            "shop",
            &[
                ("users", &["id", "name", "email", "created_at"]),
                ("orders", &["id", "user_id", "amount", "status"]),
            ],
            &["information_schema", "mysql", "shop"],
        );
        let mut c = Completer::new();
        c.update_schema(schema);
        c
    }

    fn editor_at(text: &str) -> Editor {
        let mut e = Editor::from_text(text);
        e.move_doc_end();
        e
    }

    /// Build an editor where '§' marks the cursor position within `text`.
    fn editor_marked(text: &str) -> Editor {
        let (before, after) = text
            .split_once('§')
            .expect("test text needs a § cursor marker");
        let mut e = Editor::from_text(&format!("{before}{after}"));
        let lines: Vec<&str> = before.split('\n').collect();
        e.cursor = (lines.len() - 1, lines.last().unwrap().len());
        e
    }

    fn labels(s: &[Suggestion]) -> Vec<String> {
        s.iter().map(|s| s.label.clone()).collect()
    }

    #[test]
    fn keyword_completion_after_empty_prefix() {
        let e = editor_at("SEL");
        let s = completer().compute(&e);
        assert!(labels(&s).contains(&"SELECT".to_string()));
    }

    #[test]
    fn snippet_expansion() {
        let e = editor_at("SELECT * FROM users JOIN ord");
        let s = completer().compute(&e);
        assert!(labels(&s).contains(&"orders".to_string()));
        assert!(!labels(&s).iter().any(|l| l == "users"));
    }

    #[test]
    fn keyword_snippet_expanded_on_apply() {
        let mut e = editor_at("INS");
        let sugg = Suggestion {
            label: "INSERT INTO".to_string(),
            kind: SuggestionKind::Keyword,
        };
        Completer::apply(&mut e, &sugg);
        assert_eq!(e.text(), "INSERT INTO  () VALUES ()");

        let mut e = editor_at("SELECT * FROM users JO");
        let sugg = Suggestion {
            label: "JOIN".to_string(),
            kind: SuggestionKind::Keyword,
        };
        Completer::apply(&mut e, &sugg);
        assert_eq!(e.text(), "SELECT * FROM users INNER JOIN  ON ");
    }

    #[test]
    fn table_context_after_from() {
        let e = editor_at("SELECT * FROM us");
        let s = completer().compute(&e);
        assert_eq!(labels(&s), vec!["users".to_string()]);
    }

    #[test]
    fn table_context_empty_prefix_lists_all_tables() {
        let e = editor_at("SELECT * FROM ");
        let s = completer().compute(&e);
        assert!(labels(&s).contains(&"users".to_string()));
        assert!(labels(&s).contains(&"orders".to_string()));
    }

    #[test]
    fn database_context_after_use() {
        let e = editor_at("USE sh");
        let s = completer().compute(&e);
        assert_eq!(labels(&s), vec!["shop".to_string()]);
    }

    #[test]
    fn column_context_via_alias_dot() {
        let e = editor_marked("SELECT u.na§ FROM users u");
        // "u." with empty prefix... actually prefix is "na" here; use full "u." below
        let s = completer().compute(&e);
        assert_eq!(labels(&s), vec!["name".to_string()]);
    }

    #[test]
    fn column_completion_with_alias_prefix() {
        let e = editor_marked("SELECT u.§ FROM users u");
        let s = completer().compute(&e);
        // "u." with empty prefix -> all user columns
        let l = labels(&s);
        assert!(l.contains(&"id".to_string()));
        assert!(l.contains(&"name".to_string()));
        assert!(!l.contains(&"amount".to_string()));
    }

    #[test]
    fn column_completion_via_table_name_dot() {
        let e = editor_marked("SELECT orders.am§ FROM orders");
        let s = completer().compute(&e);
        assert_eq!(labels(&s), vec!["amount".to_string()]);
    }

    #[test]
    fn cross_table_columns_without_alias() {
        // Statement references both tables via FROM/JOIN: columns from both, no dupes.
        let e = editor_at("SELECT id FROM users u JOIN orders o ON u.id = o.user_id WHERE st");
        let s = completer().compute(&e);
        assert!(labels(&s).contains(&"status".to_string()));
    }

    #[test]
    fn statement_scoped_after_semicolon() {
        // Alias from a previous statement must not leak into the next one:
        // only columns of tables referenced by the current statement appear.
        let e = editor_at("SELECT u.id FROM users u; SELECT st");
        let s = completer().compute(&e);
        assert_eq!(labels(&s), vec!["status".to_string()]);
    }

    #[test]
    fn apply_replaces_word() {
        let mut e = editor_at("SELECT * FROM us");
        let sugg = Suggestion {
            label: "users".to_string(),
            kind: SuggestionKind::Table,
        };
        Completer::apply(&mut e, &sugg);
        assert_eq!(e.text(), "SELECT * FROM users");
        assert_eq!(e.cursor.1, "SELECT * FROM users".len());
    }

    #[test]
    fn apply_keeps_qualifier() {
        let mut e = editor_marked("SELECT u.na§ FROM users u");
        let sugg = Suggestion {
            label: "name".to_string(),
            kind: SuggestionKind::Column,
        };
        Completer::apply(&mut e, &sugg);
        assert_eq!(e.text(), "SELECT u.name FROM users u");
    }

    #[test]
    fn insert_into_offers_databases_for_qualification() {
        let e = editor_at("INSERT INTO ");
        let s = completer().compute(&e);
        let l = labels(&s);
        assert!(l.contains(&"users".to_string()));
        assert!(l.contains(&"shop".to_string()));
    }

    #[test]
    fn max_suggestions_cap() {
        let schema = SchemaCache::test_new(
            "db",
            &[("t1", &["c1", "c2", "c3"]), ("t2", &["c1", "c4", "c5"])],
            &[],
        );
        let mut c = Completer::new();
        c.update_schema(schema);
        let e = editor_at("SELECT c");
        let s = c.compute(&e);
        assert!(s.len() <= MAX_SUGGESTIONS);
    }
}
