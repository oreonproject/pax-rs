use crate::database::{Database, ProvidesInfo};
use std::process::Command;

// Provides manager for tracking what packages provide
pub struct ProvidesManager {
    db: Database,
}

impl ProvidesManager {
    // Create new provides manager
    pub fn new(db: Database) -> Self {
        ProvidesManager { db }
    }

    // Add a provide entry
    pub fn add_provide(
        &self,
        package_id: i64,
        name: &str,
        version: Option<&str>,
        provide_type: &str,
    ) -> Result<(), String> {
        self.db.add_provides(package_id, name, version, provide_type)
            .map_err(|e| format!("Failed to add provide: {}", e))
    }

    // Query what provides a specific name
    pub fn query(&self, name: &str) -> Result<Vec<ProvidesInfo>, String> {
        self.db.query_provides(name)
            .map_err(|e| format!("Failed to query provides: {}", e))
    }

    // Check if a dependency is satisfied
    pub fn is_satisfied(&self, dep_name: &str) -> Result<bool, String> {
        // First check pax database
        let provides = self.query(dep_name)?;
        if !provides.is_empty() {
            return Ok(true);
        }

        // Check if it's a system binary
        if self.check_system_binary(dep_name) {
            return Ok(true);
        }

        // Check if it's a system library
        if self.check_system_library(dep_name) {
            return Ok(true);
        }

        Ok(false)
    }

    // Check if a binary exists on the system
    fn check_system_binary(&self, name: &str) -> bool {
        Command::new("which")
            .arg(name)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    // Check if a library exists on the system
    fn check_system_library(&self, name: &str) -> bool {
        // Try ldconfig to check for library
        if name.contains(".so") {
            let output = Command::new("ldconfig")
                .arg("-p")
                .output();
            
            if let Ok(output) = output {
                let stdout = String::from_utf8_lossy(&output.stdout);
                return stdout.contains(name);
            }
        }

        // Also check common lib directories
        let lib_dirs = [
            "/lib",
            "/lib64",
            "/usr/lib",
            "/usr/lib64",
            "/usr/local/lib",
            "/usr/local/lib64",
        ];

        for dir in &lib_dirs {
            let path = std::path::Path::new(dir).join(name);
            if path.exists() {
                return true;
            }
        }

        false
    }

    // Find which package provides something
    pub fn find_provider(&self, name: &str) -> Result<Option<String>, String> {
        let provides = self.query(name)?;
        
        if let Some(first) = provides.first() {
            Ok(Some(first.package_name.clone()))
        } else {
            Ok(None)
        }
    }

    // Check for conflicts between packages
    pub fn check_conflicts(
        &self,
        new_provides: &[(String, String)], // (name, type)
    ) -> Result<Vec<String>, String> {
        let mut conflicts = Vec::new();

        for (provide_name, _) in new_provides {
            let existing = self.query(provide_name)?;
            
            for existing_provide in existing {
                // Only files and binaries cause real conflicts
                if existing_provide.prov_type == "binary" 
                    || existing_provide.prov_type == "file" {
                    conflicts.push(format!(
                        "{} (provided by {})",
                        provide_name,
                        existing_provide.package_name
                    ));
                }
            }
        }

        Ok(conflicts)
    }

    // List all provides for a package
    pub fn list_package_provides(&self, package_id: i64) -> Result<Vec<String>, String> {
        // This would require a new db method
        // For now return empty
        Ok(Vec::new())
    }
}

// Helper to map common library names across distros
pub fn normalize_library_name(lib_name: &str) -> Vec<String> {
    let mut variants = vec![lib_name.to_string()];

    // Handle versioned libraries
    // e.g. libssl.so.1.1 -> libssl.so, libssl.so.1, libssl.so.1.1
    if lib_name.contains(".so") {
        if let Some(base_idx) = lib_name.find(".so") {
            let base = &lib_name[..base_idx + 3];
            variants.push(base.to_string());
            
            // Add versioned variants
            if lib_name.len() > base.len() {
                let version_part = &lib_name[base.len()..];
                if version_part.starts_with('.') {
                    let version_nums: Vec<&str> = version_part[1..].split('.').collect();
                    
                    for i in 1..=version_nums.len() {
                        let partial_version = version_nums[..i].join(".");
                        variants.push(format!("{}.{}", base, partial_version));
                    }
                }
            }
        }
    }

    // Handle distro-specific naming
    // e.g. openssl -> libssl
    match lib_name {
        "openssl" | "ssl" => {
            variants.push("libssl".to_string());
            variants.push("libssl.so".to_string());
        }
        "crypto" => {
            variants.push("libcrypto".to_string());
            variants.push("libcrypto.so".to_string());
        }
        "zlib" | "z" => {
            variants.push("libz".to_string());
            variants.push("libz.so".to_string());
        }
        _ => {}
    }

    variants
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_library_name() {
        let variants = normalize_library_name("libssl.so.1.1");
        assert!(variants.contains(&"libssl.so".to_string()));
        assert!(variants.contains(&"libssl.so.1".to_string()));
        assert!(variants.contains(&"libssl.so.1.1".to_string()));
    }

    #[test]
    fn test_normalize_common_libs() {
        let variants = normalize_library_name("openssl");
        assert!(variants.contains(&"libssl".to_string()));
        assert!(variants.contains(&"libssl.so".to_string()));
    }
}

