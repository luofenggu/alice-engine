//! # Persistence Isolator
//!
//! Generic framework that bridges in-memory structs to storage backends.
//! Business code only reads/writes memory; this layer handles durability.
//!
//! ## Design
//!
//! - `Collection<T>` — ordered list storage (messages, logs, etc.)
//! - `KvStore` — key-value storage (status, settings, etc.)
//! - `Persist` trait — implemented by model types to declare their schema
//! - `Backend` — storage backend (currently SQLite only)
//!
//! ## Rules
//!
//! - Only generic framework code lives here (like serde, tarpc, protobuf)
//! - No business concepts — those belong in engine/model/
//! - SQL, file paths, serialization formats are contained within

use anyhow::{Result, Context};
use rusqlite::Connection;
use std::path::PathBuf;
use std::marker::PhantomData;

// ─── Value: the universal bridge between memory and storage ───

/// A generic value that can be stored in any backend.
/// This is the "wire format" between business types and storage.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Real(n) => Some(*n),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

// Conversions for ergonomic use
impl From<String> for Value {
    fn from(s: String) -> Self { Value::Text(s) }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self { Value::Text(s.to_string()) }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self { Value::Integer(n) }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self { Value::Integer(n as i64) }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self { Value::Real(n) }
}

impl From<Option<String>> for Value {
    fn from(opt: Option<String>) -> Self {
        match opt {
            Some(s) => Value::Text(s),
            None => Value::Null,
        }
    }
}

// ─── Column: schema declaration ───

/// Column type in the schema declaration.
#[derive(Debug, Clone, Copy)]
pub enum ColumnType {
    /// Auto-incrementing integer primary key (one per table, must be first)
    Id,
    /// Text column (NOT NULL)
    Text,
    /// Optional text column (nullable)
    TextOptional,
    /// Integer column (NOT NULL)
    Integer,
    /// Real/float column (NOT NULL)
    Real,
}

/// A column definition — name + type.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: &'static str,
    pub col_type: ColumnType,
}

impl Column {
    pub fn id(name: &'static str) -> Self {
        Column { name, col_type: ColumnType::Id }
    }
    pub fn text(name: &'static str) -> Self {
        Column { name, col_type: ColumnType::Text }
    }
    pub fn text_optional(name: &'static str) -> Self {
        Column { name, col_type: ColumnType::TextOptional }
    }
    pub fn integer(name: &'static str) -> Self {
        Column { name, col_type: ColumnType::Integer }
    }
    pub fn real(name: &'static str) -> Self {
        Column { name, col_type: ColumnType::Real }
    }
}

// ─── Persist trait: the ".proto" for storage ───

/// Implemented by model types to declare how they map to storage.
/// This is the equivalent of a .proto file — pure declaration, no logic.
pub trait Persist: Sized {
    /// Collection/table name
    fn collection_name() -> &'static str;

    /// Schema declaration — ordered list of columns
    fn schema() -> Vec<Column>;

    /// Get the primary key ID of this item (0 if not yet persisted)
    fn id(&self) -> i64;

    /// Serialize to a row of values (must match schema order, skip Id column)
    fn to_row(&self) -> Vec<Value>;

    /// Deserialize from a row of values (all columns including Id)
    fn from_row(values: &[Value]) -> Result<Self>;
}

// ─── Backend ───

/// Storage backend configuration.
#[derive(Debug, Clone)]
pub enum Backend {
    Sqlite(PathBuf),
}

// ─── Collection<T>: list storage ───

/// An in-memory collection backed by persistent storage.
/// All queries happen in memory; mutations are written through to storage.
pub struct Collection<T: Persist> {
    items: Vec<T>,
    conn: Connection,
    _phantom: PhantomData<T>,
}

impl<T: Persist> Collection<T> {
    /// Open a collection, creating the table if needed, and load all rows.
    pub fn open(backend: &Backend) -> Result<Self> {
        let Backend::Sqlite(path) = backend;
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open sqlite: {}", path.display()))?;

        // Create table if not exists
        let create_sql = Self::build_create_table_sql();
        conn.execute_batch(&create_sql)
            .context("failed to create table")?;

        // Load all rows into memory
        let items = Self::load_all(&conn)?;

        Ok(Collection {
            items,
            conn,
            _phantom: PhantomData,
        })
    }

