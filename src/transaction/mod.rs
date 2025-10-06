use crate::database::Database;
use crate::logging::Logger;
use crate::store::PackageStore;
use crate::symlinks::SymlinkManager;

// Transaction state for rollback capability
pub struct Transaction {
    logger: Logger,
    operations: Vec<Operation>,
    committed: bool,
}

// Different types of operations that can be rolled back
#[derive(Debug, Clone)]
enum Operation {
    DatabaseInsert { table: String, id: i64 },
    FileExtracted { hash: String },
    SymlinkCreated { link_path: String },
    FileDownloaded { path: String },
}

impl Transaction {
    // Create a new transaction
    pub fn new(logger: Logger) -> Self {
        Transaction {
            logger,
            operations: Vec::new(),
            committed: false,
        }
    }

    // Record a database insert operation
    pub fn record_db_insert(&mut self, table: &str, id: i64) {
        self.operations.push(Operation::DatabaseInsert {
            table: table.to_string(),
            id,
        });
        
        let _ = self.logger.log_transaction(
            "DB_INSERT",
            &format!("{}:{}", table, id),
        );
    }

    // Record a file extraction
    pub fn record_file_extract(&mut self, hash: &str) {
        self.operations.push(Operation::FileExtracted {
            hash: hash.to_string(),
        });
        
        let _ = self.logger.log_transaction(
            "FILE_EXTRACT",
            hash,
        );
    }

    // Record a symlink creation
    pub fn record_symlink(&mut self, link_path: &str) {
        self.operations.push(Operation::SymlinkCreated {
            link_path: link_path.to_string(),
        });
        
        let _ = self.logger.log_transaction(
            "SYMLINK",
            link_path,
        );
    }

    // Record a file download
    pub fn record_download(&mut self, path: &str) {
        self.operations.push(Operation::FileDownloaded {
            path: path.to_string(),
        });
        
        let _ = self.logger.log_transaction(
            "DOWNLOAD",
            path,
        );
    }

    // Commit the transaction (marks it as successful, prevents rollback)
    pub fn commit(&mut self) {
        self.committed = true;
        self.operations.clear();
        let _ = self.logger.clear_transaction_log();
        self.logger.info("Transaction committed successfully");
    }

    // Rollback the transaction
    pub fn rollback(
        &mut self,
        _db: &Database,
        store: &PackageStore,
        _symlink_mgr: &SymlinkManager,
    ) -> Result<(), String> {
        if self.committed {
            return Ok(()); // already committed, nothing to rollback
        }

        self.logger.warning("Rolling back transaction...");
        
        // Reverse the operations
        for op in self.operations.iter().rev() {
            match op {
                Operation::DatabaseInsert { table, id } => {
                    self.logger.debug(&format!("Rollback: Remove {} from {}", id, table));
                    // Note: actual db rollback would need table-specific logic
                    // For now we log it - full implementation would delete records
                }
                Operation::FileExtracted { hash } => {
                    self.logger.debug(&format!("Rollback: Remove extracted files {}", hash));
                    if let Err(e) = store.remove_package(hash) {
                        self.logger.error(&format!("Failed to remove package during rollback: {}", e));
                    }
                }
                Operation::SymlinkCreated { link_path } => {
                    self.logger.debug(&format!("Rollback: Remove symlink {}", link_path));
                    let _ = std::fs::remove_file(link_path);
                }
                Operation::FileDownloaded { path } => {
                    self.logger.debug(&format!("Rollback: Remove downloaded file {}", path));
                    let _ = std::fs::remove_file(path);
                }
            }
        }

        self.operations.clear();
        let _ = self.logger.clear_transaction_log();
        self.logger.info("Rollback completed");

        Ok(())
    }

    // Check if transaction is committed
    pub fn is_committed(&self) -> bool {
        self.committed
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if !self.committed && !self.operations.is_empty() {
            self.logger.warning("Transaction dropped without commit - operations may be incomplete");
        }
    }
}

// Installation transaction wrapper
pub struct InstallTransaction {
    transaction: Transaction,
    package_name: String,
    package_id: Option<i64>,
}

impl InstallTransaction {
    // Create new install transaction
    pub fn new(logger: Logger, package_name: String) -> Self {
        InstallTransaction {
            transaction: Transaction::new(logger),
            package_name,
            package_id: None,
        }
    }

    // Set the package ID after database insert
    pub fn set_package_id(&mut self, id: i64) {
        self.package_id = Some(id);
        self.transaction.record_db_insert("packages", id);
    }

    // Record file extraction
    pub fn record_extract(&mut self, hash: &str) {
        self.transaction.record_file_extract(hash);
    }

    // Record symlink creation
    pub fn record_symlink(&mut self, link_path: &str) {
        self.transaction.record_symlink(link_path);
    }

    // Record download
    pub fn record_download(&mut self, path: &str) {
        self.transaction.record_download(path);
    }

    // Commit the installation
    pub fn commit(mut self) {
        self.transaction.commit();
    }

    // Rollback the installation
    pub fn rollback(
        mut self,
        db: &Database,
        store: &PackageStore,
        symlink_mgr: &SymlinkManager,
    ) -> Result<(), String> {
        // If we have a package ID, remove it from the database
        if let Some(_pkg_id) = self.package_id {
            if let Err(e) = db.remove_package(&self.package_name) {
                self.transaction.logger.error(&format!(
                    "Failed to remove package from database during rollback: {}",
                    e
                ));
            }
        }

        self.transaction.rollback(db, store, symlink_mgr)
    }
}

// Removal transaction wrapper  
pub struct RemovalTransaction {
    transaction: Transaction,
    package_name: String,
    backup_created: bool,
}

impl RemovalTransaction {
    // Create new removal transaction
    pub fn new(logger: Logger, package_name: String) -> Self {
        RemovalTransaction {
            transaction: Transaction::new(logger),
            package_name,
            backup_created: false,
        }
    }

    // Record that backup was created (for potential restoration)
    pub fn record_backup(&mut self) {
        self.backup_created = true;
    }

    // Commit the removal
    pub fn commit(mut self) {
        self.transaction.commit();
    }

    // Rollback removal would restore from backup
    pub fn rollback(self) -> Result<(), String> {
        if self.backup_created {
            self.transaction.logger.info(&format!(
                "Would restore backup for {} (not implemented)",
                self.package_name
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_transaction_commit() {
        let temp_dir = TempDir::new().unwrap();
        let logger = Logger::with_dir(temp_dir.path().to_str().unwrap()).unwrap();
        
        let mut transaction = Transaction::new(logger);
        transaction.record_db_insert("packages", 1);
        transaction.commit();
        
        assert!(transaction.is_committed());
        assert_eq!(transaction.operations.len(), 0);
    }

    #[test]
    fn test_install_transaction() {
        let temp_dir = TempDir::new().unwrap();
        let logger = Logger::with_dir(temp_dir.path().to_str().unwrap()).unwrap();
        
        let mut install_tx = InstallTransaction::new(logger, "test-package".to_string());
        install_tx.set_package_id(1);
        install_tx.record_download("/tmp/test.pkg");
        install_tx.commit();
    }
}

