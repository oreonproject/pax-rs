use super::*;
use std::fs::File;
use std::io::{Read, BufRead, BufReader};
use std::path::{Path, PathBuf};
use ar::Archive as ArArchive;
use tar::Archive as TarArchive;
use flate2::read::GzDecoder;

// Debian package adapter
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

        Ok(DebAdapter { path })
    }

    // Read control file from .deb package
    fn read_control_file(&self) -> Result<String, String> {
        let file = File::open(&self.path)
            .map_err(|e| format!("Failed to open deb file: {}", e))?;
        
        let mut archive = ArArchive::new(file);
        
        // .deb files contain: debian-binary, control.tar.gz, data.tar.gz
        while let Some(entry) = archive.next_entry() {
            let entry = entry.map_err(|e| format!("Failed to read ar entry: {}", e))?;
            let name = std::str::from_utf8(entry.header().identifier())
                .map_err(|e| format!("Invalid entry name: {}", e))?;

            if name.starts_with("control.tar") {
                // Extract control.tar.gz content
                let decoder = GzDecoder::new(entry);
                let mut tar = TarArchive::new(decoder);
                
                for entry in tar.entries()
                    .map_err(|e| format!("Failed to read tar entries: {}", e))? {
                    let mut entry = entry
                        .map_err(|e| format!("Failed to read tar entry: {}", e))?;
                    
                    let path = entry.path()
                        .map_err(|e| format!("Failed to get entry path: {}", e))?;
                    
                    if path.file_name() == Some(std::ffi::OsStr::new("control")) {
                        let mut contents = String::new();
                        entry.read_to_string(&mut contents)
                            .map_err(|e| format!("Failed to read control file: {}", e))?;
                        return Ok(contents);
                    }
                }
            }
        }

        Err("Control file not found in package".to_string())
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
        let file = File::open(&self.path)
            .map_err(|e| format!("Failed to open deb file: {}", e))?;
        
        let mut archive = ArArchive::new(file);
        let mut entries = Vec::new();
        
        // Find and extract data.tar.gz or data.tar.xz
        while let Some(entry) = archive.next_entry() {
            let entry = entry.map_err(|e| format!("Failed to read ar entry: {}", e))?;
            let name = std::str::from_utf8(entry.header().identifier())
                .map_err(|e| format!("Invalid entry name: {}", e))?;

            if name.starts_with("data.tar") {
                std::fs::create_dir_all(dest_dir)
                    .map_err(|e| format!("Failed to create dest dir: {}", e))?;

                // Handle different compression formats
                if name.ends_with(".gz") {
                    let decoder = GzDecoder::new(entry);
                    let mut tar = TarArchive::new(decoder);
                    tar.unpack(dest_dir)
                        .map_err(|e| format!("Failed to extract data: {}", e))?;
                } else {
                    // uncompressed or other format
                    let mut tar = TarArchive::new(entry);
                    tar.unpack(dest_dir)
                        .map_err(|e| format!("Failed to extract data: {}", e))?;
                }

                // List extracted files
                let file = File::open(&self.path)
                    .map_err(|e| format!("Failed to reopen file: {}", e))?;
                let mut archive = ArArchive::new(file);
                
                while let Some(entry) = archive.next_entry() {
                    let entry = entry.map_err(|e| format!("Failed to read ar entry: {}", e))?;
                    let name = std::str::from_utf8(entry.header().identifier())
                        .map_err(|e| format!("Invalid entry name: {}", e))?;

                    if name.starts_with("data.tar") {
                        let mut tar: TarArchive<Box<dyn Read>> = if name.ends_with(".gz") {
                            TarArchive::new(Box::new(GzDecoder::new(entry)))
                        } else {
                            TarArchive::new(Box::new(entry))
                        };

                        for entry in tar.entries()
                            .map_err(|e| format!("Failed to list entries: {}", e))? {
                            let entry = entry
                                .map_err(|e| format!("Failed to read entry: {}", e))?;
                            
                            let path = entry.path()
                                .map_err(|e| format!("Failed to get path: {}", e))?
                                .to_string_lossy()
                                .to_string();
                            
                            let file_type = if entry.header().entry_type().is_dir() {
                                FileType::Directory
                            } else if entry.header().entry_type().is_symlink() {
                                FileType::Symlink
                            } else {
                                FileType::Regular
                            };

                            entries.push(FileEntry { path, file_type });
                        }

                        break;
                    }
                }

                return Ok(entries);
            }
        }

        Err("data.tar not found in package".to_string())
    }

    fn get_dependencies(&self) -> Result<Vec<Dependency>, String> {
        Ok(self.extract_metadata()?.dependencies)
    }

    fn get_provides(&self) -> Result<Vec<Provides>, String> {
        Ok(self.extract_metadata()?.provides)
    }

    fn run_script(&self, stage: ScriptStage) -> Result<(), String> {
        // Debian maintainer scripts - also need careful handling
        // Scripts are in control.tar.gz: preinst, postinst, prerm, postrm
        
        let script_name = match stage {
            ScriptStage::PreInstall => "preinst",
            ScriptStage::PostInstall => "postinst",
            ScriptStage::PreRemove => "prerm",
            ScriptStage::PostRemove => "postrm",
        };

        // to do: Extract and safely execute maintainer scripts
        println!("Note: Debian {} script not executed", script_name);
        
        Ok(())
    }

    fn get_hash(&self) -> Result<String, String> {
        crate::crypto::calculate_sha256(&self.path)
            .map_err(|e| format!("Failed to calculate hash: {}", e))
    }
}

