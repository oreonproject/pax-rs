use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use utils::{err, get_metadata_dir};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub timestamp: u64,
    pub transaction_type: TransactionType,
    pub packages: Vec<PackageOperation>,
    pub status: TransactionStatus,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransactionType {
    Install,
    Remove,
    Upgrade,
    Downgrade,
    Purge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TransactionStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageOperation {
    pub package_name: String,
    pub package_version: String,
    pub operation_type: OperationType,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
    pub backup_path: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OperationType {
    Install,
    Remove,
    Upgrade,
    Downgrade,
    Purge,
}

pub struct TransactionManager {
    transactions: HashMap<String, Transaction>,
    current_transaction: Option<String>,
}

impl TransactionManager {
    pub fn new() -> Self {
        Self {
            transactions: HashMap::new(),
            current_transaction: None,
        }
    }

    pub fn start_transaction(
        &mut self,
        transaction_type: TransactionType,
        description: String,
    ) -> Result<String, String> {
        let transaction_id = self.generate_transaction_id();
        
        let transaction = Transaction {
            id: transaction_id.clone(),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            transaction_type,
            packages: Vec::new(),
            status: TransactionStatus::Pending,
            description,
        };

        self.transactions.insert(transaction_id.clone(), transaction);
        self.current_transaction = Some(transaction_id.clone());
        
        Ok(transaction_id)
    }

    pub fn add_package_operation(
        &mut self,
        package_name: String,
        package_version: String,
        operation_type: OperationType,
        old_version: Option<String>,
    ) -> Result<(), String> {
        let transaction_id = self.current_transaction.as_ref()
            .ok_or("No active transaction")?;

        let transaction = self.transactions.get_mut(transaction_id)
            .ok_or("Transaction not found")?;

        let operation = PackageOperation {
            package_name,
            package_version,
            operation_type,
            old_version,
            new_version: None,
            backup_path: None,
            manifest_path: None,
        };

        transaction.packages.push(operation);
        Ok(())
    }

    pub fn commit_transaction(&mut self) -> Result<(), String> {
        let transaction_id = self.current_transaction.as_ref()
            .ok_or("No active transaction")?;

        let transaction = self.transactions.get_mut(transaction_id)
            .ok_or("Transaction not found")?;

        transaction.status = TransactionStatus::Completed;
        let transaction_clone = transaction.clone();
        self.current_transaction = None;
        
        // Save transaction to disk
        self.save_transaction(&transaction_clone)?;
        
        Ok(())
    }

    pub fn rollback_transaction(&mut self, transaction_id: &str) -> Result<(), String> {
        // Clone the packages to avoid borrow issues
        let packages = {
            let transaction = self.transactions.get(transaction_id)
                .ok_or("Transaction not found")?;

            if transaction.status != TransactionStatus::Completed {
                return err!("Can only rollback completed transactions");
            }

            transaction.packages.clone()
        };

        println!("Rolling back transaction {}...", transaction_id);

        // Rollback packages in reverse order
        for operation in packages.iter().rev() {
            self.rollback_package_operation(operation)?;
        }

        // Update transaction status
        let transaction = self.transactions.get_mut(transaction_id)
            .ok_or("Transaction not found")?;
        transaction.status = TransactionStatus::RolledBack;
        let transaction_clone = transaction.clone();
        self.save_transaction(&transaction_clone)?;

        println!("Transaction {} rolled back successfully", transaction_id);
        Ok(())
    }

    fn rollback_package_operation(&self, operation: &PackageOperation) -> Result<(), String> {
        match operation.operation_type {
            OperationType::Install => {
                // Remove the package
                println!("Rolling back installation of {}...", operation.package_name);
                
                // Remove package metadata
                let mut metadata_path = get_metadata_dir()?;
                metadata_path.push(format!("{}.yaml", operation.package_name));
                fs::remove_file(&metadata_path).ok();

                // Remove files using manifest
                if operation.manifest_path.is_some() {
                    if let Ok(manifest) = crate::file_tracking::FileManifest::load(&operation.package_name) {
                        manifest.remove_files(false)?;
                    }
                }
            }
            OperationType::Remove => {
                // Reinstall the package
                println!("Rolling back removal of {}...", operation.package_name);
                
                // Restore from backup if available
                if operation.backup_path.is_some() {
                    if let Ok(manifest) = crate::file_tracking::FileManifest::load(&operation.package_name) {
                        // Restore files from backup
                        for file in &manifest.files {
                            if let Some(backup_file) = &file.backup_path {
                                if backup_file.exists() {
                                    fs::copy(backup_file, &file.path).ok();
                                    println!("Restored file: {}", file.path.display());
                                }
                            }
                        }
                    }
                }
            }
            OperationType::Upgrade => {
                // Downgrade to old version
                if let Some(old_version) = &operation.old_version {
                    println!("Rolling back upgrade of {} from {} to {}...", 
                        operation.package_name, operation.package_version, old_version);
                    
                    // This would involve reinstalling the old version
                    // For now, just log the operation
                    println!("Would downgrade {} to version {}", operation.package_name, old_version);
                }
            }
            OperationType::Downgrade => {
                // Upgrade back to new version
                println!("Rolling back downgrade of {}...", operation.package_name);
                println!("Would upgrade {} back to version {}", operation.package_name, operation.package_version);
            }
            OperationType::Purge => {
                // Restore package (similar to remove rollback)
                println!("Rolling back purge of {}...", operation.package_name);
                
                if operation.backup_path.is_some() {
                    if let Ok(manifest) = crate::file_tracking::FileManifest::load(&operation.package_name) {
                        for file in &manifest.files {
                            if let Some(backup_file) = &file.backup_path {
                                if backup_file.exists() {
                                    fs::copy(backup_file, &file.path).ok();
                                    println!("Restored file: {}", file.path.display());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub fn list_transactions(&self) -> Vec<&Transaction> {
        let mut transactions: Vec<&Transaction> = self.transactions.values().collect();
        transactions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        transactions
    }

    pub fn get_transaction(&self, transaction_id: &str) -> Option<&Transaction> {
        self.transactions.get(transaction_id)
    }

    pub fn cleanup_old_transactions(&mut self, keep_count: usize) -> Result<(), String> {
        let mut transactions: Vec<_> = self.transactions.values().collect();
        transactions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        if transactions.len() > keep_count {
            let to_remove: Vec<String> = transactions
                .into_iter()
                .skip(keep_count)
                .map(|t| t.id.clone())
                .collect();

            for transaction_id in to_remove {
                self.transactions.remove(&transaction_id);
                
                // Remove transaction file
                let mut transaction_path = get_metadata_dir()?;
                transaction_path.push("transactions");
                transaction_path.push(format!("{}.yaml", transaction_id));
                fs::remove_file(&transaction_path).ok();
            }
        }

        Ok(())
    }

    fn generate_transaction_id(&self) -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        format!("tx_{}", timestamp)
    }

    fn save_transaction(&self, transaction: &Transaction) -> Result<(), String> {
        let mut transaction_path = get_metadata_dir()?;
        transaction_path.push("transactions");
        fs::create_dir_all(&transaction_path).ok();
        transaction_path.push(format!("{}.yaml", transaction.id));

        let mut file = File::create(&transaction_path)
            .map_err(|_| format!("Failed to create transaction file for {}", transaction.id))?;

        let yaml = serde_norway::to_string(transaction)
            .map_err(|_| format!("Failed to serialize transaction {}", transaction.id))?;

        file.write_all(yaml.as_bytes())
            .map_err(|_| format!("Failed to write transaction {}", transaction.id))?;

        Ok(())
    }

    pub fn load_transactions(&mut self) -> Result<(), String> {
        let mut transaction_dir = get_metadata_dir()?;
        transaction_dir.push("transactions");

        if !transaction_dir.exists() {
            return Ok(());
        }

        let entries = fs::read_dir(&transaction_dir)
            .map_err(|_| "Failed to read transactions directory")?;

        for entry in entries.flatten() {
            if let Some(extension) = entry.path().extension() {
                if extension == "yaml" {
                    if let Some(stem) = entry.path().file_stem() {
                        if let Some(transaction_id) = stem.to_str() {
                            if let Ok(transaction) = self.load_transaction(transaction_id) {
                                self.transactions.insert(transaction_id.to_string(), transaction);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn load_transaction(&self, transaction_id: &str) -> Result<Transaction, String> {
        let mut transaction_path = get_metadata_dir()?;
        transaction_path.push("transactions");
        transaction_path.push(format!("{}.yaml", transaction_id));

        let mut file = File::open(&transaction_path)
            .map_err(|_| format!("Failed to open transaction file {}", transaction_id))?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|_| format!("Failed to read transaction file {}", transaction_id))?;

        serde_norway::from_str(&contents)
            .map_err(|_| format!("Failed to parse transaction file {}", transaction_id))
    }
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self::new()
    }
}
