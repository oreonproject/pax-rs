use crate::database::Database;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

const SYSTEM_BIN_DIR: &str = "/usr/local/bin";
const SYSTEM_LIB_DIR: &str = "/usr/local/lib";

// Symlink manager for creating and managing symlinks
pub struct SymlinkManager {
    db: Database,
    links_base: PathBuf,
}

impl SymlinkManager {
    // Create new symlink manager
    pub fn new(db: Database, links_base: &str) -> Self {
        SymlinkManager {
            db,
            links_base: PathBuf::from(links_base),
        }
    }

    // Create symlinks for a package's files
    pub fn create_symlinks(
        &self,
        package_id: i64,
        package_hash: &str,
        store_path: &Path,
        files: &[String],
    ) -> Result<Vec<(String, String)>, String> {
        let mut created_links = Vec::new();

        for file in files {
            let source = store_path.join(package_hash).join(file);
            
            if !source.exists() {
                continue;
            }

            // Determine link location based on file type
            if let Some(link_path) = self.determine_link_path(file) {
                // Create intermediate symlink in pax links directory
                let intermediate_link = self.links_base.join(&link_path);
                
                // Ensure parent directory exists
                if let Some(parent) = intermediate_link.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("Failed to create directory: {}", e))?;
                }

                // Create symlink
                if intermediate_link.exists() {
                    // Check if its a symlink we can replace
                    if intermediate_link.is_symlink() {
                        fs::remove_file(&intermediate_link)
                            .map_err(|e| format!("Failed to remove existing symlink: {}", e))?;
                    } else {
                        return Err(format!(
                            "Cannot create symlink: {} already exists and is not a symlink",
                            intermediate_link.display()
                        ));
                    }
                }

                // Create symlink from links dir to store
                symlink(&source, &intermediate_link)
                    .map_err(|e| format!("Failed to create symlink: {}", e))?;

                // Store in database
                self.db.add_symlink(
                    package_id,
                    intermediate_link.to_str().unwrap(),
                    source.to_str().unwrap(),
                ).map_err(|e| format!("Failed to add symlink to database: {}", e))?;

                // Create system symlink if appropriate
                if let Some(system_link) = self.determine_system_link(&link_path) {
                    if let Err(e) = self.create_system_symlink(&intermediate_link, &system_link) {
                        eprintln!("Warning: Failed to create system symlink: {}", e);
                        // not fatal, continue
                    } else {
                        created_links.push((
                            intermediate_link.to_string_lossy().to_string(),
                            system_link.to_string_lossy().to_string(),
                        ));
                    }
                }

                created_links.push((
                    source.to_string_lossy().to_string(),
                    intermediate_link.to_string_lossy().to_string(),
                ));
            }
        }

        Ok(created_links)
    }

    // Determine where a file should be linked in the links directory
    fn determine_link_path(&self, file_path: &str) -> Option<String> {
        let path = Path::new(file_path);
        
        // Check if file is in bin/, lib/, or share/
        if let Some(parent) = path.parent() {
            let parent_str = parent.to_string_lossy();
            
            if parent_str.contains("bin") {
                return path.file_name()
                    .map(|name| format!("bin/{}", name.to_string_lossy()));
            } else if parent_str.contains("lib") {
                return path.file_name()
                    .map(|name| format!("lib/{}", name.to_string_lossy()));
            } else if parent_str.contains("share") {
                // Preserve share subdirectory structure
                if let Ok(relative) = path.strip_prefix("usr/share").or_else(|_| path.strip_prefix("share")) {
                    return Some(format!("share/{}", relative.to_string_lossy()));
                }
            }
        }

        None
    }

    // Determine if a file should also be linked in system directories
    fn determine_system_link(&self, link_path: &str) -> Option<PathBuf> {
        if link_path.starts_with("bin/") {
            let filename = &link_path[4..];
            Some(PathBuf::from(SYSTEM_BIN_DIR).join(filename))
        } else if link_path.starts_with("lib/") {
            let filename = &link_path[4..];
            Some(PathBuf::from(SYSTEM_LIB_DIR).join(filename))
        } else {
            None
        }
    }

    // Create symlink in system directory
    fn create_system_symlink(&self, source: &Path, target: &Path) -> Result<(), String> {
        // Check if target already exists
        if target.exists() {
            if target.is_symlink() {
                // Remove old symlink
                fs::remove_file(target)
                    .map_err(|e| format!("Failed to remove existing symlink: {}", e))?;
            } else {
                return Err(format!("Target exists and is not a symlink: {}", target.display()));
            }
        }

        // Ensure parent directory exists
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        symlink(source, target)
            .map_err(|e| format!("Failed to create system symlink: {}", e))?;

        Ok(())
    }

    // Remove symlinks for a package
    pub fn remove_symlinks(&self, package_id: i64) -> Result<(), String> {
        let symlinks = self.db.get_symlinks(package_id)
            .map_err(|e| format!("Failed to get symlinks: {}", e))?;

        for link in symlinks {
            let link_path = PathBuf::from(&link.link_path);
            
            if link_path.exists() && link_path.is_symlink() {
                fs::remove_file(&link_path)
                    .map_err(|e| format!("Failed to remove symlink: {}", e))?;
            }

            // Also remove system symlink if it points to our link
            if let Some(system_link) = self.determine_system_link(&link.link_path) {
                if system_link.exists() && system_link.is_symlink() {
                    if let Ok(target) = fs::read_link(&system_link) {
                        if target == link_path {
                            let _ = fs::remove_file(&system_link);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // Update symlinks (for package upgrades)
    pub fn update_symlinks(
        &self,
        package_id: i64,
        package_hash: &str,
        store_path: &Path,
        files: &[String],
    ) -> Result<(), String> {
        // Remove old symlinks
        self.remove_symlinks(package_id)?;
        
        // Create new symlinks
        self.create_symlinks(package_id, package_hash, store_path, files)?;
        
        Ok(())
    }

    // Clean up orphaned symlinks
    pub fn cleanup_orphaned(&self) -> Result<Vec<String>, String> {
        let mut cleaned = Vec::new();

        // Check bin directory
        for entry in ["bin", "lib", "share"] {
            let dir = self.links_base.join(entry);
            if !dir.exists() {
                continue;
            }

            for entry in fs::read_dir(&dir)
                .map_err(|e| format!("Failed to read directory: {}", e))? {
                let entry = entry
                    .map_err(|e| format!("Failed to read entry: {}", e))?;
                let path = entry.path();

                if path.is_symlink() {
                    // Check if symlink target exists
                    if let Ok(target) = fs::read_link(&path) {
                        if !target.exists() {
                            // Orphaned symlink
                            fs::remove_file(&path)
                                .map_err(|e| format!("Failed to remove orphaned symlink: {}", e))?;
                            cleaned.push(path.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }

        Ok(cleaned)
    }

    // Run ldconfig to update library cache
    pub fn update_library_cache(&self) -> Result<(), String> {
        let output = std::process::Command::new("ldconfig")
            .output()
            .map_err(|e| format!("Failed to run ldconfig: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("ldconfig failed: {}", stderr));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_link_path() {
        let db = Database::open(":memory:").unwrap();
        let manager = SymlinkManager::new(db, "/opt/pax/links");

        assert_eq!(
            manager.determine_link_path("usr/bin/test"),
            Some("bin/test".to_string())
        );
        
        assert_eq!(
            manager.determine_link_path("usr/lib/libtest.so"),
            Some("lib/libtest.so".to_string())
        );
    }
}

