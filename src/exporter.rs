use crate::db::{QueryResult, value_to_display};
use mysql::Value;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Tsv,
    Json,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 3] = [ExportFormat::Csv, ExportFormat::Tsv, ExportFormat::Json];

    pub fn label(&self) -> &'static str {
        match self {
            ExportFormat::Csv => "CSV",
            ExportFormat::Tsv => "TSV",
            ExportFormat::Json => "JSON",
        }
    }

    pub fn default_ext(&self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Tsv => "tsv",
            ExportFormat::Json => "json",
        }
    }
}

/// Expand a leading "~" to the user's home directory.
pub fn expand_path(path: &str) -> PathBuf {
    if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// Append the format's default extension if the path has none.
pub fn with_default_ext(path: &Path, format: ExportFormat) -> PathBuf {
    if path.extension().is_none() {
        let mut s = path.as_os_str().to_os_string();
        s.push(".");
        s.push(format.default_ext());
        PathBuf::from(s)
    } else {
        path.to_path_buf()
    }
}

fn csv_value(v: &Value) -> String {
    match v {
        Value::NULL => String::new(),
        other => value_to_display(other),
    }
}

fn json_value(v: &Value) -> serde_json::Value {
    match v {
        Value::NULL => serde_json::Value::Null,
        Value::Int(i) => (*i).into(),
        Value::UInt(u) => (*u).into(),
        Value::Float(f) => (*f).into(),
        Value::Double(d) => (*d).into(),
        other => value_to_display(other).into(),
    }
}

/// Incremental writer for paginated exports. Writes the file header on open,
/// then accepts batches of rows via `export_page`, and finalizes on `finish`.
pub struct PageWriter<W: Write> {
    writer: W,
    format: ExportFormat,
    path: PathBuf,
    rows_written: usize,
}

impl PageWriter<File> {
    /// Create the output file and write the format-specific header.
    pub fn open(path: PathBuf, format: ExportFormat, columns: &[String]) -> Result<Self, String> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
            && !dir.is_dir()
        {
            return Err(format!("directory does not exist: {}", dir.display()));
        }
        let file =
            File::create(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let mut pw = PageWriter {
            writer: file,
            format,
            path,
            rows_written: 0,
        };
        pw.write_header(columns)?;
        Ok(pw)
    }
}

impl<W: Write> PageWriter<W> {
    fn write_header(&mut self, columns: &[String]) -> Result<(), String> {
        match self.format {
            ExportFormat::Csv | ExportFormat::Tsv => {
                let delim = if self.format == ExportFormat::Csv {
                    b','
                } else {
                    b'\t'
                };
                let mut w = csv::WriterBuilder::new()
                    .delimiter(delim)
                    .from_writer(Vec::new());
                w.write_record(columns)
                    .map_err(|e| format!("write failed: {e}"))?;
                let bytes = w.into_inner().map_err(|e| format!("write failed: {e}"))?;
                self.writer
                    .write_all(&bytes)
                    .map_err(|e| format!("write failed: {e}"))?;
            }
            ExportFormat::Json => {
                self.writer
                    .write_all(b"[\n")
                    .map_err(|e| format!("write failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// Append a batch of rows to the output file.
    pub fn export_page(&mut self, columns: &[String], rows: &[Vec<Value>]) -> Result<(), String> {
        match self.format {
            ExportFormat::Csv | ExportFormat::Tsv => {
                let delim = if self.format == ExportFormat::Csv {
                    b','
                } else {
                    b'\t'
                };
                let mut buf: Vec<u8> = Vec::new();
                {
                    let mut w = csv::WriterBuilder::new()
                        .delimiter(delim)
                        .has_headers(false)
                        .from_writer(&mut buf);
                    for row in rows {
                        let record: Vec<String> = row.iter().map(csv_value).collect();
                        w.write_record(&record)
                            .map_err(|e| format!("write failed: {e}"))?;
                    }
                    w.flush().map_err(|e| format!("write failed: {e}"))?;
                }
                self.writer
                    .write_all(&buf)
                    .map_err(|e| format!("write failed: {e}"))?;
            }
            ExportFormat::Json => {
                for row in rows {
                    if self.rows_written > 0 {
                        self.writer
                            .write_all(b",\n")
                            .map_err(|e| format!("write failed: {e}"))?;
                    }
                    let mut obj = serde_json::Map::new();
                    for (col, val) in columns.iter().zip(row.iter()) {
                        obj.insert(col.clone(), json_value(val));
                    }
                    serde_json::to_writer(&mut self.writer, &serde_json::Value::Object(obj))
                        .map_err(|e| format!("write failed: {e}"))?;
                    self.rows_written += 1;
                }
                return Ok(());
            }
        }
        self.rows_written += rows.len();
        Ok(())
    }

    /// Finalize the file and return the final path and total row count.
    pub fn finish(mut self) -> Result<(PathBuf, usize), String> {
        if self.format == ExportFormat::Json {
            self.writer
                .write_all(b"\n]\n")
                .map_err(|e| format!("write failed: {e}"))?;
        }
        self.writer
            .flush()
            .map_err(|e| format!("write failed: {e}"))?;
        Ok((self.path, self.rows_written))
    }
}

/// Export a query result to `path`. `~` is expanded; a default extension is
/// appended when the path has none. Returns the final path and row count.
pub fn export(
    result: &QueryResult,
    path: &str,
    format: ExportFormat,
) -> Result<(PathBuf, usize), String> {
    let final_path = with_default_ext(&expand_path(path), format);
    let mut writer = PageWriter::open(final_path.clone(), format, &result.columns)?;
    writer.export_page(&result.columns, &result.rows)?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sample() -> QueryResult {
        QueryResult {
            columns: vec!["id".into(), "name".into()],
            rows: vec![
                vec![Value::Int(1), Value::Bytes(b"alice".to_vec())],
                vec![Value::Int(2), Value::NULL],
                vec![Value::Int(3), Value::Bytes(b"a,b \"q\"".to_vec())],
            ],
            truncated: false,
            elapsed: std::time::Duration::ZERO,
            source: None,
            has_more: false,
        }
    }

    #[test]
    fn export_csv() {
        let dir = std::env::temp_dir().join("mysql_tui_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.csv");
        let (final_path, n) = export(&sample(), path.to_str().unwrap(), ExportFormat::Csv).unwrap();
        assert_eq!(n, 3);
        let text = fs::read_to_string(&final_path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "id,name");
        assert_eq!(lines[1], "1,alice");
        assert_eq!(lines[2], "2,");
        assert_eq!(lines[3], "3,\"a,b \"\"q\"\"\"");
    }

    #[test]
    fn export_tsv() {
        let dir = std::env::temp_dir().join("mysql_tui_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out_tsv");
        let (final_path, n) = export(&sample(), path.to_str().unwrap(), ExportFormat::Tsv).unwrap();
        assert_eq!(n, 3);
        assert_eq!(final_path.extension().unwrap(), "tsv");
        let text = fs::read_to_string(&final_path).unwrap();
        assert!(text.starts_with("id\tname\n"));
        assert!(text.contains("a,b \"q\"\n") || text.contains("\"a,b \"\"q\"\"\""));
    }

    #[test]
    fn export_json_types() {
        let dir = std::env::temp_dir().join("mysql_tui_test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.json");
        let (_, n) = export(&sample(), path.to_str().unwrap(), ExportFormat::Json).unwrap();
        assert_eq!(n, 3);
        let text = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["id"], serde_json::json!(1));
        assert_eq!(arr[0]["name"], serde_json::json!("alice"));
        assert!(arr[1]["name"].is_null());
        assert_eq!(arr[2]["name"], serde_json::json!("a,b \"q\""));
    }

    #[test]
    fn expand_home() {
        let p = expand_path("~/x.csv");
        assert!(p.to_str().unwrap().contains('/'));
        assert!(!p.to_str().unwrap().starts_with("~"));
        assert_eq!(expand_path("/tmp/x.csv"), PathBuf::from("/tmp/x.csv"));
        assert_eq!(expand_path("relative.csv"), PathBuf::from("relative.csv"));
    }

    #[test]
    fn default_extension_appended() {
        let dir = std::env::temp_dir().join("mysql_tui_test");
        fs::create_dir_all(&dir).unwrap();
        let base = dir.join("noext");
        let (final_path, _) = export(&sample(), base.to_str().unwrap(), ExportFormat::Csv).unwrap();
        assert_eq!(final_path, dir.join("noext.csv"));
        let _ = fs::remove_file(&final_path);
    }

    #[test]
    fn export_missing_directory_fails() {
        let err = export(&sample(), "/nonexistent-dir-xyz/out.csv", ExportFormat::Csv).unwrap_err();
        assert!(err.contains("directory does not exist"));
    }
}
