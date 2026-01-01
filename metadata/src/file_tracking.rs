use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use utils::get_metadata_dir;
use crate::processed::render_progress;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictType {
    FileOwnership,
    DirectoryOwnership,
    SymlinkOwnership,
    UntrackedFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConflict {
    pub path: PathBuf,
    pub existing_owner: String,
    pub new_package: String,
    pub conflict_type: ConflictType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileManifest {
    pub package_name: String,
    pub package_version: String,
    pub files: Vec<InstalledFile>,
    pub directories: Vec<InstalledDirectory>,
    pub symlinks: Vec<InstalledSymlink>,
    pub installed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledFile {
    pub path: PathBuf,
    pub size: u64,
    pub permissions: u32,
    pub checksum: String,
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledDirectory {
    pub path: PathBuf,
    pub permissions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSymlink {
    pub path: PathBuf,
    pub target: PathBuf,
}

impl FileManifest {
    pub fn new(package_name: String, package_version: String) -> Self {
        Self {
            package_name,
            package_version,
            files: Vec::new(),
            directories: Vec::new(),
            symlinks: Vec::new(),
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    pub fn add_file(&mut self, path: PathBuf, size: u64, permissions: u32, checksum: String) {
        self.files.push(InstalledFile {
            path,
            size,
            permissions,
            checksum,
            backup_path: None,
        });
    }

    pub fn add_directory(&mut self, path: PathBuf, permissions: u32) {
        self.directories.push(InstalledDirectory {
            path,
            permissions,
        });
    }

    pub fn add_symlink(&mut self, path: PathBuf, target: PathBuf) {
        self.symlinks.push(InstalledSymlink { path, target });
    }

    pub fn save(&self) -> Result<(), String> {
        let mut manifest_path = get_metadata_dir()?;
        manifest_path.push("manifests");
        fs::create_dir_all(&manifest_path).ok();
        manifest_path.push(format!("{}.yaml", self.package_name));

        let mut file = File::create(&manifest_path)
            .map_err(|_| format!("Failed to create manifest file for {}", self.package_name))?;

        let yaml = serde_norway::to_string(self)
            .map_err(|_| format!("Failed to serialize manifest for {}", self.package_name))?;

        file.write_all(yaml.as_bytes())
            .map_err(|_| format!("Failed to write manifest for {}", self.package_name))?;

        Ok(())
    }

    pub fn load(package_name: &str) -> Result<Self, String> {
        let mut manifest_path = get_metadata_dir()?;
        manifest_path.push("manifests");
        manifest_path.push(format!("{}.yaml", package_name));

        let mut file = File::open(&manifest_path)
            .map_err(|_| format!("Failed to open manifest for {}", package_name))?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|_| format!("Failed to read manifest for {}", package_name))?;

        serde_norway::from_str(&contents)
            .map_err(|_| format!("Failed to parse manifest for {}", package_name))
    }

    pub fn remove_files(&self, purge: bool) -> Result<(), String> {
        // Safety check: prevent removal of critical system directories
        let critical_dirs = [
            "/", "/bin", "/sbin", "/lib", "/lib64", "/usr", "/usr/bin", "/usr/sbin", 
            "/usr/lib", "/usr/lib64", "/etc", "/var", "/tmp", "/home", "/root",
            "/proc", "/sys", "/dev", "/mnt", "/media", "/opt", "/boot", "/run"
        ];
        
        let total_items = self.files.len() + self.symlinks.len() + self.directories.len();
        let mut processed = 0usize;
        
        // Remove files in reverse order (deepest first)
        for file in self.files.iter().rev() {
            processed += 1;

            let actual_path = if file.path.exists() {
                file.path.clone()
            } else {
                // Try to find the file in common installation directories
                // This handles cases where manifests have incorrect paths
                let file_name = file.path.file_name().unwrap_or_default();
                let possible_paths = vec![
                    PathBuf::from("/usr/bin").join(file_name),
                    PathBuf::from("/usr/sbin").join(file_name),
                    PathBuf::from("/usr/lib").join(file_name),
                    PathBuf::from("/usr/lib64").join(file_name),
                    PathBuf::from("/bin").join(file_name),
                    PathBuf::from("/sbin").join(file_name),
                    PathBuf::from("/lib").join(file_name),
                    PathBuf::from("/lib64").join(file_name),
                ];

                possible_paths.into_iter().find(|p| p.exists()).unwrap_or_else(|| file.path.clone())
            };
            
            // Check if this is a critical system file
            if critical_dirs.iter().any(|&dir| actual_path.starts_with(dir) && actual_path != Path::new(dir)) {
                render_progress("Removing", processed, total_items, &format!("[SKIP] {}", actual_path.display()));
                continue;
            }
            
            if actual_path.exists() {
                // Check if file was modified (compare checksums) - skip this check for now since paths might be wrong
                if !purge {
                    // For non-purge removal, be more conservative
                    render_progress("Removing", processed, total_items, &format!("[SKIP] {}", actual_path.display()));
                        continue;
                }

                if let Err(_e) = fs::remove_file(&actual_path) {
                    render_progress("Removing", processed, total_items, &format!("[FAIL] {}", actual_path.display()));
                } else {
                    render_progress("Removing", processed, total_items, &format!("[OK] {}", actual_path.display()));
                }
            } else {
                render_progress("Removing", processed, total_items, &format!("[MISS] {}", actual_path.display()));
            }
        }

        // Remove symlinks
        for symlink in &self.symlinks {
            processed += 1;
            
            // Check if this is a critical system symlink
            if critical_dirs.iter().any(|&dir| symlink.path.starts_with(dir) && symlink.path != Path::new(dir)) {
                render_progress("Removing", processed, total_items, &format!("[SKIP] {}", symlink.path.display()));
                continue;
            }
            
            if symlink.path.exists() {
                if let Err(_e) = fs::remove_file(&symlink.path) {
                    render_progress("Removing", processed, total_items, &format!("[FAIL] {}", symlink.path.display()));
                } else {
                    render_progress("Removing", processed, total_items, &format!("[OK] {}", symlink.path.display()));
                }
            } else {
                render_progress("Removing", processed, total_items, &format!("[MISS] {}", symlink.path.display()));
            }
        }

        // Remove directories (only if empty and not critical)
        for dir in &self.directories {
            processed += 1;
            
            // Check if this is a critical system directory
            if critical_dirs.contains(&dir.path.to_str().unwrap_or("")) {
                render_progress("Removing", processed, total_items, &format!("[SKIP] {}", dir.path.display()));
                continue;
            }
            
            if dir.path.exists() {
                if let Err(e) = fs::remove_dir(&dir.path) {
                    // Directory not empty, that's fine
                    if e.kind() != std::io::ErrorKind::DirectoryNotEmpty {
                        render_progress("Removing", processed, total_items, &format!("[FAIL] {}", dir.path.display()));
                    } else {
                        render_progress("Removing", processed, total_items, &format!("[SKIP] {}", dir.path.display()));
                    }
                } else {
                    render_progress("Removing", processed, total_items, &format!("[OK] {}", dir.path.display()));
                }
            } else {
                render_progress("Removing", processed, total_items, &format!("[MISS] {}", dir.path.display()));
            }
        }

        Ok(())
    }

    pub fn check_conflicts(&self) -> Result<Vec<FileConflict>, String> {
        let mut conflicts = Vec::new();
        
        for file in &self.files {
            if file.path.exists() {
                // Check if file is owned by another package
                if let Ok(owner) = get_file_owner(&file.path) {
                    if owner != self.package_name {
                        conflicts.push(FileConflict {
                            path: file.path.clone(),
                            existing_owner: owner,
                            new_package: self.package_name.clone(),
                            conflict_type: ConflictType::FileOwnership,
                        });
                    }
                } else {
                    // File exists but not tracked by any package
                    conflicts.push(FileConflict {
                        path: file.path.clone(),
                        existing_owner: "unknown".to_string(),
                        new_package: self.package_name.clone(),
                        conflict_type: ConflictType::UntrackedFile,
                    });
                }
            }
        }
        
        for dir in &self.directories {
            if dir.path.exists() {
                if let Ok(owner) = get_file_owner(&dir.path) {
                    if owner != self.package_name {
                        conflicts.push(FileConflict {
                            path: dir.path.clone(),
                            existing_owner: owner,
                            new_package: self.package_name.clone(),
                            conflict_type: ConflictType::DirectoryOwnership,
                        });
                    }
                }
            }
        }
        
        for symlink in &self.symlinks {
            if symlink.path.exists() {
                if let Ok(owner) = get_file_owner(&symlink.path) {
                    if owner != self.package_name {
                        conflicts.push(FileConflict {
                            path: symlink.path.clone(),
                            existing_owner: owner,
                            new_package: self.package_name.clone(),
                            conflict_type: ConflictType::SymlinkOwnership,
                        });
                    }
                }
            }
        }
        
        Ok(conflicts)
    }

    pub fn backup_existing_files(&mut self) -> Result<(), String> {
        let backup_dir = get_backup_dir()?;
        fs::create_dir_all(&backup_dir).ok();

        for file in &mut self.files {
            if file.path.exists() {
                let backup_path = backup_dir.join(format!(
                    "{}_{}",
                    file.path.file_name().unwrap().to_string_lossy(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                ));

                if let Err(e) = fs::copy(&file.path, &backup_path) {
                    println!(
                        "\x1B[93m[WARN] Failed to backup file {}: {}\x1B[0m",
                        file.path.display(),
                        e
                    );
                } else {
                    file.backup_path = Some(backup_path);
                    println!("Backed up file: {}", file.path.display());
                }
            }
        }

        Ok(())
    }
}

pub fn calculate_file_checksum(path: &Path) -> Result<String, String> {
    use sha2::{Sha256, Digest};

    let mut file = File::open(path)
        .map_err(|_| format!("Failed to open file {}", path.display()))?;

    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)
            .map_err(|_| format!("Failed to read file {}", path.display()))?;
        
        if bytes_read == 0 {
            break;
        }

        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

pub fn get_backup_dir() -> Result<PathBuf, String> {
    let mut backup_dir = get_metadata_dir()?;
    backup_dir.push("backups");
    Ok(backup_dir)
}

pub fn cleanup_old_backups() -> Result<(), String> {
    let backup_dir = get_backup_dir()?;
    if !backup_dir.exists() {
        return Ok(());
    }

    let mut entries: Vec<_> = fs::read_dir(&backup_dir)
        .map_err(|_| "Failed to read backup directory")?
        .filter_map(|entry| entry.ok())
        .collect();

    // Sort by modification time (oldest first)
    entries.sort_by(|a, b| {
        a.metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
            .cmp(
                &b.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH),
            )
    });

    // Keep only the last 10 backups
    if entries.len() > 10 {
        for entry in entries.iter().take(entries.len() - 10) {
            if let Err(e) = fs::remove_file(entry.path()) {
                println!(
                    "\x1B[93m[WARN] Failed to remove old backup {}: {}\x1B[0m",
                    entry.path().display(),
                    e
                );
            }
        }
    }

    Ok(())
}

/// Get the package that owns a specific file
pub fn get_file_owner(path: &Path) -> Result<String, String> {
    let metadata_dir = get_metadata_dir()?;
    
    // Search through all installed package manifests
    for entry in fs::read_dir(&metadata_dir)
        .map_err(|e| format!("Failed to read metadata directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let entry_path = entry.path();
        
        if entry_path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Ok(content) = fs::read_to_string(&entry_path) {
                if let Ok(manifest) = serde_json::from_str::<FileManifest>(&content) {
                    // Check if this package owns the file
                    for file in &manifest.files {
                        if file.path == path {
                            return Ok(manifest.package_name.clone());
                        }
                    }
                    for dir in &manifest.directories {
                        if dir.path == path {
                            return Ok(manifest.package_name.clone());
                        }
                    }
                    for symlink in &manifest.symlinks {
                        if symlink.path == path {
                            return Ok(manifest.package_name.clone());
                        }
                    }
                }
            }
        }
    }
    
    Err("File not owned by any package".to_string())
}
