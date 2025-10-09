use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

// Debian package adapter using dpkg/dpkg-deb backend
pub struct DebAdapter {
    path: PathBuf,
}

impl DebAdapter {
    // Create new DEB adapter from file
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        
        if !path.exists() {
            return Err(format!("File does not exist: {}", path.display()));
        }

        // Verify dpkg-deb is available
        Self::check_dpkg_available()?;

        Ok(DebAdapter { path })
    }

    // Check if dpkg-deb command is available
    fn check_dpkg_available() -> Result<(), String> {
        let output = Command::new("which")
            .arg("dpkg-deb")
            .output();
        
        match output {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err("dpkg-deb command not found. Please install dpkg tools.".to_string())
        }
    }

    // Read control file from .deb package using dpkg-deb
    fn read_control_file(&self) -> Result<String, String> {
        let output = Command::new("dpkg-deb")
            .args(&["-f", self.path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to run dpkg-deb: {}", e))?;
        
        if !output.status.success() {
            return Err(format!("dpkg-deb failed: {}", String::from_utf8_lossy(&output.stderr)));
        }
        
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    // Parse debian control file format
    fn parse_control_field(content: &str, field: &str) -> Option<String> {
        for line in content.lines() {
            if let Some(value) = line.strip_prefix(&format!("{}: ", field)) {
                return Some(value.trim().to_string());
            }
        }
        None
    }

    // Parse debian dependencies
    fn parse_dependencies(dep_string: &str) -> Vec<Dependency> {
        let mut deps = Vec::new();
        
        // Debian deps format: "pkg1 (>= 1.0), pkg2, pkg3 (<< 2.0)"
        for dep in dep_string.split(',') {
            let dep = dep.trim();
            
            // Skip alternative dependencies (containing |)
            if dep.contains('|') {
                continue;
            }
            
            // Parse version constraint
            if let Some(paren_start) = dep.find('(') {
                let name = dep[..paren_start].trim().to_string();
                let version_part = &dep[paren_start+1..];
                
                if let Some(paren_end) = version_part.find(')') {
                    let constraint = version_part[..paren_end].trim();
                    deps.push(Dependency {
                        name,
                        version_constraint: Some(constraint.to_string()),
                        dep_type: DependencyType::Runtime,
                    });
                    continue;
                }
            }
            
            // no version constraint
            if !dep.is_empty() {
                deps.push(Dependency {
                    name: dep.to_string(),
                    version_constraint: None,
                    dep_type: DependencyType::Runtime,
                });
            }
        }
        
        deps
    }

    // Extract provides from control file
    fn extract_provides(&self, control: &str, package_name: &str, version: &str) -> Vec<Provides> {
        let mut provides = Vec::new();
        
        // Package name itself is a provide
        provides.push(Provides {
            name: package_name.to_string(),
            version: Some(version.to_string()),
            provide_type: ProvideType::Virtual,
        });

        // Check for explicit provides field
        if let Some(provides_str) = Self::parse_control_field(control, "Provides") {
            for provide in provides_str.split(',') {
                let provide = provide.trim();
                if !provide.is_empty() {
                    provides.push(Provides {
                        name: provide.to_string(),
                        version: None,
                        provide_type: ProvideType::Virtual,
                    });
                }
            }
        }

        provides
    }
}

impl PackageAdapter for DebAdapter {
    fn extract_metadata(&self) -> Result<PackageMetadata, String> {
        let control = self.read_control_file()?;
        
        let name = Self::parse_control_field(&control, "Package")
            .ok_or("Missing Package field")?;
        
        let version = Self::parse_control_field(&control, "Version")
            .ok_or("Missing Version field")?;
        
        let description = Self::parse_control_field(&control, "Description")
            .unwrap_or_else(|| "No description".to_string());

        let dependencies = if let Some(deps_str) = Self::parse_control_field(&control, "Depends") {
            Self::parse_dependencies(&deps_str)
        } else {
            Vec::new()
        };

        let provides = self.extract_provides(&control, &name, &version);

        let conflicts = if let Some(conflicts_str) = Self::parse_control_field(&control, "Conflicts") {
            conflicts_str.split(',').map(|s| s.trim().to_string()).collect()
        } else {
            Vec::new()
        };

        Ok(PackageMetadata {
            name,
            version,
            description,
            origin: "deb".to_string(),
            dependencies: dependencies.clone(),
            runtime_dependencies: dependencies,
            provides,
            conflicts,
        })
    }

    fn extract_files(&self, dest_dir: &Path) -> Result<Vec<FileEntry>, String> {
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("Failed to create dest dir: {}", e))?;

        // Extract using dpkg-deb -x
        let output = Command::new("dpkg-deb")
            .args(&["-x", self.path.to_str().unwrap(), dest_dir.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to run dpkg-deb: {}", e))?;
        
        if !output.status.success() {
            return Err(format!("dpkg-deb extraction failed: {}", String::from_utf8_lossy(&output.stderr)));
        }

        // List files using dpkg-deb -c
        let output = Command::new("dpkg-deb")
            .args(&["-c", self.path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to list package contents: {}", e))?;
        
        if !output.status.success() {
            return Err(format!("dpkg-deb listing failed: {}", String::from_utf8_lossy(&output.stderr)));
        }

        let mut entries = Vec::new();
        let listing = String::from_utf8_lossy(&output.stdout);
        
        for line in listing.lines() {
            // Parse dpkg-deb -c output format: "permissions user/group size date time ./path"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 6 {
                continue;
            }
            
            let path_str = parts[parts.len() - 1];
            let path = path_str.trim_start_matches("./");
            
            if path.is_empty() || path == "." {
                continue;
            }
            
            // Determine file type from first character of permissions
            let file_type = if parts[0].starts_with('d') {
                FileType::Directory
            } else if parts[0].starts_with('l') {
                FileType::Symlink
            } else {
                FileType::Regular
            };

            entries.push(FileEntry {
                path: path.to_string(),
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

    fn run_script(&self, stage: ScriptStage) -> Result<(), String> {
        use tempfile::TempDir;
        
        let script_name = match stage {
            ScriptStage::PreInstall => "preinst",
            ScriptStage::PostInstall => "postinst",
            ScriptStage::PreRemove => "prerm",
            ScriptStage::PostRemove => "postrm",
        };

        // Extract control archive to temp directory
        let temp_dir = TempDir::new()
            .map_err(|e| format!("Failed to create temp directory: {}", e))?;
        
        let output = Command::new("dpkg-deb")
            .args(&["--control", self.path.to_str().unwrap(), temp_dir.path().to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to extract control: {}", e))?;
        
        if !output.status.success() {
            return Err(format!("Failed to extract control scripts: {}", String::from_utf8_lossy(&output.stderr)));
        }

        let script_path = temp_dir.path().join(script_name);
        
        if !script_path.exists() {
            return Ok(());
        }

        println!("Running Debian {} script...", script_name);
        
        // Make script executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)
                .map_err(|e| format!("Failed to get script permissions: {}", e))?
                .permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(&script_path, perms)
                .map_err(|e| format!("Failed to set script permissions: {}", e))?;
        }
        
        // Execute script
        let output = Command::new(&script_path)
            .arg("install")
            .output()
            .map_err(|e| format!("Failed to execute {} script: {}", script_name, e))?;
        
        if !output.status.success() {
            eprintln!("Warning: {} script failed: {}", script_name, String::from_utf8_lossy(&output.stderr));
        } else {
            println!("{} script executed successfully", script_name);
        }
        
        Ok(())
    }

    fn get_hash(&self) -> Result<String, String> {
        crate::crypto::calculate_sha256(&self.path)
            .map_err(|e| format!("Failed to calculate hash: {}", e))
    }
}

