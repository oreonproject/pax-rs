use rusqlite::{Connection, Result as SqlResult, params};
use std::path::Path;
use std::sync::{Arc, Mutex};

// Database wrapper for managing package state
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    // Opens or creates database at the specified path
    pub fn open<P: AsRef<Path>>(path: P) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        let db = Database {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
        Ok(db)
    }

    // Initialize database schema if it doesnt exist
    fn init_schema(&self) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        
        // packages table - stores installed package metadata
        conn.execute(
            "CREATE TABLE IF NOT EXISTS packages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                version TEXT NOT NULL,
                description TEXT,
                origin TEXT,
                hash TEXT NOT NULL,
                install_date INTEGER NOT NULL,
                size INTEGER NOT NULL
            )",
            [],
        )?;

        // files table - tracks which files belong to which packages
        conn.execute(
            "CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                package_id INTEGER NOT NULL,
                path TEXT NOT NULL,
                file_type TEXT NOT NULL,
                FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
            )",
            [],
        )?;

        // provides table - what each package provides
        conn.execute(
            "CREATE TABLE IF NOT EXISTS provides (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                package_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                version TEXT,
                type TEXT NOT NULL,
                FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
            )",
            [],
        )?;

        // dependencies table - package dependency relationships
        conn.execute(
            "CREATE TABLE IF NOT EXISTS dependencies (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                package_id INTEGER NOT NULL,
                depends_on TEXT NOT NULL,
                version_constraint TEXT,
                dep_type TEXT NOT NULL,
                FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
            )",
            [],
        )?;

        // symlinks table - managed symbolic links
        conn.execute(
            "CREATE TABLE IF NOT EXISTS symlinks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                package_id INTEGER NOT NULL,
                link_path TEXT NOT NULL UNIQUE,
                target_path TEXT NOT NULL,
                FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
            )",
            [],
        )?;

        // create indexes for common queries
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_files_package ON files(package_id)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_provides_name ON provides(name)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_deps_package ON dependencies(package_id)",
            [],
        )?;

        Ok(())
    }

    // Insert a new package
    pub fn insert_package(
        &self,
        name: &str,
        version: &str,
        description: &str,
        origin: &str,
        hash: &str,
        size: u64,
    ) -> SqlResult<i64> {
        let conn = self.conn.lock().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        conn.execute(
            "INSERT INTO packages (name, version, description, origin, hash, install_date, size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![name, version, description, origin, hash, now, size as i64],
        )?;

        Ok(conn.last_insert_rowid())
    }

    // Get package ID by name
    pub fn get_package_id(&self, name: &str) -> SqlResult<Option<i64>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM packages WHERE name = ?1")?;
        let mut rows = stmt.query(params![name])?;

        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    // Check if package is installed
    pub fn is_installed(&self, name: &str) -> SqlResult<bool> {
        Ok(self.get_package_id(name)?.is_some())
    }

    // Get package info
    pub fn get_package_info(&self, name: &str) -> SqlResult<Option<PackageInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, version, description, origin, hash, install_date, size 
             FROM packages WHERE name = ?1"
        )?;
        let mut rows = stmt.query(params![name])?;

        if let Some(row) = rows.next()? {
            Ok(Some(PackageInfo {
                name: row.get(0)?,
                version: row.get(1)?,
                description: row.get(2)?,
                origin: row.get(3)?,
                hash: row.get(4)?,
                install_date: row.get(5)?,
                size: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    // List all installed packages
    pub fn list_packages(&self) -> SqlResult<Vec<PackageInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT name, version, description, origin, hash, install_date, size 
             FROM packages ORDER BY name"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PackageInfo {
                name: row.get(0)?,
                version: row.get(1)?,
                description: row.get(2)?,
                origin: row.get(3)?,
                hash: row.get(4)?,
                install_date: row.get(5)?,
                size: row.get(6)?,
            })
        })?;

        let mut packages = Vec::new();
        for row in rows {
            packages.push(row?);
        }
        Ok(packages)
    }

    // Remove a package
    pub fn remove_package(&self, name: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM packages WHERE name = ?1", params![name])?;
        Ok(())
    }

    // Add file entry for a package
    pub fn add_file(&self, package_id: i64, path: &str, file_type: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO files (package_id, path, file_type) VALUES (?1, ?2, ?3)",
            params![package_id, path, file_type],
        )?;
        Ok(())
    }

    // Get files for a package
    pub fn get_package_files(&self, package_id: i64) -> SqlResult<Vec<FileEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT path, file_type FROM files WHERE package_id = ?1"
        )?;
        let rows = stmt.query_map(params![package_id], |row| {
            Ok(FileEntry {
                path: row.get(0)?,
                file_type: row.get(1)?,
            })
        })?;

        let mut files = Vec::new();
        for row in rows {
            files.push(row?);
        }
        Ok(files)
    }

    // Add provides entry
    pub fn add_provides(
        &self,
        package_id: i64,
        name: &str,
        version: Option<&str>,
        prov_type: &str,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO provides (package_id, name, version, type) VALUES (?1, ?2, ?3, ?4)",
            params![package_id, name, version, prov_type],
        )?;
        Ok(())
    }

    // Query what provides a specific thing
    pub fn query_provides(&self, name: &str) -> SqlResult<Vec<ProvidesInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT p.name, p.version, p.type, pkg.name 
             FROM provides p 
             JOIN packages pkg ON p.package_id = pkg.id 
             WHERE p.name = ?1"
        )?;
        let rows = stmt.query_map(params![name], |row| {
            Ok(ProvidesInfo {
                name: row.get(0)?,
                version: row.get(1)?,
                prov_type: row.get(2)?,
                package_name: row.get(3)?,
            })
        })?;

        let mut provides = Vec::new();
        for row in rows {
            provides.push(row?);
        }
        Ok(provides)
    }

    // Add dependency
    pub fn add_dependency(
        &self,
        package_id: i64,
        depends_on: &str,
        version_constraint: Option<&str>,
        dep_type: &str,
    ) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO dependencies (package_id, depends_on, version_constraint, dep_type)
             VALUES (?1, ?2, ?3, ?4)",
            params![package_id, depends_on, version_constraint, dep_type],
        )?;
        Ok(())
    }

    // Get dependencies for a package
    pub fn get_dependencies(&self, package_id: i64) -> SqlResult<Vec<DependencyInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT depends_on, version_constraint, dep_type 
             FROM dependencies WHERE package_id = ?1"
        )?;
        let rows = stmt.query_map(params![package_id], |row| {
            Ok(DependencyInfo {
                depends_on: row.get(0)?,
                version_constraint: row.get(1)?,
                dep_type: row.get(2)?,
            })
        })?;

        let mut deps = Vec::new();
        for row in rows {
            deps.push(row?);
        }
        Ok(deps)
    }

    // Add symlink entry
    pub fn add_symlink(&self, package_id: i64, link_path: &str, target_path: &str) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO symlinks (package_id, link_path, target_path) VALUES (?1, ?2, ?3)",
            params![package_id, link_path, target_path],
        )?;
        Ok(())
    }

    // Get symlinks for a package
    pub fn get_symlinks(&self, package_id: i64) -> SqlResult<Vec<SymlinkInfo>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT link_path, target_path FROM symlinks WHERE package_id = ?1"
        )?;
        let rows = stmt.query_map(params![package_id], |row| {
            Ok(SymlinkInfo {
                link_path: row.get(0)?,
                target_path: row.get(1)?,
            })
        })?;

        let mut symlinks = Vec::new();
        for row in rows {
            symlinks.push(row?);
        }
        Ok(symlinks)
    }

    // Check which packages depend on this package
    pub fn get_reverse_dependencies(&self, package_name: &str) -> SqlResult<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT pkg.name 
             FROM dependencies d 
             JOIN packages pkg ON d.package_id = pkg.id 
             WHERE d.depends_on = ?1"
        )?;
        let rows = stmt.query_map(params![package_name], |row| row.get(0))?;

        let mut deps = Vec::new();
        for row in rows {
            deps.push(row?);
        }
        Ok(deps)
    }
}

// Data structures for database results
#[derive(Debug, Clone)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub origin: String,
    pub hash: String,
    pub install_date: i64,
    pub size: i64,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub file_type: String,
}

#[derive(Debug, Clone)]
pub struct ProvidesInfo {
    pub name: String,
    pub version: Option<String>,
    pub prov_type: String,
    pub package_name: String,
}

#[derive(Debug, Clone)]
pub struct DependencyInfo {
    pub depends_on: String,
    pub version_constraint: Option<String>,
    pub dep_type: String,
}

#[derive(Debug, Clone)]
pub struct SymlinkInfo {
    pub link_path: String,
    pub target_path: String,
}

