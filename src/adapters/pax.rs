use super::*;
use serde::{Deserialize, Serialize};
use serde_yaml;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use zstd::stream::read::Decoder as ZstdDecoder;
use tar::Archive as TarArchive;

// Native .pax package format
// Structure: zstd compressed tarball with metadata.json at root
pub struct PaxAdapter {
    path: PathBuf,
    metadata: Option<PaxMetadata>,
}

// Simplified .paxmeta format - points to upstream sources
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PaxMetadata {
    // Required fields
    name: String,
    version: String,
    description: String,
    source: String,  // Direct URL to upstream tarball
    
    // Optional fields
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,  // SHA256 checksum (format: "sha256:...")
    #[serde(default = "default_arch")]
    arch: Vec<String>,  // Target architectures (defaults to ["x86_64", "aarch64"])
    #[serde(default)]
    dependencies: Vec<String>,  // Format: "name>=version" or "name"
    #[serde(default)]
    runtime_dependencies: Vec<String>,
    #[serde(default)]
    provides: Vec<String>,  // Simple string array
    #[serde(default)]
    conflicts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<String>,  // Build commands
    #[serde(skip_serializing_if = "Option::is_none")]
    install: Option<String>,  // Post-install script
    #[serde(skip_serializing_if = "Option::is_none")]
    uninstall: Option<String>,  // Post-uninstall script
}

// Default architecture list
fn default_arch() -> Vec<String> {
    vec!["x86_64".to_string(), "aarch64".to_string()]
}

impl PaxAdapter {
    // Create new PAX adapter from file
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        
        if !path.exists() {
            return Err(format!("File does not exist: {}", path.display()));
        }

        Ok(PaxAdapter {
            path,
            metadata: None,
        })
    }

    // Load metadata from package
    fn load_metadata(&mut self) -> Result<&PaxMetadata, String> {
        if self.metadata.is_some() {
            return Ok(self.metadata.as_ref().unwrap());
        }

        let file = File::open(&self.path)
            .map_err(|e| format!("Failed to open pax file: {}", e))?;
        
        let decoder = ZstdDecoder::new(file)
            .map_err(|e| format!("Failed to create decoder: {}", e))?;
        
        let mut archive = TarArchive::new(decoder);
        
        // Find and read .paxmeta
        for entry in archive.entries()
            .map_err(|e| format!("Failed to read archive entries: {}", e))? {
            let mut entry = entry
                .map_err(|e| format!("Failed to read entry: {}", e))?;
            
            let path = entry.path()
                .map_err(|e| format!("Failed to get entry path: {}", e))?;
            
            if path == Path::new(".paxmeta") {
                let mut contents = String::new();
                entry.read_to_string(&mut contents)
                    .map_err(|e| format!("Failed to read metadata: {}", e))?;
                
                let metadata: PaxMetadata = serde_yaml::from_str(&contents)
                    .map_err(|e| format!("Failed to parse metadata: {}", e))?;
                
                self.metadata = Some(metadata);
                return Ok(self.metadata.as_ref().unwrap());
            }
        }

        Err(".paxmeta not found in package".to_string())
    }

    // Convert string dependency to Dependency struct
    fn parse_dependency(dep_str: &str) -> Dependency {
        // Format: "name" or "name>=version" or "name<=version" etc
        if let Some(pos) = dep_str.find(">=") {
            Dependency {
                name: dep_str[..pos].trim().to_string(),
                version_constraint: Some(format!(">={}", dep_str[pos+2..].trim())),
                dep_type: DependencyType::Runtime,
            }
        } else if let Some(pos) = dep_str.find("<=") {
            Dependency {
                name: dep_str[..pos].trim().to_string(),
                version_constraint: Some(format!("<={}", dep_str[pos+2..].trim())),
                dep_type: DependencyType::Runtime,
            }
        } else if let Some(pos) = dep_str.find('=') {
            Dependency {
                name: dep_str[..pos].trim().to_string(),
                version_constraint: Some(format!("={}", dep_str[pos+1..].trim())),
                dep_type: DependencyType::Runtime,
            }
        } else if let Some(pos) = dep_str.find('>') {
            Dependency {
                name: dep_str[..pos].trim().to_string(),
                version_constraint: Some(format!(">{}", dep_str[pos+1..].trim())),
                dep_type: DependencyType::Runtime,
            }
        } else if let Some(pos) = dep_str.find('<') {
            Dependency {
                name: dep_str[..pos].trim().to_string(),
                version_constraint: Some(format!("<{}", dep_str[pos+1..].trim())),
                dep_type: DependencyType::Runtime,
            }
        } else {
            Dependency {
                name: dep_str.trim().to_string(),
                version_constraint: None,
                dep_type: DependencyType::Runtime,
            }
        }
    }
}

