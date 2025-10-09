use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

// RPM package adapter using rpm command as backend
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

        // Verify rpm command is available
        Self::check_rpm_available()?;

        Ok(RpmAdapter { path })
    }

    // Check if rpm command is available
    fn check_rpm_available() -> Result<(), String> {
        let output = Command::new("which")
            .arg("rpm")
            .output();
        
        match output {
            Ok(output) if output.status.success() => Ok(()),
            _ => Err("rpm command not found. Please install rpm tools.".to_string())
        }
    }

    // Query RPM metadata field
    fn query_rpm(&self, format: &str) -> Result<String, String> {
        let output = Command::new("rpm")
            .args(&["-qp", "--queryformat", format, self.path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to run rpm: {}", e))?;
        
        if !output.status.success() {
            return Err(format!("rpm query failed: {}", String::from_utf8_lossy(&output.stderr)));
        }
        
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    // Query RPM list field (like requires, provides)
    fn query_rpm_list(&self, flag: &str) -> Result<Vec<String>, String> {
        let output = Command::new("rpm")
            .args(&["-qp", flag, self.path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to run rpm: {}", e))?;
        
        if !output.status.success() {
            return Err(format!("rpm query failed: {}", String::from_utf8_lossy(&output.stderr)));
        }
        
        let result = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter(|line| !line.starts_with("rpmlib("))
            .map(|line| {
                // Extract just the package name, removing version constraints
                line.split_whitespace()
                    .next()
                    .unwrap_or(line)
                    .to_string()
            })
            .collect();
        
        Ok(result)
    }
}

impl PackageAdapter for RpmAdapter {
    fn extract_metadata(&self) -> Result<PackageMetadata, String> {
        // Query RPM metadata using rpm command
        let name = self.query_rpm("%{NAME}")?;
        let version = self.query_rpm("%{VERSION}-%{RELEASE}")?;
        let description = self.query_rpm("%{SUMMARY}")?;
        
        // Get dependencies (requires)
        let dep_names = self.query_rpm_list("--requires")?;
        let dependencies: Vec<Dependency> = dep_names.into_iter()
            .map(|name| Dependency {
                name,
                version_constraint: None,
                dep_type: DependencyType::Runtime,
            })
            .collect();
        
        // Get provides
        let provide_names = self.query_rpm_list("--provides")?;
        let mut provides: Vec<Provides> = provide_names.into_iter()
            .map(|name| Provides {
                name,
                version: None,
                provide_type: ProvideType::Virtual,
            })
            .collect();
        
        // Add package name itself as a provide
        if !provides.iter().any(|p| p.name == name) {
            provides.insert(0, Provides {
                name: name.clone(),
                version: Some(version.clone()),
                provide_type: ProvideType::Virtual,
            });
        }
        
        // Get conflicts
        let conflicts = self.query_rpm_list("--conflicts")
            .unwrap_or_default();
        
        Ok(PackageMetadata {
            name,
            version,
            description,
            origin: "rpm".to_string(),
            dependencies: dependencies.clone(),
            runtime_dependencies: dependencies,
            provides,
            conflicts,
        })
    }

    fn extract_files(&self, dest_dir: &Path) -> Result<Vec<FileEntry>, String> {
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("Failed to create dest dir: {}", e))?;

        // Extract using rpm2cpio + cpio
        let rpm2cpio = Command::new("rpm2cpio")
            .arg(self.path.to_str().unwrap())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to run rpm2cpio: {}", e))?;

        let cpio = Command::new("cpio")
            .args(&["-idm", "--quiet"])
            .current_dir(dest_dir)
            .stdin(rpm2cpio.stdout.ok_or("Failed to get rpm2cpio stdout")?)
            .output()
            .map_err(|e| format!("Failed to run cpio: {}", e))?;

        if !cpio.status.success() {
            return Err(format!("cpio extraction failed: {}", String::from_utf8_lossy(&cpio.stderr)));
        }

        // List files using rpm -qpl
        let output = Command::new("rpm")
            .args(&["-qpl", self.path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to list RPM contents: {}", e))?;
        
        if !output.status.success() {
            return Err(format!("rpm listing failed: {}", String::from_utf8_lossy(&output.stderr)));
        }

        let mut entries = Vec::new();
        let listing = String::from_utf8_lossy(&output.stdout);
        
        for line in listing.lines() {
            let path = line.trim();
            
            if path.is_empty() {
                continue;
            }
            
            // Remove leading slash to make relative
            let relative_path = path.strip_prefix('/').unwrap_or(path);
            
            if relative_path.is_empty() {
                continue;
            }
            
            // Determine file type from filesystem after extraction
            let full_path = dest_dir.join(relative_path);
            let file_type = if full_path.is_dir() {
                FileType::Directory
            } else if full_path.is_symlink() {
                FileType::Symlink
            } else {
                FileType::Regular
            };

            entries.push(FileEntry {
                path: relative_path.to_string(),
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
        use tempfile::NamedTempFile;
        use std::io::Write;
        
        let script_flag = match stage {
            ScriptStage::PreInstall => "--script=prein",
            ScriptStage::PostInstall => "--script=postin",
            ScriptStage::PreRemove => "--script=preun",
            ScriptStage::PostRemove => "--script=postun",
        };

        // Query scriptlet content
        let output = Command::new("rpm")
            .args(&["-qp", script_flag, self.path.to_str().unwrap()])
            .output()
            .map_err(|e| format!("Failed to query RPM scriptlet: {}", e))?;
        
        if !output.status.success() || output.stdout.is_empty() {
            return Ok(());
        }

        let script_content = String::from_utf8_lossy(&output.stdout);
        
        if script_content.trim().is_empty() {
            return Ok(());
        }

        println!("Running RPM {:?} scriptlet...", stage);

        // Write script to temporary file and execute
        let mut temp_file = NamedTempFile::new()
            .map_err(|e| format!("Failed to create temp script file: {}", e))?;
        
        temp_file.write_all(script_content.as_bytes())
            .map_err(|e| format!("Failed to write script: {}", e))?;
        
        // Make script executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(temp_file.path())
                .map_err(|e| format!("Failed to get script permissions: {}", e))?
                .permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(temp_file.path(), perms)
                .map_err(|e| format!("Failed to set script permissions: {}", e))?;
        }
        
        // Execute script with sh
        let output = Command::new("sh")
            .arg(temp_file.path())
            .output()
            .map_err(|e| format!("Failed to execute scriptlet: {}", e))?;
        
        if !output.status.success() {
            eprintln!("Warning: {:?} scriptlet failed: {}", stage, String::from_utf8_lossy(&output.stderr));
        } else {
            println!("Scriptlet executed successfully");
        }
        
        Ok(())
    }

    fn get_hash(&self) -> Result<String, String> {
        crate::crypto::calculate_sha256(&self.path)
            .map_err(|e| format!("Failed to calculate hash: {}", e))
    }
}