    /// Insert an item. Returns the auto-generated ID.
    /// The item's Id field value is ignored; SQLite assigns the real ID.
    pub fn insert(&mut self, item: &T) -> Result<i64> {
        let row = item.to_row();
        let schema = T::schema();

        // Build INSERT — skip the Id column
        let non_id_columns: Vec<&Column> = schema.iter()
            .filter(|c| !matches!(c.col_type, ColumnType::Id))
            .collect();

        let col_names: Vec<&str> = non_id_columns.iter().map(|c| c.name).collect();
        let placeholders: Vec<&str> = non_id_columns.iter().map(|_| "?").collect();

        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            T::collection_name(),
            col_names.join(", "),
            placeholders.join(", "),
        );

        // Bind values
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = row.iter().map(|v| value_to_sql(v)).collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();

        self.conn.execute(&sql, param_refs.as_slice())
            .context("failed to insert")?;

        let id = self.conn.last_insert_rowid();

        // Reload the inserted row to get the complete item with ID
        let select_sql = format!(
            "SELECT {} FROM {} WHERE rowid = ?",
            Self::column_names_csv(),
            T::collection_name(),
        );
        let mut stmt = self.conn.prepare(&select_sql)?;
        let values = Self::read_row_values(&mut stmt, &[&id])?;
        let new_item = T::from_row(&values)?;
        self.items.push(new_item);

        Ok(id)
    }

    /// Get all items (read-only slice).
    pub fn all(&self) -> &[T] {
        &self.items
    }

    /// Iterate over all items.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// Number of items.
    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// Update items matching a filter. The updater closure mutates each matching item.
    /// Changes are written back to storage.
    pub fn update_where<F, U>(&mut self, filter: F, updater: U) -> Result<usize>
    where
        F: Fn(&T) -> bool,
        U: Fn(&mut T),
    {
        let schema = T::schema();
        let id_col = schema.iter()
            .find(|c| matches!(c.col_type, ColumnType::Id))
            .expect("Collection requires an Id column in schema");

        let non_id_columns: Vec<&Column> = schema.iter()
            .filter(|c| !matches!(c.col_type, ColumnType::Id))
            .collect();

        let set_clause: Vec<String> = non_id_columns.iter()
            .map(|c| format!("{} = ?", c.name))
            .collect();

        let sql = format!(
            "UPDATE {} SET {} WHERE {} = ?",
            T::collection_name(),
            set_clause.join(", "),
            id_col.name,
        );

        let mut count = 0;
        for item in self.items.iter_mut() {
            if filter(item) {
                updater(item);
                count += 1;

                let row = item.to_row();
                let id = item.id();

                let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = row.iter().map(|v| value_to_sql(v)).collect();
                params.push(Box::new(id));
                let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();

                self.conn.execute(&sql, param_refs.as_slice())
                    .context("failed to update")?;
            }
        }

        Ok(count)
    }

    /// Reload all data from storage (useful after external changes).
    pub fn reload(&mut self) -> Result<()> {
        self.items = Self::load_all(&self.conn)?;
        Ok(())
    }

    // ─── Internal helpers ───

    fn build_create_table_sql() -> String {
        let schema = T::schema();
        let col_defs: Vec<String> = schema.iter().map(|col| {
            match col.col_type {
                ColumnType::Id => format!("{} INTEGER PRIMARY KEY AUTOINCREMENT", col.name),
                ColumnType::Text => format!("{} TEXT NOT NULL DEFAULT ''", col.name),
                ColumnType::TextOptional => format!("{} TEXT", col.name),
                ColumnType::Integer => format!("{} INTEGER NOT NULL DEFAULT 0", col.name),
                ColumnType::Real => format!("{} REAL NOT NULL DEFAULT 0.0", col.name),
            }
        }).collect();

        format!(
            "CREATE TABLE IF NOT EXISTS {} ({})",
            T::collection_name(),
            col_defs.join(", "),
        )
    }

    fn column_names_csv() -> String {
        T::schema().iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
    }

    fn load_all(conn: &Connection) -> Result<Vec<T>> {
        let sql = format!("SELECT {} FROM {}", Self::column_names_csv(), T::collection_name());
        let mut stmt = conn.prepare(&sql)?;

        let schema = T::schema();
        let col_count = schema.len();

        let rows = stmt.query_map([], |row| {
            let mut values = Vec::with_capacity(col_count);
            for (i, col) in schema.iter().enumerate() {
                let value = match col.col_type {
                    ColumnType::Id | ColumnType::Integer => {
                        let v: i64 = row.get(i)?;
                        Value::Integer(v)
                    }
                    ColumnType::Text => {
                        let v: String = row.get(i)?;
                        Value::Text(v)
                    }
                    ColumnType::TextOptional => {
                        let v: Option<String> = row.get(i)?;
                        match v {
                            Some(s) => Value::Text(s),
                            None => Value::Null,
                        }
                    }
                    ColumnType::Real => {
                        let v: f64 = row.get(i)?;
                        Value::Real(v)
                    }
                };
                values.push(value);
            }
            Ok(values)
        })?;

        let mut items = Vec::new();
        for row_result in rows {
            let values = row_result?;
            let item = T::from_row(&values)?;
            items.push(item);
        }

        Ok(items)
    }

    fn read_row_values(stmt: &mut rusqlite::Statement, params: &[&dyn rusqlite::types::ToSql]) -> Result<Vec<Value>> {
        let schema = T::schema();
        stmt.query_row(params, |row| {
            let mut values = Vec::with_capacity(schema.len());
            for (i, col) in schema.iter().enumerate() {
                let value = match col.col_type {
                    ColumnType::Id | ColumnType::Integer => {
                        let v: i64 = row.get(i)?;
                        Value::Integer(v)
                    }
                    ColumnType::Text => {
                        let v: String = row.get(i)?;
                        Value::Text(v)
                    }
                    ColumnType::TextOptional => {
                        let v: Option<String> = row.get(i)?;
                        match v {
                            Some(s) => Value::Text(s),
                            None => Value::Null,
                        }
                    }
                    ColumnType::Real => {
                        let v: f64 = row.get(i)?;
                        Value::Real(v)
                    }
                };
                values.push(value);
            }
            Ok(values)
        }).context("failed to read row")
    }


}

