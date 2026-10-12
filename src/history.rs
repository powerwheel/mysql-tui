use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_HISTORY: usize = 200;

/// ~/.config/mysql-tui/history.json — plaintext JSON array, oldest first.
pub fn history_path() -> PathBuf {
    let mut p = crate::config::config_path();
    p.set_file_name("history.json");
    p
}

/// Missing or unreadable file yields an empty history; a corrupt file is
/// reported so the user knows it will be overwritten on the next save.
pub fn load(path: &Path) -> Result<Vec<String>, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };
    let mut entries: Vec<String> = serde_json::from_str(&text)
        .map_err(|e| format!("invalid history file {}: {e}", path.display()))?;
    entries.retain(|s| !s.trim().is_empty());
    if entries.len() > MAX_HISTORY {
        entries.drain(..entries.len() - MAX_HISTORY);
    }
    Ok(entries)
}

pub fn save(path: &Path, entries: &[String]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text).map_err(|e| format!("failed to write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mysql-tui-test-{}-{name}", std::process::id()));
        p.push("history.json");
        p
    }

    #[test]
    fn missing_file_is_empty() {
        let p = temp_path("missing");
        assert_eq!(load(&p).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn roundtrip_creates_dir_and_preserves_order() {
        let p = temp_path("roundtrip");
        let entries = vec!["SELECT 1".to_string(), "SELECT 2;\nSELECT 3".to_string()];
        save(&p, &entries).unwrap();
        assert_eq!(load(&p).unwrap(), entries);
        assert!(!p.with_extension("json.tmp").exists());
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn corrupt_file_reports_error() {
        let p = temp_path("corrupt");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "not json").unwrap();
        assert!(load(&p).unwrap_err().contains("invalid history file"));
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn load_trims_to_max_and_drops_blank() {
        let p = temp_path("trim");
        let mut entries: Vec<String> = (0..MAX_HISTORY + 5).map(|i| format!("SELECT {i}")).collect();
        entries.insert(0, "   ".to_string());
        save(&p, &entries).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(back.len(), MAX_HISTORY);
        assert_eq!(back.last().unwrap(), &format!("SELECT {}", MAX_HISTORY + 4));
        fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
}
