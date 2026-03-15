//! Database connection pool management via `dao!` macro.
//!
//! Provides declarative database pool creation — declare a name and relative path,
//! get a Clone-able struct with automatic pool management.
//!
//! # Example
//!
//! ```ignore
//! use mad_hatter::dao;
//!
//! dao!(MyDb, "data/my.db");
//!
//! let db = MyDb::open("/base/dir")?;
//! let mut conn = db.conn()?;
//! // use conn with Diesel...
//! ```

/// Declare a database connection pool struct.
///
/// Generates a `Clone`-able struct that manages a Diesel r2d2 connection pool.
/// The relative path is joined with the base directory passed to `open()`.
///
/// - `open(base_dir)` — creates the pool (and parent directories)
/// - `conn()` — gets a pooled connection (auto-returned on Drop)
#[macro_export]
macro_rules! dao {
    ($name:ident, $rel_path:literal) => {
        #[derive(Clone)]
        pub struct $name {
            pool: diesel::r2d2::Pool<diesel::r2d2::ConnectionManager<diesel::SqliteConnection>>,
        }

        impl $name {
            /// Open (or create) the database, building a connection pool.
            ///
            /// `base_dir` is the root directory; the declared relative path is appended.
            pub fn open(base_dir: impl AsRef<std::path::Path>) -> Result<Self, Box<dyn std::error::Error>> {
                let db_path = base_dir.as_ref().join($rel_path);
                if let Some(parent) = db_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let path_str = db_path
                    .to_str()
                    .ok_or_else(|| format!("invalid db path: {}", db_path.display()))?;
                let manager = diesel::r2d2::ConnectionManager::<diesel::SqliteConnection>::new(path_str);
                let pool = diesel::r2d2::Pool::builder()
                    .max_size(2)
                    .build(manager)
                    .map_err(|e| format!("failed to create pool for {}: {}", $rel_path, e))?;
                Ok(Self { pool })
            }

            /// Get a connection from the pool.
            pub fn conn(
                &self,
            ) -> Result<
                diesel::r2d2::PooledConnection<diesel::r2d2::ConnectionManager<diesel::SqliteConnection>>,
                Box<dyn std::error::Error>,
            > {
                self.pool
                    .get()
                    .map_err(|e| format!("pool error for {}: {}", $rel_path, e).into())
            }
        }
    };
}
