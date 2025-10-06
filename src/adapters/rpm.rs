use super::*;
use std::path::{Path, PathBuf};

// RPM package adapter
pub struct RpmAdapter {
    path: PathBuf,
}

impl RpmAdapter {
    // Create new RPM adapter from file
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        
        if !path.exists() {
            return Err(format!("File does not exist: {}", path.display()));
        }

        Ok(RpmAdapter { path })
    }
}

impl PackageAdapter for RpmAdapter {
    fn extract_metadata(&self) -> Result<PackageMetadata, String> {
        // Simplified RPM metadata extraction
        // In a full implementation, we'd parse the RPM header
        // For now, extract basic info from filename
        
        let filename = self.path.file_stem()
            .and_then(|s| s.to_str())
            .ok_or("Invalid filename")?;
        
        // Try to parse filename: package-version-release.arch
        let parts: Vec<&str> = filename.split('-').collect();
        let name = parts.get(0).unwrap_or(&"unknown").to_string();
        let version = parts.get(1).unwrap_or(&"unknown").to_string();
        
        Ok(PackageMetadata {
            name,
            version,
            description: "RPM package (limited metadata - install rpm-python for full support)".to_string(),
            origin: "rpm".to_string(),
            dependencies: Vec::new(),
            runtime_dependencies: Vec::new(),
            provides: Vec::new(),
            conflicts: Vec::new(),
        })
    }

    fn extract_files(&self, dest_dir: &Path) -> Result<Vec<FileEntry>, String> {
        // RPM files contain a cpio archive - we need to extract it
        // The rpm crate doesn't provide direct extraction, so we shell out to rpm2cpio
        
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("Failed to create dest dir: {}", e))?;

        // Try using rpm2cpio + cpio command
        // First check if rpm2cpio is available
        let rpm2cpio_check = std::process::Command::new("which")
            .arg("rpm2cpio")
            .output();

        if rpm2cpio_check.is_ok() && rpm2cpio_check.unwrap().status.success() {
            // Use rpm2cpio for extraction
            self.extract_with_rpm2cpio(dest_dir)?;
        } else {
            // Fallback: try to extract manually from RPM payload
            self.extract_payload_manual(dest_dir)?;
        }

        // List extracted files
        let mut entries = Vec::new();
        for entry in walkdir::WalkDir::new(dest_dir) {
            let entry = entry.map_err(|e| format!("Failed to walk directory: {}", e))?;
            if entry.path() == dest_dir {
                continue;
            }

            let relative_path = entry.path()
                .strip_prefix(dest_dir)
                .unwrap()
                .to_string_lossy()
                .to_string();

            let file_type = if entry.file_type().is_dir() {
                FileType::Directory
            } else if entry.file_type().is_symlink() {
                FileType::Symlink
            } else {
                FileType::Regular
            };

            entries.push(FileEntry {
                path: relative_path,
                file_type,
            });
        }

        Ok(entries)
    }

    fn get_dependencies(&self) -> Result<Vec<Dependency>, String> {
        Ok(self.extract_metadata()?.dependencies)
    }

    fn get_provides(&self) -> Result<Vec<Provides>, String> {
        Ok(self.extract_metadata()?.provides)
    }

    fn run_script(&self, _stage: ScriptStage) -> Result<(), String> {
        // RPM scriptlets - we need to be careful running these to prevent our system from being fucked
        // For now, we'll just log that they exist
        // Note: With simplified RPM parsing, we can't extract scriptlets
        // This would require full RPM header parsing
        
        Ok(())
    }

    fn get_hash(&self) -> Result<String, String> {
        crate::crypto::calculate_sha256(&self.path)
            .map_err(|e| format!("Failed to calculate hash: {}", e))
    }
}

// Helper methods for RPM extraction (not part of trait)
impl RpmAdapter {
    // Extract using rpm2cpio command
    fn extract_with_rpm2cpio(&self, dest_dir: &Path) -> Result<(), String> {
        use std::process::{Command, Stdio};

        let rpm2cpio = Command::new("rpm2cpio")
            .arg(&self.path)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn rpm2cpio: {}", e))?;

        let cpio = Command::new("cpio")
            .arg("-idm")
            .current_dir(dest_dir)
            .stdin(rpm2cpio.stdout.ok_or("Failed to get rpm2cpio stdout")?)
            .output()
            .map_err(|e| format!("Failed to run cpio: {}", e))?;

        if !cpio.status.success() {
            return Err(format!("cpio extraction failed: {}", 
                String::from_utf8_lossy(&cpio.stderr)));
        }

        Ok(())
    }

    // Fallback manual extraction (limited, mainly for testing)
    fn extract_payload_manual(&self, dest_dir: &Path) -> Result<(), String> {
        // This is a simplified fallback
        // In practice, proper RPM extraction requires handling the CPIO format
        // which is complex, so we prefer rpm2cpio when available
        
        println!("Warning: rpm2cpio not found, using limited extraction");
        println!("Install rpm2cpio for full RPM support");
        println!("Note: Manual RPM extraction is not fully implemented");
        
        // Just create the destination directory
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("Failed to create directory: {}", e))?;

        Ok(())
    }
}