impl PackageAdapter for PaxAdapter {
    fn extract_metadata(&self) -> Result<PackageMetadata, String> {
        let mut adapter = PaxAdapter {
            path: self.path.clone(),
            metadata: self.metadata.clone(),
        };
        
        let meta = adapter.load_metadata()?;
        
        let dependencies: Vec<Dependency> = meta.dependencies
            .iter()
            .map(|d| Self::parse_dependency(d))
            .collect();
        
        let runtime_dependencies: Vec<Dependency> = meta.runtime_dependencies
            .iter()
            .map(|d| Self::parse_dependency(d))
            .collect();

        // Simplified provides - just strings, default to Virtual type
        let provides: Vec<Provides> = meta.provides
            .iter()
            .map(|name| {
                Provides {
                    name: name.clone(),
                    version: None,
                    provide_type: ProvideType::Virtual,
                }
            })
            .collect();

        Ok(PackageMetadata {
            name: meta.name.clone(),
            version: meta.version.clone(),
            description: meta.description.clone(),
            origin: "pax".to_string(),
            dependencies,
            runtime_dependencies,
            provides,
            conflicts: meta.conflicts.clone(),
        })
    }

    fn extract_files(&self, dest_dir: &Path) -> Result<Vec<FileEntry>, String> {
        let file = File::open(&self.path)
            .map_err(|e| format!("Failed to open pax file: {}", e))?;
        
        let decoder = ZstdDecoder::new(file)
            .map_err(|e| format!("Failed to create decoder: {}", e))?;
        
        let mut archive = TarArchive::new(decoder);
        let mut entries = Vec::new();
        
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| format!("Failed to create dest dir: {}", e))?;
        
        archive.unpack(dest_dir)
            .map_err(|e| format!("Failed to extract archive: {}", e))?;

        // Re-open to list files
        let file = File::open(&self.path)
            .map_err(|e| format!("Failed to reopen pax file: {}", e))?;
        
        let decoder = ZstdDecoder::new(file)
            .map_err(|e| format!("Failed to create decoder: {}", e))?;
        
        let mut archive = TarArchive::new(decoder);
        
        for entry in archive.entries()
            .map_err(|e| format!("Failed to read entries: {}", e))? {
            let entry = entry
                .map_err(|e| format!("Failed to read entry: {}", e))?;
            
            let path = entry.path()
                .map_err(|e| format!("Failed to get path: {}", e))?
                .to_string_lossy()
                .to_string();
            
            // skip metadata file from listing
            if path == ".paxmeta" {
                continue;
            }
            
            let file_type = if entry.header().entry_type().is_dir() {
                FileType::Directory
            } else if entry.header().entry_type().is_symlink() {
                FileType::Symlink
            } else {
                FileType::Regular
            };

            entries.push(FileEntry { path, file_type });
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
        // Native pax packages can have install/uninstall scripts
        // These would be defined in metadata and extracted to the package dir
        
        let mut adapter = PaxAdapter {
            path: self.path.clone(),
            metadata: self.metadata.clone(),
        };
        
        let meta = adapter.load_metadata()?;
        
        let script = match stage {
            ScriptStage::PreInstall => None,  // not supported in current format
            ScriptStage::PostInstall => meta.install.as_ref(),
            ScriptStage::PreRemove => None,   // not supported
            ScriptStage::PostRemove => meta.uninstall.as_ref(),
        };

        if let Some(script_content) = script {
            if !script_content.is_empty() {
                println!("Running {:?} script...", stage);
                
                // Execute script in a safe manner
                use std::process::Command;
                use std::io::Write;
                use tempfile::NamedTempFile;
                
                // Write script to temporary file
                let mut temp_file = NamedTempFile::new()
                    .map_err(|e| format!("Failed to create temp script file: {}", e))?;
                
                temp_file.write_all(script_content.as_bytes())
                    .map_err(|e| format!("Failed to write script: {}", e))?;
                
                let temp_path = temp_file.path();
                
                // Make script executable
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(temp_path)
                        .map_err(|e| format!("Failed to get script permissions: {}", e))?
                        .permissions();
                    perms.set_mode(0o700);
                    std::fs::set_permissions(temp_path, perms)
                        .map_err(|e| format!("Failed to set script permissions: {}", e))?;
                }
                
                // Execute script with sh
                let output = Command::new("sh")
                    .arg("-c")
                    .arg(script_content)
                    .env("PAX_PACKAGE", &meta.name)
                    .env("PAX_VERSION", &meta.version)
                    .output()
                    .map_err(|e| format!("Failed to execute script: {}", e))?;
                
                if !output.status.success() {
                    eprintln!("Warning: Script failed with exit code: {:?}", output.status.code());
                    eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
                    // Non-fatal - continue installation
                } else {
                    println!("Script executed successfully");
                    if !output.stdout.is_empty() {
                        println!("Output: {}", String::from_utf8_lossy(&output.stdout));
                    }
                }
            }
        }

        Ok(())
    }

    fn get_hash(&self) -> Result<String, String> {
        crate::crypto::calculate_sha256(&self.path)
            .map_err(|e| format!("Failed to calculate hash: {}", e))
    }
}

