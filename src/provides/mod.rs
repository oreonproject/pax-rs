use crate::database::{Database, ProvidesInfo};
use crate::distro_compat::DistroCompat;
use std::process::Command;

// Provides manager for tracking what packages provide
pub struct ProvidesManager {
    db: Database,
    distro_compat: DistroCompat,
}

impl ProvidesManager {
    // Create new provides manager
    pub fn new(db: Database) -> Self {
        ProvidesManager { 
            db,
            distro_compat: DistroCompat::new(),
        }
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

        // Check for special runtime linker symbols
        if dep_name.starts_with("rtld(") || dep_name.starts_with("ld-linux") {
            // Runtime linker symbols are always provided by the system
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

        // Check if it's a system package (with distro translation)
        if self.distro_compat.is_dependency_satisfied(dep_name) {
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
        // Parse the library name to extract the base library
        // Handle RPM-style dependencies like: libc.so.6(GLIBC_2.34)(64bit)
        let lib_name = Self::parse_library_name(name);
        
        // Try ldconfig to check for library
        if lib_name.contains(".so") {
            let output = Command::new("ldconfig")
                .arg("-p")
                .output();
            
            if let Ok(output) = output {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Check if the base library name is found
                if stdout.lines().any(|line| {
                    line.contains(&lib_name) || line.contains(name)
                }) {
                    return true;
                }
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
            // Try exact match first
            let exact_path = std::path::Path::new(dir).join(name);
            if exact_path.exists() {
                return true;
            }
            
            // Try base library name
            let base_path = std::path::Path::new(dir).join(&lib_name);
            if base_path.exists() {
                return true;
            }
            
            // Check for any version of the library
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Ok(file_name) = entry.file_name().into_string() {
                        if file_name.starts_with(&lib_name) {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }
    
    // Parse library name from RPM/DEB dependency format
    // Examples:
    //   libc.so.6(GLIBC_2.34)(64bit) -> libc.so.6
    //   libssl.so.1.1 -> libssl.so.1.1
    //   rtld(GNU_HASH) -> rtld
    fn parse_library_name(dep_name: &str) -> String {
        // Remove anything in parentheses (version symbols, architecture)
        if let Some(paren_pos) = dep_name.find('(') {
            dep_name[..paren_pos].to_string()
        } else {
            dep_name.to_string()
        }
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
    pub fn list_package_provides(&self, _package_id: i64) -> Result<Vec<String>, String> {
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