// ─── KvStore: key-value storage ───

/// A key-value store backed by persistent storage.
/// All reads happen from an in-memory cache; writes go through to storage.
pub struct KvStore {
    cache: std::collections::HashMap<String, String>,
    conn: Connection,
    table_name: &'static str,
}

impl KvStore {
    /// Open a KV store with the given table name.
    pub fn open(backend: &Backend, table_name: &'static str) -> Result<Self> {
        let Backend::Sqlite(path) = backend;
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open sqlite: {}", path.display()))?;

        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {} (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            table_name,
        )).context("failed to create kv table")?;

        // Load all into cache
        let mut cache = std::collections::HashMap::new();
        {
            let mut stmt = conn.prepare(&format!("SELECT key, value FROM {}", table_name))?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (k, v) = row?;
                cache.insert(k, v);
            }
        }

        Ok(KvStore { cache, conn, table_name })
    }

    /// Get a value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.cache.get(key).map(|s| s.as_str())
    }

    /// Set a key-value pair (insert or update).
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let sql = format!(
            "INSERT OR REPLACE INTO {} (key, value) VALUES (?, ?)",
            self.table_name,
        );
        self.conn.execute(&sql, rusqlite::params![key, value])
            .context("failed to set kv")?;
        self.cache.insert(key.to_string(), value.to_string());
        Ok(())
    }

    /// Remove a key.
    pub fn remove(&mut self, key: &str) -> Result<()> {
        let sql = format!("DELETE FROM {} WHERE key = ?", self.table_name);
        self.conn.execute(&sql, rusqlite::params![key])
            .context("failed to remove kv")?;
        self.cache.remove(key);
        Ok(())
    }
}

// ─── Helpers ───

#[cfg(test)]
#[cfg(test)]
mod tests;

fn value_to_sql(v: &Value) -> Box<dyn rusqlite::types::ToSql> {
    match v {
        Value::Null => Box::new(Option::<String>::None),
        Value::Integer(n) => Box::new(*n),
        Value::Real(n) => Box::new(*n),
        Value::Text(s) => Box::new(s.clone()),
    }
}

