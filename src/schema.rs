use std::collections::HashMap;

/// Cached metadata about the connected server, used for autocompletion.
#[derive(Debug, Clone, Default)]
pub struct SchemaCache {
    pub database: String,
    pub tables: Vec<String>,
    /// lowercase table name -> column names in ordinal order
    pub columns: HashMap<String, Vec<String>>,
    pub all_columns: Vec<String>,
    pub databases: Vec<String>,
    /// database name -> table names in ordinal order (for the sidebar tree)
    pub tables_by_db: HashMap<String, Vec<String>>,
    /// database.table -> column names (lazy-loaded for sidebar tree)
    pub column_names: HashMap<String, Vec<String>>,
    /// database.table -> DDL from SHOW CREATE TABLE (lazy-loaded)
    pub ddl: HashMap<String, String>,
}

impl SchemaCache {
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    pub fn columns_of(&self, table: &str) -> Option<&Vec<String>> {
        self.columns.get(&table.to_lowercase())
    }
}

#[cfg(test)]
impl SchemaCache {
    pub fn test_new(database: &str, tables: &[(&str, &[&str])], databases: &[&str]) -> Self {
        let mut columns = HashMap::new();
        let mut all_columns: Vec<String> = Vec::new();
        let mut table_names = Vec::new();
        for (t, cols) in tables {
            table_names.push(t.to_string());
            let cols: Vec<String> = cols.iter().map(|c| c.to_string()).collect();
            for c in &cols {
                if !all_columns.contains(c) {
                    all_columns.push(c.clone());
                }
            }
            columns.insert(t.to_lowercase(), cols);
        }
        all_columns.sort();
        let mut tables_by_db = HashMap::new();
        tables_by_db.insert(database.to_string(), table_names.clone());
        Self {
            database: database.to_string(),
            tables: table_names,
            columns,
            all_columns,
            databases: databases.iter().map(|d| d.to_string()).collect(),
            tables_by_db,
            column_names: HashMap::new(),
            ddl: HashMap::new(),
        }
    }
}
