use std::collections::HashMap;
use std::io::Write;
use std::fs;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

use crate::adapters::{detect_package_type, PackageType};
use crate::database::Database;
use crate::download::DownloadManager;
use crate::provides::ProvidesManager;
use crate::repository::{create_client_from_settings, PackageEntry};
use crate::resolver::{DependencyResolver, PackageInfo};
use crate::store::PackageStore;
use crate::symlinks::SymlinkManager;
use crate::verify::verify_package;
use crate::{Command, PostAction, StateBox};
use nix::unistd;
use settings::get_settings;

/// Package metadata for local packages
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalPackageMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub arch: Vec<String>,
    pub dependencies: Vec<String>,
    pub runtime_dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    pub install_script: Option<String>,
    pub uninstall_script: Option<String>,
    pub files: Vec<String>,
}

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install packages with dependency resolution",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    // Check for root privileges
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }

    let args = match args {
        None => {
            println!("Usage: pax install <package1> [package2] [...]");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    // load settings
    let settings = match get_settings() {
        Ok(s) => s,
        Err(_) => return PostAction::PullSources,
    };

    // Initialize components
    let db = match Database::open("/opt/pax/db/pax.db") {
        Ok(db) => db,
        Err(e) => {
            println!("Failed to open database: {}", e);
            return PostAction::Return;
        }
    };

    let store = match PackageStore::new() {
        Ok(s) => s,
        Err(e) => {
            println!("Failed to initialize package store: {}", e);
            return PostAction::Return;
        }
    };

    let downloader = match DownloadManager::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Failed to initialize download manager: {}", e);
            return PostAction::Return;
        }
    };

    let repo_client = match create_client_from_settings(&settings) {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to create repository client: {}", e);
            return PostAction::Return;
        }
    };

    let resolver = DependencyResolver::new(db.clone());
    let provides_mgr = ProvidesManager::new(db.clone());
    let symlink_mgr = SymlinkManager::new(db.clone(), "/opt/pax/links");

    println!("Resolving dependencies...");

    // Search for packages in repositories
    let mut packages_to_install = HashMap::new();
    
    for pkg_name in args {
        // Check if this is a local file path
        if std::path::Path::new(pkg_name).exists() {
            
            // Extract package metadata to get package info
            let metadata = match extract_package_metadata(pkg_name) {
                Ok(m) => m,
                Err(e) => {
                    println!("Failed to read package metadata: {}", e);
                    return PostAction::Return;
                }
            };
            
            // Create a PackageEntry from the metadata
            let hash = match calculate_file_hash(pkg_name) {
                Ok(h) => h,
                Err(e) => {
                    println!("Failed to calculate hash: {}", e);
                    return PostAction::Return;
                }
            };
            
            let canonical_path = match std::fs::canonicalize(pkg_name) {
                Ok(p) => p,
                Err(e) => {
                    println!("Failed to canonicalize path: {}", e);
                    return PostAction::Return;
                }
            };
            
            let pkg_entry = PackageEntry {
                name: metadata.name.clone(),
                version: metadata.version.clone(),
                description: metadata.description.clone(),
                hash,
                download_url: format!("file://{}", canonical_path.display()),
                signature_url: String::new(), // No signature for local files
                dependencies: metadata.dependencies.clone(),
                provides: metadata.provides.clone(),
                runtime_dependencies: metadata.runtime_dependencies.clone(),
                size: 0, // Will be calculated during extraction
            };
            
            packages_to_install.insert(metadata.name.clone(), ("local".to_string(), pkg_entry));
        } else {
            // Search in repositories
            match repo_client.search_package(pkg_name) {
                Ok(Some((source, pkg_entry))) => {
                    println!("Found {} (version {}) in {}", pkg_name, pkg_entry.version, source);
                    packages_to_install.insert(pkg_name.clone(), (source, pkg_entry));
                }
                Ok(None) => {
                    println!("Package not found: {}", pkg_name);
                    return PostAction::Return;
                }
                Err(e) => {
                    println!("Error searching for {}: {}", pkg_name, e);
                    return PostAction::Return;
                }
            }
        }
    }

    // Build available packages map for resolver
    let mut available = HashMap::new();
    for (pkg_name, (_, entry)) in &packages_to_install {
        available.insert(
            pkg_name.clone(),
            PackageInfo {
                version: entry.version.clone(),
                dependencies: entry.dependencies.iter().map(|d| {
                    crate::adapters::Dependency {
                        name: d.clone(),
                        version_constraint: None,
                        dep_type: crate::adapters::DependencyType::Runtime,
                    }
                }).collect(),
                provides: entry.provides.clone(),
            },
        );
    }

    // Resolve dependencies
    let package_names: Vec<String> = packages_to_install.keys().cloned().collect();
    let resolved = match resolver.resolve(&package_names, &available) {
        Ok(r) => r,
        Err(e) => {
            println!("Dependency resolution failed: {}", e);
            return PostAction::Return;
        }
    };

    println!("\nPackages to install:");
    for pkg in &resolved {
        println!("  - {} ({})", pkg.name, pkg.version.as_ref().unwrap_or(&"unknown".to_string()));
    }

    // Ask for confirmation
    print!("\nProceed with installation? [Y/n]: ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        println!("Failed to read input");
        return PostAction::Return;
    }
    
    if ["no", "n"].contains(&input.trim().to_lowercase().as_str()) {
        println!("Installation cancelled");
        return PostAction::Return;
    }

    // Install packages in order
    for pkg in resolved {
        if let Err(e) = install_package(
            &pkg.name,
            &packages_to_install,
            &repo_client,
            &downloader,
            &store,
            &db,
            &provides_mgr,
            &symlink_mgr,
        ) {
            println!("Failed to install {}: {}", pkg.name, e);
            println!("Installation aborted");
            return PostAction::Return;
        }
    }

    // Update library cache
    println!("\nUpdating system library cache...");
    if let Err(e) = symlink_mgr.update_library_cache() {
        eprintln!("Warning: Failed to update library cache: {}", e);
    }

    println!("\n\x1B[32mInstallation complete!\x1B[0m");
    PostAction::Return
}

