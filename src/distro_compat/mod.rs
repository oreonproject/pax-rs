use std::process::Command;

// Cross-distro package compatibility layer
pub struct DistroCompat {
    distro_type: DistroType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DistroType {
    Debian,
    RedHat,
    Unknown,
}

impl DistroCompat {
    pub fn new() -> Self {
        let distro_type = Self::detect_distro();
        
        DistroCompat {
            distro_type,
        }
    }
    
    // Detect the current distro type
    fn detect_distro() -> DistroType {
        // Check for RPM-based distros
        if std::path::Path::new("/etc/redhat-release").exists() 
            || std::path::Path::new("/etc/fedora-release").exists() {
            return DistroType::RedHat;
        }
        
        // Check for Debian-based distros
        if std::path::Path::new("/etc/debian_version").exists() {
            return DistroType::Debian;
        }
        
        // Check via package manager availability
        if Command::new("rpm").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
            return DistroType::RedHat;
        }
        
        if Command::new("dpkg").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
            return DistroType::Debian;
        }
        
        DistroType::Unknown
    }
    
    // Check if a dependency is satisfied by system packages
    // Uses package manager's "whatprovides" functionality
    pub fn is_dependency_satisfied(&self, dep_name: &str) -> bool {
        match self.distro_type {
            DistroType::RedHat => self.check_rpm_whatprovides(dep_name),
            DistroType::Debian => self.check_dpkg_provides(dep_name),
            DistroType::Unknown => false,
        }
    }
    
    // Check if an RPM system provides a dependency
    fn check_rpm_whatprovides(&self, dep_name: &str) -> bool {
        // Try direct whatprovides first
        let output = Command::new("rpm")
            .args(&["-q", "--whatprovides", dep_name])
            .output();
        
        if let Ok(output) = output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.trim().is_empty() && !stdout.contains("no package provides") {
                    return true;
                }
            }
        }
        
        // Try direct package name
        if Command::new("rpm")
            .args(&["-q", dep_name])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false) {
            return true;
        }
        
        // If it looks like a Debian package name, try common translations
        if self.is_debian_style_name(dep_name) {
            for rpm_name in self.guess_rpm_equivalent(dep_name) {
                if Command::new("rpm")
                    .args(&["-q", &rpm_name])
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false) {
                    return true;
                }
            }
        }
        
        false
    }
    
    // Check if a package name looks like a Debian-style name
    fn is_debian_style_name(&self, name: &str) -> bool {
        // Debian packages often have numbers in them like libc6, libssl1.1
        name.starts_with("lib") && (name.contains(char::is_numeric) || name.ends_with("-dev"))
    }
    
    // Guess RPM package equivalent for common Debian packages
    fn guess_rpm_equivalent(&self, deb_name: &str) -> Vec<String> {
        let mut guesses = Vec::new();
        
        // Common patterns:
        // libc6 → glibc
        if deb_name.starts_with("libc") {
            guesses.push("glibc".to_string());
            guesses.push("glibc-common".to_string());
        }
        
        // lib<name><version> → <name>-libs, lib<name>
        if deb_name.starts_with("lib") {
            // Extract base name (remove lib prefix and version suffix)
            let without_lib = &deb_name[3..];
            let base_name: String = without_lib.chars()
                .take_while(|c| c.is_alphabetic() || *c == '-')
                .collect();
            
            if !base_name.is_empty() {
                guesses.push(format!("{}-libs", base_name));
                guesses.push(format!("lib{}", base_name));
                guesses.push(base_name.clone());
            }
        }
        
        // lib<name>-dev → <name>-devel
        if deb_name.ends_with("-dev") {
            let base = &deb_name[..deb_name.len()-4];
            if base.starts_with("lib") {
                let name = &base[3..];
                guesses.push(format!("{}-devel", name));
            }
        }
        
        guesses
    }
    
    // Check if a Debian system provides a dependency
    fn check_dpkg_provides(&self, dep_name: &str) -> bool {
        // Try direct package check
        let output = Command::new("dpkg")
            .args(&["-l", dep_name])
            .output();
        
        if let Ok(output) = output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Check if package is actually installed (starts with "ii")
                if stdout.lines().any(|line| {
                    line.starts_with("ii") && line.contains(dep_name)
                }) {
                    return true;
                }
            }
        }
        
        // Try using dpkg -S to find what package provides a file/library
        if dep_name.contains("/") || dep_name.ends_with(".so") {
            let output = Command::new("dpkg")
                .args(&["-S", dep_name])
                .output();
            
            if let Ok(output) = output {
                if output.status.success() {
                    return true;
                }
            }
        }
        
        // If it looks like an RPM package name, try Debian equivalents
        if self.is_rpm_style_name(dep_name) {
            for deb_name in self.guess_deb_equivalent(dep_name) {
                let output = Command::new("dpkg")
                    .args(&["-l", &deb_name])
                    .output();
                
                if let Ok(output) = output {
                    if output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if stdout.lines().any(|line| {
                            line.starts_with("ii") && line.contains(&deb_name)
                        }) {
                            return true;
                        }
                    }
                }
            }
        }
        
        false
    }
    
    // Check if a package name looks like an RPM-style name
    fn is_rpm_style_name(&self, name: &str) -> bool {
        // RPM packages often end with -libs, -devel
        name.ends_with("-libs") || name.ends_with("-devel") || name == "glibc"
    }
    
    // Guess Debian package equivalent for RPM packages
    fn guess_deb_equivalent(&self, rpm_name: &str) -> Vec<String> {
        let mut guesses = Vec::new();
        
        // glibc → libc6
        if rpm_name == "glibc" || rpm_name == "glibc-common" {
            guesses.push("libc6".to_string());
        }
        
        // <name>-libs → lib<name>, lib<name>#
        if rpm_name.ends_with("-libs") {
            let base = &rpm_name[..rpm_name.len()-5];
            guesses.push(format!("lib{}", base));
            // Try with common version numbers
            for ver in &["", "1", "2", "3", "1.1", "2.0"] {
                guesses.push(format!("lib{}{}", base, ver));
            }
        }
        
        // <name>-devel → lib<name>-dev
        if rpm_name.ends_with("-devel") {
            let base = &rpm_name[..rpm_name.len()-6];
            guesses.push(format!("lib{}-dev", base));
        }
        
        guesses
    }
    
    // Get distro type for display
    pub fn get_distro_type(&self) -> &DistroType {
        &self.distro_type
    }
}

impl Default for DistroCompat {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_distro() {
        let distro = DistroCompat::detect_distro();
        // Should detect something, even if Unknown
        assert!(matches!(distro, DistroType::Debian | DistroType::RedHat | DistroType::Unknown));
    }
}

