//! Global database registry — provides process-wide access to the SQLite
//! index DB.
//!
//! Initialized once at application startup. Domain layers access the DB via
//! `global_db()`.

use std::sync::{Arc, OnceLock};

use crate::sqlite::OpenPawDb;

static GLOBAL_DB: OnceLock<Arc<OpenPawDb>> = OnceLock::new();

/// Serializes the lazy open in `ensure_global_db`. The check-then-set pattern
/// would otherwise let two threads open the DB concurrently (each running
/// schema migrations) and silently drop one of the `Arc`s. `Mutex::new` is
/// const, so this is a true static.
static INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Initialize the global DB. Call once at application startup.
/// Subsequent calls are silently ignored (does not panic).
pub fn init_global_db(db: Arc<OpenPawDb>) {
    let _ = GLOBAL_DB.set(db);
}

/// Get a reference to the global DB. Returns `None` if not yet initialized.
pub fn global_db() -> Option<&'static Arc<OpenPawDb>> {
    GLOBAL_DB.get()
}

/// Check whether the global DB has been initialized.
pub fn is_global_db_initialized() -> bool {
    GLOBAL_DB.get().is_some()
}

/// Ensure the global DB is initialized. If not yet initialized, attempt to
/// open it at the default location (`<openpaw home>/index.db`). Returns
/// `true` if the DB is available after this call, `false` if initialization
/// failed (caller should fall back to file-based storage).
///
/// This enables "DB-first with automatic fallback" — consumers call this
/// instead of `is_global_db_initialized()` to auto-init on first use.
pub fn ensure_global_db() -> bool {
    if GLOBAL_DB.get().is_some() {
        return true;
    }
    // Hold the init lock across open + migration so concurrent first users
    // serialize instead of racing (double-open / dropped Arc).
    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Double-check: another thread may have initialized while we waited.
    if GLOBAL_DB.get().is_some() {
        return true;
    }
    // Attempt to open the DB at the default location.
    let db_path = openpaw_core::openpaw_home().join("index.db");
    match OpenPawDb::open(&db_path) {
        Ok(db) => {
            let _ = GLOBAL_DB.set(Arc::new(db));
            true
        }
        Err(_) => false,
    }
}