// Install a single package
fn install_package(
    pkg_name: &str,
    packages: &HashMap<String, (String, PackageEntry)>,
    _repo_client: &crate::repository::RepositoryClient,
    downloader: &DownloadManager,
    store: &PackageStore,
    db: &Database,
    provides_mgr: &ProvidesManager,
    symlink_mgr: &SymlinkManager,
) -> Result<(), String> {
    // Skip if already installed
    if db.is_installed(pkg_name)
        .map_err(|e| format!("Database error: {}", e))? {
        println!("  {} already installed, skipping", pkg_name);
        return Ok(());
    }

    // Skip if satisfied by system
    if provides_mgr.is_satisfied(pkg_name)? {
        println!("  {} satisfied by system, skipping", pkg_name);
        return Ok(());
    }

    let (source, entry) = packages.get(pkg_name)
        .ok_or_else(|| format!("Package {} not found in list", pkg_name))?;

    println!("\nInstalling {}...", pkg_name);

    // Handle local vs remote packages
    let pkg_path = if source == "local" {
        // Local package - extract path from file:// URL
        let path = entry.download_url.strip_prefix("file://")
            .ok_or_else(|| "Invalid local package URL".to_string())?;
        println!("Using local package: {}", path);
        std::path::PathBuf::from(path)
    } else {
        // Download package from repository
        println!("Downloading {} from {}...", pkg_name, entry.download_url);
        let pkg_path = downloader.download_package(
            &entry.download_url,
            pkg_name,
            &entry.version,
        )?;

        // Download signature
        let sig_path = downloader.download_signature(
            &entry.signature_url,
            pkg_name,
            &entry.version,
        )?;

        // Verify package
        println!("Verifying package...");
        let verify_result = verify_package(&pkg_path, &sig_path, &entry.hash)?;
        
        if !verify_result.is_valid() {
            return Err(format!("Verification failed: {}", verify_result.error_message()));
        }
        
        pkg_path
    };

    // Extract to store
    println!("Extracting package...");
    let hash = entry.hash.clone();
    
    // Detect package type and extract
    let files = if let Some(pkg_type) = detect_package_type(&pkg_path) {
        match pkg_type {
            PackageType::Pax => {
                store.extract_pax_package(&pkg_path, &hash)?
            }
            PackageType::Rpm => {
                // for rpm/deb, we'd use the adapters to extract
                // simplified for now
                store.extract_pax_package(&pkg_path, &hash)?
            }
            PackageType::Deb => {
                store.extract_pax_package(&pkg_path, &hash)?
            }
        }
    } else {
        return Err("Unknown package type".to_string());
    };

    let size = store.get_package_size(&hash)?;

    // Add to database
    println!("Updating database...");
    let pkg_id = db.insert_package(
        pkg_name,
        &entry.version,
        &entry.description,
        source,
        &hash,
        size,
    ).map_err(|e| format!("Failed to insert package: {}", e))?;

    // Add file entries
    for file in &files {
        db.add_file(pkg_id, file, "regular")
            .map_err(|e| format!("Failed to add file: {}", e))?;
    }

    // Add dependencies
    for dep in &entry.dependencies {
        db.add_dependency(pkg_id, dep, None, "runtime")
            .map_err(|e| format!("Failed to add dependency: {}", e))?;
    }

    // Add provides
    for provide in &entry.provides {
        db.add_provides(pkg_id, provide, None, "virtual")
            .map_err(|e| format!("Failed to add provide: {}", e))?;
    }

    // Create symlinks
    println!("Creating symlinks...");
    symlink_mgr.create_symlinks(
        pkg_id,
        &hash,
        &store.get_package_path(&hash),
        &files,
    )?;

    println!("  {} installed successfully", pkg_name);

    Ok(())
}

/// Extract metadata from a local .pax package
fn extract_package_metadata(package_path: &str) -> Result<LocalPackageMetadata, String> {
    use tempfile::TempDir;
    use std::process::Command;
    
    let temp_dir = TempDir::new()
        .map_err(|e| format!("Failed to create temp directory: {}", e))?;
    
    let extract_dir = temp_dir.path().join("extract");
    fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Failed to create extract directory: {}", e))?;
    
    // Decompress and extract the package
    let zstd_output = Command::new("zstd")
        .arg("-dc")
        .arg(package_path)
        .output()
        .map_err(|e| format!("Failed to decompress package: {}", e))?;
    
    if !zstd_output.status.success() {
        return Err("Failed to decompress package".to_string());
    }
    
    let mut tar_process = Command::new("tar")
        .arg("-xf")
        .arg("-")
        .arg("-C")
        .arg(&extract_dir)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start tar process: {}", e))?;
    
    if let Some(stdin) = tar_process.stdin.take() {
        std::io::Write::write_all(&mut std::io::BufWriter::new(stdin), &zstd_output.stdout)
            .map_err(|e| format!("Failed to write to tar stdin: {}", e))?;
    }
    
    let tar_output = tar_process.wait_with_output()
        .map_err(|e| format!("Failed to wait for tar process: {}", e))?;
    
    if !tar_output.status.success() {
        return Err("Failed to extract package".to_string());
    }
    
    // Read metadata.yaml
    let metadata_path = extract_dir.join("metadata.yaml");
    if !metadata_path.exists() {
        return Err("metadata.yaml not found in package".to_string());
    }
    
    let metadata_content = fs::read_to_string(&metadata_path)
        .map_err(|e| format!("Failed to read metadata.yaml: {}", e))?;
    
    serde_yaml::from_str(&metadata_content)
        .map_err(|e| format!("Failed to parse metadata: {}", e))
}

/// Calculate SHA256 hash of a file
fn calculate_file_hash(file_path: &str) -> Result<String, String> {
    let mut file = fs::File::open(file_path)
        .map_err(|e| format!("Failed to open file: {}", e))?;
    
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("Failed to read file: {}", e))?;
    
    let hash = hasher.finalize();
    Ok(format!("{:x}", hash))
}
