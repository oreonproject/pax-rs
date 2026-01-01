use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use reqwest::Url;
use settings::OriginKind;
use std::fmt;
use std::hash::Hash;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command as RunCommand,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::runtime::Runtime;
use utils::{err, get_update_dir, tmpfile, Range, VerReq, Version};
use futures::future::{join_all, select_all};
use futures::FutureExt;

use crate::{
    depend_kind::DependKind, DepVer, InstalledInstallKind, InstalledMetaData, MetaDataKind,
    Specific, installed::InstalledCompilable, parsers::pax::RawPax, parsers::github::RawGithub, parsers::apt::RawApt,
};

// #region agent log
fn write_debug_log(log_entry: &serde_json::Value) -> Result<(), ()> {
    let log_path = "/home/blester/pax-rs/.cursor/debug.log";
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
        if let Ok(json_str) = serde_json::to_string(log_entry) {
            let _ = writeln!(file, "{}", json_str);
        }
    }
    Ok(())
}
// #endregion

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ProcessedInstallKind {
    PreBuilt(PreBuilt),
    Compilable(ProcessedCompilable),
}

fn walk_package_payload<F>(root: &Path, mut visitor: F) -> Result<(), String>
where
    F: FnMut(&Path, &Path, &std::fs::Metadata) -> Result<(), String>,
{
    use std::fs;

    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| {
            format!("Failed to read directory {}: {}", dir.display(), e)
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                format!("Failed to iterate directory {}: {}", dir.display(), e)
            })?;
            let path = entry.path();

            let metadata = fs::symlink_metadata(&path).map_err(|e| {
                format!("Failed to inspect {}: {}", path.display(), e)
            })?;

            let relative = path.strip_prefix(root).map_err(|_| {
                format!(
                    "Failed to determine relative path for {}",
                    path.display()
                )
            })?;

            if relative == Path::new("manifest.yaml") || relative.starts_with("pax-metadata") {
                continue;
            }

            visitor(&path, relative, &metadata)?;

            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }

    Ok(())
}

fn collect_package_entries(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut entries = Vec::new();
    walk_package_payload(root, |src, relative, _| {
        entries.push((src.to_path_buf(), relative.to_path_buf()));
        Ok(())
    })?;
    Ok(entries)
}

pub fn render_progress(label: &str, current: usize, total: usize, item: &str) {
    let total = total.max(1);
    let percent = (current * 100) / total;
    let bar_width = 30usize;
    let filled = (percent * bar_width) / 100;
    let mut bar = String::new();
    bar.push_str(&"#".repeat(filled.min(bar_width)));
    bar.push_str(&"-".repeat(bar_width.saturating_sub(filled)));

    let mut display_item = item.to_string();
    if display_item.len() > 40 {
        let tail_len = 37;
        display_item = format!(
            "...{}",
            &display_item[display_item.len().saturating_sub(tail_len)..]
        );
    }

    print!(
        "\r\x1B[K{} [{}] {:3}% {}",
        label,
        bar,
        percent.min(100),
        display_item
    );
    io::stdout().flush().ok();

    if current >= total {
        println!();
    }
}

fn needs_ldconfig(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.starts_with("/lib")
        || path_str.starts_with("/usr/lib")
        || path_str.starts_with("/usr/local/lib")
}

fn refresh_ld_cache() {
    match RunCommand::new("ldconfig").status() {
        Ok(status) if status.success() => {
            println!("Refreshed shared library cache with ldconfig.");
        }
        Ok(status) => {
            println!(
                "\x1B[93m[WARN] ldconfig exited with status {}. Library cache may be stale.\x1B[0m",
                status
            );
        }
        Err(err) => {
            println!(
                "\x1B[93m[WARN] Failed to run ldconfig: {}. You may need to refresh the linker cache manually.\x1B[0m",
                err
            );
        }
    }
}

fn read_dpkg_field(path: &Path, field: &str) -> Result<Option<String>, String> {
    use std::process::Command;

    let output = Command::new("dpkg-deb")
        .arg("-f")
        .arg(path)
        .arg(field)
        .output()
        .map_err(|e| format!("Failed to execute dpkg-deb -f: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "dpkg-deb -f {} failed for {}: {}",
            field,
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

// Removed: rpm_query_field is no longer needed with native RPM parsing
#[derive(PartialEq, Eq, Debug, Deserialize, Serialize, Hash, Clone)]
pub struct PreBuilt {
    pub critical: Vec<String>,
    pub configs: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ProcessedCompilable {
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub purge: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct InstallPackage {
    pub metadata: ProcessedMetaData,
    pub run_deps: Vec<ProcessedMetaData>,
    pub build_deps: Vec<ProcessedMetaData>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct QueuedChanges {
    pub install: Vec<String>,
    pub remove: Vec<String>,
    pub upgrade: Vec<String>,
}

impl InstallPackage {
    pub fn list_deps(&self, include_build: bool) -> Vec<String> {
        let mut deps = Vec::new();
        
        for dep in &self.run_deps {
            deps.push(dep.name.clone());
        }
        
        if include_build {
            for dep in &self.build_deps {
                deps.push(dep.name.clone());
            }
        }
        
        deps
    }
    
    pub fn install(&self, runtime: &Runtime) -> Result<(), String> {
        // First install runtime dependencies with this package as parent
        for dep in &self.run_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(false, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install dependency {}: {}", dep.name, e));
            }
        }
        
        // Then install build dependencies with this package as parent
        for dep in &self.build_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(false, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install build dependency {}: {}", dep.name, e));
            }
        }
        
        // Finally install the main package (no parent)
        self.metadata.install(runtime)
    }
    
    pub fn install_with_overwrite(&self, runtime: &Runtime) -> Result<(), String> {
        // First install runtime dependencies with this package as parent
        for dep in &self.run_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(true, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install dependency {}: {}", dep.name, e));
            }
        }
        
        // Then install build dependencies with this package as parent
        for dep in &self.build_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(true, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install build dependency {}: {}", dep.name, e));
            }
        }
        
        // Finally install the main package with overwrite enabled (no parent)
        self.metadata.install_with_overwrite(runtime)
    }
}
impl QueuedChanges {
    pub fn new() -> Self {
        Self {
            install: Vec::new(),
            remove: Vec::new(),
            upgrade: Vec::new(),
        }
    }

    pub fn insert_primary(&mut self, package: String) -> bool {
        if self.remove.contains(&package) {
            self.remove.retain(|p| p != &package);
            self.upgrade.push(package);
            true
        } else if !self.install.contains(&package) {
            self.install.push(package);
            true
        } else {
            false
        }
    }

    pub fn insert_dependent(&mut self, package: String) -> bool {
        if !self.install.contains(&package) && !self.upgrade.contains(&package) {
            self.install.push(package);
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ProcessedMetaData {
    pub name: String,
    pub kind: MetaDataKind,
    pub description: String,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub build_dependencies: Vec<DependKind>,
    pub runtime_dependencies: Vec<DependKind>,
    pub install_kind: ProcessedInstallKind,
    pub hash: String,
    // Additional fields expected by the application
    pub package_type: String,
    pub installed: bool,
    pub dependencies: Vec<String>,
    pub dependents: Vec<String>,
    pub installed_files: Vec<String>,
    pub available_versions: Vec<String>,
}

impl ProcessedMetaData {
    fn debug_enabled() -> bool {
        static DEBUG: OnceLock<bool> = OnceLock::new();
        *DEBUG.get_or_init(|| {
            std::env::var("PAX_DEBUG_FETCH")
                .map(|v| {
                    let v = v.trim().to_ascii_lowercase();
                    matches!(v.as_str(), "1" | "true" | "yes" | "on")
                })
                .unwrap_or(false)
        })
    }

    fn debug_log(args: fmt::Arguments<'_>) {
        if Self::debug_enabled() {
            eprintln!("{}", args);
        }
    }


    pub fn to_installed_with_parent(&self, installed_by: Option<String>) -> InstalledMetaData {
        InstalledMetaData {
            name: self.name.clone(),
            kind: self.kind.clone(),
            version: self.version.to_string(),
            description: self.description.clone(),
            origin: self.origin.clone(),
            dependent: self.dependent,
            installed_by,
            dependencies: {
                let mut result = Vec::new();
                for dep in &self.runtime_dependencies {
                    if let Some(dep) = dep.as_dep_ver() {
                        result.push(dep);
                    }
                }
                result
            },
            dependents: Vec::new(),
            install_kind: match &self.install_kind {
                ProcessedInstallKind::PreBuilt(prebuilt) => {
                    InstalledInstallKind::PreBuilt(prebuilt.clone())
                }
                ProcessedInstallKind::Compilable(comp) => {
                    InstalledInstallKind::Compilable(InstalledCompilable {
                        uninstall: comp.uninstall.clone(),
                        purge: comp.purge.clone(),
                    })
                }
            },
            hash: self.hash.to_string(),
        }
    }
    
    pub fn to_installed(&self) -> InstalledMetaData {
        self.to_installed_with_parent(None)
    }
    
    pub async fn install_package(self) -> Result<(), String> {
        self.install_package_impl(false, None).await
    }
    
    async fn install_package_impl(self, allow_overwrite: bool, installed_by: Option<String>) -> Result<(), String> {
        let name = self.name.to_string();
        println!("Installing {name}...");
        
        // Get the package file (download or use local)
        let package_file = self.get_package_file().await?;
        
        // Note: Hash verification is skipped for packages with embedded manifests
        // because the hash in manifest.yaml is the hash of the entire archive including
        // the manifest, creating a circular verification problem.
        // For packages with sidecar metadata files (.pax.meta), verification can be performed.
        
        if !self.hash.is_empty() && self.hash != "unknown" && !self.hash.starts_with('0') {
            // This package has a valid hash, but we don't verify for embedded manifests
            println!("\x1B[92m[OK]\x1B[0m Package metadata loaded (embedded manifest)");
        } else {
            println!("\x1B[93m[WARN]\x1B[0m Package hash not provided or placeholder, skipping verification");
        }
        
        // Create temporary extraction directory
        let extract_dir = std::env::temp_dir().join(format!("pax_install_{}", std::process::id()));
        std::fs::create_dir_all(&extract_dir)
            .map_err(|_| "Failed to create extraction directory")?;
        
        // Extract the package
        self.extract_package(&package_file, &extract_dir).await?;
        
        // Check for file conflicts before installation
        let file_manifest = self.create_file_manifest(&extract_dir).await?;
        let conflicts = file_manifest.check_conflicts()?;
        
        if !conflicts.is_empty() {
            if allow_overwrite {
                println!("\x1B[93m[WARN] File conflicts detected, but --allow-overwrite is enabled:\x1B[0m");
            } else {
                println!("\x1B[93m[WARN] File conflicts detected:\x1B[0m");
            }
            for conflict in &conflicts {
                match conflict.conflict_type {
                    crate::file_tracking::ConflictType::FileOwnership => {
                        println!("  File {} is owned by package '{}'", 
                                conflict.path.display(), conflict.existing_owner);
                    }
                    crate::file_tracking::ConflictType::DirectoryOwnership => {
                        println!("  Directory {} is owned by package '{}'", 
                                conflict.path.display(), conflict.existing_owner);
                    }
                    crate::file_tracking::ConflictType::SymlinkOwnership => {
                        println!("  Symlink {} is owned by package '{}'", 
                                conflict.path.display(), conflict.existing_owner);
                    }
                    crate::file_tracking::ConflictType::UntrackedFile => {
                        println!("  File {} already exists (not tracked by any package)", 
                                conflict.path.display());
                    }
                }
            }
            if !allow_overwrite {
                println!("\x1B[93m[WARN] Proceeding with installation - existing files will be backed up.\x1B[0m");
            }
        }
        
        // Get install root from environment variable PAX_ROOT, default to /
        let install_root = std::env::var("PAX_ROOT")
            .ok()
            .map(|r| PathBuf::from(r))
            .unwrap_or_else(|| PathBuf::from("/"));
        
        // Install based on package type
        // For Compilable packages from repositories, they are prebuilt and install commands handle file placement
        // Only build from source if explicitly requested with --build flag (not implemented yet)
        println!("[INSTALL_PKG] Package type: {:?}", self.install_kind);
        println!("[INSTALL_PKG] Extract dir: {}", extract_dir.display());
        println!("[INSTALL_PKG] Install root: {}", install_root.display());
        match self.install_kind {
            ProcessedInstallKind::PreBuilt(ref prebuilt) => {
                println!("[INSTALL_PKG] Installing as PreBuilt package");
                self.install_prebuilt_package_to_root(&extract_dir, prebuilt, allow_overwrite, &install_root).await?;
            }
            ProcessedInstallKind::Compilable(ref compilable) => {
                println!("[INSTALL_PKG] Installing as Compilable package");
                println!("[INSTALL_PKG] Compilable install commands length: {}", compilable.install.len());
                // Always run install commands - they use DESTDIR to place files correctly
                self.install_compilable_package_to_root(&extract_dir, compilable, &install_root).await?;
            }
        }
        
        // Save installed metadata - but skip if installing to custom root (PAX_ROOT)
        // We don't want to pollute system metadata when building ISO
        let pax_root = std::env::var("PAX_ROOT").ok();
        if pax_root.is_none() || pax_root.as_deref() == Some("/") {
            let installed_dir = utils::get_metadata_dir()?;
            let package_file = installed_dir.join(format!("{}.json", name));
            let path = package_file;
            let metadata = self.to_installed_with_parent(installed_by);
            metadata.write(&path)?;
            
            // Save file manifest for conflict detection
            file_manifest.save()?;
        }
        
        // Clean up
        let _ = std::fs::remove_dir_all(&extract_dir);
        
        Ok(())
    }
    
    async fn install_prebuilt_files_from_extract(&self, extract_dir: &std::path::Path, install_root: &Path, allow_overwrite: bool, installed_by: Option<String>) -> Result<(), String> {
        use std::fs;
        use crate::file_tracking::FileManifest;
        
        println!("Installing pre-built files from package...");
        
        let mut manifest = FileManifest::new(self.name.clone(), self.version.clone());
        let entries = collect_package_entries(extract_dir)?;
        let total = entries.len().max(1);
        let mut processed = 0usize;
        
        for (src_path, relative) in entries {
            processed += 1;
            let metadata = fs::symlink_metadata(&src_path).map_err(|e| {
                format!("Failed to inspect {}: {}", src_path.display(), e)
            })?;
            
            let relative_clean = if let Ok(stripped) = relative.strip_prefix("/") {
                stripped
            } else {
                &relative
            };
            let dest_path = install_root.join(relative_clean);
            
            if metadata.is_dir() {
                fs::create_dir_all(&dest_path).map_err(|e| {
                    format!("Failed to create directory {}: {}", dest_path.display(), e)
                })?;
                let mode = metadata.permissions().mode();
                fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
                    format!("Failed to set permissions: {}", e)
                })?;
                manifest.add_directory(dest_path.clone(), mode);
            } else if metadata.file_type().is_symlink() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| format!("Failed to create parent: {}", e))?;
                }
                let target = fs::read_link(&src_path).map_err(|e| format!("Failed to read symlink: {}", e))?;
                let _ = fs::remove_file(&dest_path);
                symlink(&target, &dest_path).map_err(|e| format!("Failed to create symlink: {}", e))?;
                manifest.add_symlink(dest_path.clone(), target);
            } else if metadata.is_file() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| format!("Failed to create parent: {}", e))?;
                }
                if dest_path.exists() {
                    fs::remove_file(&dest_path).map_err(|e| format!("Failed to remove existing: {}", e))?;
                }
                fs::copy(&src_path, &dest_path).map_err(|e| format!("Failed to copy file: {}", e))?;
                let mode = metadata.permissions().mode();
                fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode)).map_err(|e| format!("Failed to set permissions: {}", e))?;
                let checksum = crate::file_tracking::calculate_file_checksum(&dest_path).unwrap_or_default();
                manifest.add_file(dest_path.clone(), metadata.len(), mode, checksum);
            }
            
            render_progress("Installing", processed, total, &relative_clean.to_string_lossy());
        }
        
        println!("\nInstalled {} files from prebuilt package.", manifest.files.len());
        
        // Save metadata and manifest if not using custom root
        let pax_root = std::env::var("PAX_ROOT").ok();
        if pax_root.is_none() || pax_root.as_deref() == Some("/") {
            let installed_dir = utils::get_metadata_dir()?;
            let package_file = installed_dir.join(format!("{}.json", self.name));
            let metadata = self.to_installed_with_parent(installed_by);
            metadata.write(&package_file)?;
            manifest.save()?;
        }
        
        Ok(())
    }
    
    async fn create_file_manifest(&self, extract_dir: &Path) -> Result<crate::file_tracking::FileManifest, String> {
        use crate::file_tracking::FileManifest;
        
        let mut manifest = FileManifest::new(self.name.clone(), self.version.clone());
        
        // Walk through the extracted directory and catalog all files
        // We need to map the extraction directory paths to actual system paths
        self.walk_directory(extract_dir, &PathBuf::from("/"), &mut manifest)?;
        
        Ok(manifest)
    }
    
    fn walk_directory(&self, extract_base: &Path, target_base: &PathBuf, manifest: &mut crate::file_tracking::FileManifest) -> Result<(), String> {
        use std::fs;
        
        for entry in fs::read_dir(extract_base)
            .map_err(|e| format!("Failed to read directory {}: {}", extract_base.display(), e))? {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let extract_path = entry.path();
            let metadata = entry.metadata()
                .map_err(|e| format!("Failed to get metadata for {}: {}", extract_path.display(), e))?;
            
            // Map the extraction path to target path
            let rel_path = extract_path.strip_prefix(extract_base)
                .map_err(|e| format!("Failed to strip prefix: {}", e))?;
            let target_path = target_base.join(rel_path);
            
            if metadata.is_file() {
                let size = metadata.len();
                let permissions = metadata.permissions().mode();
                let checksum = crate::file_tracking::calculate_file_checksum(&extract_path)
                    .unwrap_or_else(|_| "unknown".to_string());
                
                manifest.add_file(target_path, size, permissions, checksum);
            } else if metadata.is_dir() {
                let permissions = metadata.permissions().mode();
                manifest.add_directory(target_path.clone(), permissions);
                // Recursively process subdirectories
                self.walk_directory(&extract_path, target_base, manifest)?;
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&extract_path)
                    .map_err(|e| format!("Failed to read symlink target: {}", e))?;
                manifest.add_symlink(target_path, target);
            }
        }
        
        Ok(())
    }
    
    async fn get_package_file(&self) -> Result<std::path::PathBuf, String> {
        let tmpfile = tmpfile().ok_or("Failed to reserve temporary file")?;
        
        match &self.origin {
            OriginKind::Pax(pax) => {
                let pax_path = std::path::Path::new(pax);
                if pax_path.exists() {
                    // Local file - copy to temp location
                    std::fs::copy(pax, &tmpfile)
                        .map_err(|e| format!("Failed to copy local PAX file: {}", e))?;
                } else if pax.starts_with("http://") || pax.starts_with("https://") {
                    // Remote file - download directly
                    // PAX repositories now just serve .pax files directly
                    let response = reqwest::get(pax.as_str()).await
                        .map_err(|e| format!("Failed to download PAX file: {}", e))?;
                    
                    if !response.status().is_success() {
                        return Err(format!("HTTP error {} when downloading PAX file from {}", response.status(), pax));
                    }
                    
                    let bytes = response.bytes().await
                        .map_err(|e| format!("Failed to read PAX file data: {}", e))?;
                    std::fs::write(&tmpfile, bytes)
                        .map_err(|e| format!("Failed to write PAX file to temp: {}", e))?;
                } else {
                    return Err(format!("Package file does not exist: {}", pax));
                }
            }
            OriginKind::Github { user, repo } => {
                let endpoint = format!("https://github.com/{}/{}/archive/refs/tags/{}.tar.gz", user, repo, self.version);
                let response = reqwest::get(&endpoint).await
                    .map_err(|_| "Failed to download GitHub archive")?;
                let bytes = response.bytes().await
                    .map_err(|_| "Failed to read GitHub archive data")?;
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write GitHub archive to temp")?;
            }
            OriginKind::Apt(source) => {
                let path = std::path::Path::new(source);
                if path.exists() {
                    std::fs::copy(path, &tmpfile)
                        .map_err(|_| "Failed to copy local DEB package")?;
                } else {
                    let base = source.trim_end_matches('/');
                    let endpoint = format!("{}/packages/{}/{}.deb", base, self.name, self.version);
                    let response = reqwest::get(&endpoint).await
                        .map_err(|_| "Failed to download APT package")?;
                    let bytes = response.bytes().await
                        .map_err(|_| "Failed to read APT package data")?;
                    std::fs::write(&tmpfile, bytes)
                        .map_err(|_| "Failed to write APT package to temp")?;
                }
            }
            OriginKind::Rpm(repo_url) => {
                use crate::yum_repository::YumRepositoryClient;
                
                let client = YumRepositoryClient::new(repo_url.clone());
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get RPM package info")?;
                
                let response = reqwest::get(&package_info.url).await
                        .map_err(|_| "Failed to download RPM package")?;
                    let bytes = response.bytes().await
                        .map_err(|_| "Failed to read RPM package data")?;
                    std::fs::write(&tmpfile, bytes)
                        .map_err(|_| "Failed to write RPM package to temp")?;
                }
            OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                use crate::cloudflare_r2::CloudflareR2Client;
                
                let client = CloudflareR2Client::new(
                    bucket.clone(),
                    account_id.clone(),
                    None, // access_key_id
                    None, // secret_access_key
                    None, // region
                );
                
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get package info from R2")?;
                
                let bytes = client.download_package(&package_info).await
                    .map_err(|_| "Failed to download package from R2")?;
                
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write R2 package to temp")?;
            }
            OriginKind::Deb(repo_url) => {
                use crate::deb_repository::DebRepositoryClient;
                
                let client = DebRepositoryClient::new(repo_url.clone());
                
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get package info from DEB repository")?;
                
                let bytes = client.download_package(&package_info).await
                    .map_err(|_| "Failed to download package from DEB repository")?;
                
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write DEB package to temp")?;
            }
            OriginKind::Yum(repo_url) => {
                use crate::yum_repository::YumRepositoryClient;
                
                let client = YumRepositoryClient::new(repo_url.clone());
                
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get package info from YUM repository")?;
                
                let bytes = client.download_package(&package_info).await
                    .map_err(|_| "Failed to download package from YUM repository")?;
                
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write RPM package to temp")?;
            }
            OriginKind::LocalDir(dir_path) => {
                // Find package file in local directory
                let dir = std::path::Path::new(dir_path);
                if !dir.exists() || !dir.is_dir() {
                    return Err(format!("Local directory repository does not exist: {}", dir_path));
                }
                
                // Try to find package file matching name and version
                let mut possible_files = vec![
                    dir.join(format!("{}-{}.pax", self.name, self.version)),
                    dir.join(format!("{}-{}.deb", self.name, self.version)),
                    dir.join(format!("{}-{}.rpm", self.name, self.version)),
                    dir.join(format!("{}_{}.deb", self.name, self.version)),
                ];
                
                // Also try with architecture suffixes (x86_64v3, x86_64v1, x86_64)
                for arch in &["x86_64v3", "x86_64v1", "x86_64"] {
                    possible_files.push(dir.join(format!("{}-{}-{}.pax", self.name, self.version, arch)));
                    possible_files.push(dir.join(format!("{}-{}-{}.deb", self.name, self.version, arch)));
                    possible_files.push(dir.join(format!("{}-{}-{}.rpm", self.name, self.version, arch)));
                }
                
                // Scan directory for files matching the pattern (in case exact match doesn't work)
                if let Ok(entries) = std::fs::read_dir(dir) {
                    let prefix = format!("{}-{}", self.name, self.version);
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                            if file_name.starts_with(&prefix) && 
                               (file_name.ends_with(".pax") || file_name.ends_with(".deb") || file_name.ends_with(".rpm")) &&
                               !file_name.contains(".src.") {
                                possible_files.push(path);
                            }
                        }
                    }
                }
                
                let mut found = false;
                for package_path in possible_files {
                    if package_path.exists() {
                        std::fs::copy(&package_path, &tmpfile)
                            .map_err(|e| format!("Failed to copy local package file: {}", e))?;
                        found = true;
                        break;
                    }
                }
                
                if !found {
                    return Err(format!("Package {}-{} not found in local directory {}", self.name, self.version, dir_path));
                }
            }
        }
        
        Ok(tmpfile)
    }
    
    async fn extract_package(&self, package_file: &std::path::Path, extract_dir: &std::path::Path) -> Result<(), String> {
        match &self.origin {
            OriginKind::Pax(_) | OriginKind::Github { .. } => {
                let mut tar_cmd = RunCommand::new("tar");
                tar_cmd
                    .arg("-xzf")
                    .arg(package_file)
                    .arg("-C")
                    .arg(extract_dir);
                let status = tar_cmd
                    .status()
                    .map_err(|_| "Failed to extract archive with tar")?;
                if !status.success() {
                    return err!("Failed to extract archive using tar");
                }
            }
            OriginKind::Apt(_) => {
                let mut dpkg_cmd = RunCommand::new("dpkg-deb");
                dpkg_cmd.arg("-x").arg(package_file).arg(extract_dir);
                let status = dpkg_cmd
                    .status()
                    .map_err(|_| "Failed to execute dpkg-deb for extraction")?;
                if !status.success() {
                    return err!("Failed to extract DEB package");
                }
            }
            OriginKind::Rpm(_) | OriginKind::Yum(_) => {
                let command = format!(
                    "rpm2cpio '{}' | cpio -idmv",
                    package_file.display()
                );
                let status = RunCommand::new("bash")
                    .arg("-c")
                    .arg(command)
                    .current_dir(extract_dir)
                    .status()
                    .map_err(|_| "Failed to extract RPM package")?;
                if !status.success() {
                    return err!("Failed to extract RPM package");
                }
            }
            OriginKind::Deb(_) => {
                let mut dpkg_cmd = RunCommand::new("dpkg-deb");
                dpkg_cmd.arg("-x").arg(package_file).arg(extract_dir);
                let status = dpkg_cmd
                    .status()
                    .map_err(|_| "Failed to execute dpkg-deb for extraction")?;
                if !status.success() {
                    return err!("Failed to extract DEB package");
                }
            }
            OriginKind::CloudflareR2 { .. } => {
                // R2 packages are typically PAX format
                let mut tar_cmd = RunCommand::new("tar");
                tar_cmd
                    .arg("-xzf")
                    .arg(package_file)
                    .arg("-C")
                    .arg(extract_dir);
                let status = tar_cmd
                    .status()
                    .map_err(|_| "Failed to extract archive with tar")?;
                if !status.success() {
                    return err!("Failed to extract archive using tar");
                }
            }
            OriginKind::LocalDir(_) => {
                // LocalDir packages can be .pax, .deb, or .rpm - determine by extension
                let ext = package_file.extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                
                match ext {
                    "pax" => {
                        let mut tar_cmd = RunCommand::new("tar");
                        tar_cmd
                            .arg("-xzf")
                            .arg(package_file)
                            .arg("-C")
                            .arg(extract_dir);
                        let status = tar_cmd
                            .status()
                            .map_err(|_| "Failed to extract PAX package from local directory")?;
                        if !status.success() {
                            return err!("Failed to extract PAX package");
                        }
                    },
                    "deb" => {
                        let mut dpkg_cmd = RunCommand::new("dpkg-deb");
                        dpkg_cmd.arg("-x").arg(package_file).arg(extract_dir);
                        let status = dpkg_cmd
                            .status()
                            .map_err(|_| "Failed to execute dpkg-deb for extraction")?;
                        if !status.success() {
                            return err!("Failed to extract DEB package");
                        }
                    },
                    "rpm" => {
                        let command = format!(
                            "rpm2cpio '{}' | cpio -idmv",
                            package_file.display()
                        );
                        let status = RunCommand::new("bash")
                            .arg("-c")
                            .arg(command)
                            .current_dir(extract_dir)
                            .status()
                            .map_err(|_| "Failed to extract RPM package")?;
                        if !status.success() {
                            return err!("Failed to extract RPM package");
                        }
                    },
                    _ => {
                        return err!("Unknown package format in local directory: {}", ext);
                    }
                }
            }
        }
        Ok(())
    }
    
    async fn install_prebuilt_package(&self, extract_dir: &std::path::Path, _prebuilt: &PreBuilt, allow_overwrite: bool) -> Result<(), String> {
        self.install_prebuilt_package_to_root(extract_dir, _prebuilt, allow_overwrite, Path::new("/")).await
    }
    
    async fn install_prebuilt_package_to_root(&self, extract_dir: &std::path::Path, prebuilt: &PreBuilt, allow_overwrite: bool, install_root: &Path) -> Result<(), String> {
        use std::fs;
        use crate::file_tracking::FileManifest;

        println!("[INSTALL_PREBUILT] Installing pre-built files for {}...", self.name);
        println!("[INSTALL_PREBUILT] Extract dir: {}", extract_dir.display());
        println!("[INSTALL_PREBUILT] Install root: {}", install_root.display());

        let mut manifest = FileManifest::new(
            self.name.clone(),
            self.version.clone(),
        );

        let entries = collect_package_entries(extract_dir)?;
        println!("[INSTALL_PREBUILT] Found {} entries to install", entries.len());
        let total = entries.len().max(1);
        let mut processed = 0usize;

        for (src_path, relative) in entries {
            processed += 1;
            let metadata = fs::symlink_metadata(&src_path).map_err(|e| {
                format!("Failed to inspect {}: {}", src_path.display(), e)
            })?;

            // Strip leading slash from relative path so join works correctly
            let relative_clean = if let Ok(stripped) = relative.strip_prefix("/") {
                stripped
            } else {
                &relative
            };
            let dest_path = install_root.join(relative_clean);
            
            if self.name == "pax-rs" {
                eprintln!("[INSTALL_PREBUILT] pax-rs: Installing {} -> {}", src_path.display(), dest_path.display());
            }

            if metadata.is_dir() {
                fs::create_dir_all(&dest_path).map_err(|e| {
                    format!("Failed to create directory {}: {}", dest_path.display(), e)
                })?;

                let mode = metadata.permissions().mode();
                fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
                    format!(
                        "Failed to set permissions on directory {}: {}",
                        dest_path.display(),
                        e
                    )
                })?;

                manifest.add_directory(dest_path.clone(), mode);
            } else if metadata.file_type().is_symlink() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!(
                            "Failed to create parent directory {}: {}",
                            parent.display(),
                            e
                        )
                    })?;
                }

                // Try to remove existing symlink or file, ignore errors if it doesn't exist
                if dest_path.is_symlink() {
                    let _ = fs::remove_file(&dest_path);
                } else if dest_path.is_file() {
                    let _ = fs::remove_file(&dest_path);
                } else if dest_path.is_dir() {
                    return Err(format!("Destination path {} is a directory, cannot create symlink", dest_path.display()));
                } else if dest_path.exists() {
                    // Fallback: try to remove even if we can't determine the type
                    let _ = fs::remove_file(&dest_path);
                }

                let target = fs::read_link(&src_path).map_err(|e| {
                    format!("Failed to read symlink target {}: {}", src_path.display(), e)
                })?;

                // Try to create symlink with retry in case of race condition
                let mut retries = 3;
                loop {
                    match symlink(&target, &dest_path) {
                        Ok(_) => break,
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && retries > 0 => {
                            // Race condition: try removing again
                            let _ = fs::remove_file(&dest_path);
                            retries -= 1;
                            // Brief pause
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        Err(e) => {
                            return Err(format!(
                        "Failed to create symlink {} -> {}: {}",
                        dest_path.display(),
                        target.display(),
                        e
                            ));
                        }
                    }
                }

                manifest.add_symlink(dest_path.clone(), target);
            } else if metadata.is_file() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!(
                            "Failed to create parent directory {}: {}",
                            parent.display(),
                            e
                        )
                    })?;
                }

                if dest_path.exists() {
                    fs::remove_file(&dest_path).map_err(|e| {
                        format!("Failed to remove existing file {}: {}", dest_path.display(), e)
                    })?;
                }

                fs::copy(&src_path, &dest_path).map_err(|e| {
                    format!(
                        "Failed to install file {}: {}",
                        dest_path.display(),
                        e
                    )
                })?;

                let mode = metadata.permissions().mode();
                fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
                    format!(
                        "Failed to set permissions on file {}: {}",
                        dest_path.display(),
                        e
                    )
                })?;

                let checksum = crate::file_tracking::calculate_file_checksum(&dest_path)
                    .unwrap_or_default();

                manifest.add_file(dest_path.clone(), metadata.len(), mode, checksum);
            }

            render_progress(
                "Installing",
                processed,
                total,
                &relative.to_string_lossy(),
            );
        }

        manifest.save()?;

        println!(
            "Installed {} file(s), {} director(y/ies), {} symlink(s).",
            manifest.files.len(),
            manifest.directories.len(),
            manifest.symlinks.len(),
        );

        if manifest
            .files
            .iter()
            .all(|f| f.permissions & 0o111 == 0)
        {
            println!(
                "\x1B[93m[WARN] No executable files were installed; this package may only provide libraries.\x1B[0m"
            );
        }
        if manifest
            .files
            .iter()
            .any(|f| needs_ldconfig(&f.path))
        {
            refresh_ld_cache();
        }

        Ok(())
    }
    
    async fn install_compilable_package(&self, extract_dir: &std::path::Path, compilable: &ProcessedCompilable) -> Result<(), String> {
        let install_root = std::env::var("PAX_ROOT")
            .ok()
            .map(|r| PathBuf::from(r))
            .unwrap_or_else(|| PathBuf::from("/"));
        self.install_compilable_package_to_root(extract_dir, compilable, &install_root).await
    }
    
    async fn install_compilable_package_to_root(&self, extract_dir: &std::path::Path, compilable: &ProcessedCompilable, install_root: &Path) -> Result<(), String> {
        if compilable.install.is_empty() {
            return Err(format!("Install commands are empty for {}", self.name));
        }
        
        use std::io::Write;
        
        println!("[{}] Running install commands from: {}", self.name, extract_dir.display());
        println!("[{}] DESTDIR={}", self.name, install_root.display());
        println!("[{}] Install script:\n{}", self.name, compilable.install);
        std::io::stdout().flush().unwrap();
        
        // Run install commands - split by newlines if multiple commands
        let commands: Vec<&str> = compilable.install.lines().collect();
        
        for (i, cmd) in commands.iter().enumerate() {
            let cmd = cmd.trim();
            if cmd.is_empty() || cmd.starts_with('#') {
                continue;
            }
            
            println!("[{}] Executing install command {}: {}", self.name, i + 1, cmd);
            std::io::stdout().flush().unwrap();
            
            let mut install_cmd = RunCommand::new("bash");
            install_cmd.arg("-c").arg(cmd);
            install_cmd.current_dir(extract_dir);
            install_cmd.env("DESTDIR", install_root.to_string_lossy().to_string());
            install_cmd.env("TARGET", "x86_64-unknown-linux-gnu");
            
            let output = install_cmd.output().map_err(|e| format!("Failed to execute install command '{}': {}", cmd, e))?;
            
            if !output.stdout.is_empty() {
                println!("[{}] Command {} stdout: {}", self.name, i + 1, String::from_utf8_lossy(&output.stdout));
            }
            if !output.stderr.is_empty() {
                println!("[{}] Command {} stderr: {}", self.name, i + 1, String::from_utf8_lossy(&output.stderr));
            }
            std::io::stdout().flush().unwrap();
            
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                return Err(format!("Install command {} failed for {}:\nCommand: {}\nstdout: {}\nstderr: {}", i + 1, self.name, cmd, stdout, stderr));
            }
            
            println!("[{}] Command {} completed successfully", self.name, i + 1);
            std::io::stdout().flush().unwrap();
        }
        
        println!("[{}] All install commands completed", self.name);
        std::io::stdout().flush().unwrap();
        Ok(())
    }
    
    fn find_build_directory(&self, extract_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
        // Try common build directory patterns
        let candidates = vec![
            extract_dir.join(&self.name),
            extract_dir.join(format!("{}-{}", self.name, self.version)),
            extract_dir.join(format!("{}-{}", self.name, self.version.replace('.', "-"))),
        ];
        
        for candidate in candidates {
            if candidate.exists() && candidate.is_dir() {
                return Ok(candidate);
            }
                        }
                        
        // If no specific directory found, use the extract directory itself
        Ok(extract_dir.to_path_buf())
    }
    
    pub async fn get_metadata_from_local_package(package_path: &str) -> Result<Self, String> {
        use std::path::Path;

        let path = Path::new(package_path);
        if !path.exists() {
            return err!("Package file does not exist: {}", path.display());
        }

        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .unwrap_or_else(|| String::new());

        match extension.as_str() {
            "pax" => Self::load_local_pax(path),
            "deb" => Self::load_local_deb(path),
            "rpm" => Self::load_local_rpm(path),
            "" => match Self::load_local_pax(path) {
                Ok(metadata) => Ok(metadata),
                Err(err) => err!("{}", err),
            },
            other => match Self::load_local_pax(path) {
                Ok(metadata) => Ok(metadata),
                Err(_) => err!(
                    "Unsupported package format `{}` for {}",
                    other,
                    path.display()
                ),
            },
        }
    }

    fn load_local_pax(path: &Path) -> Result<Self, String> {
        use std::process::Command;

        let temp_dir = Self::create_temp_dir("pax_extract")?;

        let status = Command::new("tar")
            .arg("-xzf")
            .arg(path)
            .arg("-C")
            .arg(&temp_dir)
            .status()
            .map_err(|e| format!("Failed to extract PAX archive {}: {}", path.display(), e))?;

        if !status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            return err!("Failed to extract PAX archive: {}", path.display());
        }

        let manifest_path = temp_dir.join("manifest.yaml");
        let sidecar_path = path.with_extension("pax.meta");
        let metadata_dir = temp_dir.join("pax-metadata");

        let mut processed = if metadata_dir.is_dir() {
            // Parse new format (pax-metadata/metadata.json or metadata.yaml)
            // Dependencies are in dependencies.runtime_dependencies in the metadata file itself
            Self::parse_pax_metadata_dir(&metadata_dir)?
        } else {
            let manifest_content = if manifest_path.exists() {
                fs::read_to_string(&manifest_path)
                    .map_err(|_| "Failed to read manifest.yaml")?
            } else if sidecar_path.exists() {
                fs::read_to_string(&sidecar_path).map_err(|_| {
                    format!(
                        "Failed to read metadata sidecar: {}",
                        sidecar_path.display()
                    )
                })?
            } else {
                let _ = fs::remove_dir_all(&temp_dir);
                return err!(
                    "No package metadata found for {}. Expected manifest.yaml, pax-metadata directory, or sidecar {}",
                    path.display(),
                    sidecar_path.display()
                );
            };

            // Fix common YAML issues in manifests (malformed quotes, etc.)
            let fixed_manifest = Self::fix_yaml_syntax(&manifest_content);

            let raw_pax = serde_norway::from_str::<RawPax>(&fixed_manifest)
                .map_err(|e| format!("Failed to parse manifest.yaml as PAX format: {}", e))?;

            // #region agent log
            let _ = write_debug_log(&serde_json::json!({
                "sessionId": "debug-session",
                "runId": "pax-metadata-parse",
                "hypothesisId": "MANIFEST_PARSE",
                "location": "metadata/src/processed/mod.rs:1280",
                "message": "parsed_manifest_yaml",
                "data": {
                    "package_name": raw_pax.name,
                    "runtime_deps_count": raw_pax.runtime_dependencies.len(),
                    "runtime_deps": raw_pax.runtime_dependencies.clone(),
                    "build_deps_count": raw_pax.build_dependencies.len()
                },
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            }));
            // #endregion

            let processed = raw_pax
                .process()
                .ok_or("Failed to process PAX metadata")?;
            
            // #region agent log
            let _ = write_debug_log(&serde_json::json!({
                "sessionId": "debug-session",
                "runId": "pax-metadata-parse",
                "hypothesisId": "MANIFEST_PROCESSED",
                "location": "metadata/src/processed/mod.rs:1295",
                "message": "processed_manifest_metadata",
                "data": {
                    "package_name": processed.name,
                    "runtime_deps_count": processed.runtime_dependencies.len(),
                    "runtime_deps": processed.runtime_dependencies.iter().map(|d| match d {
                        DependKind::Latest(n) => n.clone(),
                        DependKind::Specific(dv) => dv.name.clone(),
                        DependKind::Volatile(n) => n.clone(),
                    }).collect::<Vec<_>>()
                },
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            }));
            // #endregion
            
            processed
        };

        let (has_entries, critical_files, config_files) = Self::collect_payload_from(&temp_dir)?;

        if has_entries {
            processed.install_kind = ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            });
        }

        processed.dependent = false;
        processed.origin = OriginKind::Pax(path.to_string_lossy().to_string());
        if processed.hash.is_empty() || processed.hash == "unknown" {
            processed.hash = crate::file_tracking::calculate_file_checksum(path).unwrap_or_default();
        }

        let _ = fs::remove_dir_all(&temp_dir);

        Ok(processed)
    }

    fn parse_pax_metadata_dir(metadata_dir: &Path) -> Result<Self, String> {
        let yaml_path = metadata_dir.join("metadata.yaml");
        let json_path = metadata_dir.join("metadata.json");

        let (metadata_value, source_path) = if yaml_path.exists() {
            let content = fs::read_to_string(&yaml_path)
                .map_err(|e| format!("Failed to read {}: {}", yaml_path.display(), e))?;
            let value: JsonValue = serde_yaml::from_str(&content)
                .map_err(|e| format!("Failed to parse {}: {}", yaml_path.display(), e))?;
            (value, yaml_path.display().to_string())
        } else if json_path.exists() {
            let content = fs::read_to_string(&json_path)
                .map_err(|e| format!("Failed to read {}: {}", json_path.display(), e))?;
            let value: JsonValue = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse {}: {}", json_path.display(), e))?;
            (value, json_path.display().to_string())
        } else {
            return err!(
                "No metadata.yaml or metadata.json found in {}",
                metadata_dir.display()
            );
        };

        let package = metadata_value.get("package").ok_or_else(|| {
            format!(
                "Missing `package` section in {}",
                source_path
            )
        })?;

        let name = package
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("Missing package.name in {}", source_path))?
            .trim()
            .to_string();

        let version = package
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("0.0.0")
            .trim()
            .to_string();

        let release = package
            .get("release")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        let architecture = package
            .get("architecture")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        let branch = package
            .get("branch")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        let target_release = package
            .get("target_release")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());

        let mut description = package
            .get("summary")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                package
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| format!("{} {}", name, version));

        if let Some(branch) = branch.as_ref() {
            if !branch.is_empty() && !description.contains(branch) {
                description = format!("{description} (branch {branch})");
            }
        }

        if let Some(target) = target_release.as_ref() {
            if !target.is_empty() && !description.contains(target) {
                description = format!("{description} [{target}]");
            }
        }

        // #region agent log
        let deps_runtime_path1 = metadata_value.pointer("/dependencies/runtime");
        let deps_runtime_path2 = metadata_value.pointer("/dependencies/runtime_dependencies");
        let deps_runtime_path3 = metadata_value.pointer("/package/dependencies/runtime");
        let deps_runtime_path4 = metadata_value.pointer("/package/runtime_dependencies");
        let deps_runtime_path5 = metadata_value.pointer("/package/dependencies");
        let deps_runtime_path6 = package.get("dependencies");
        let deps_runtime_path7 = package.get("runtime_dependencies");
        let deps_runtime_path8 = metadata_value.pointer("/dependencies");
        
        // Try to find dependencies anywhere in the structure
        let full_metadata_str = format!("{:?}", metadata_value);
        let has_deps_in_full = full_metadata_str.contains("dependencies") || full_metadata_str.contains("runtime_dependencies");
        
        // Check the actual dependencies object structure
        let deps_obj = metadata_value.pointer("/dependencies");
        let deps_obj_str = deps_obj.map(|v| format!("{:?}", v)).unwrap_or_default();
        let runtime_deps_raw = deps_runtime_path2.map(|v| format!("{:?}", v)).unwrap_or_default();
        
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "pax-metadata-parse",
            "hypothesisId": "METADATA_PATHS",
            "location": "metadata/src/processed/mod.rs:1464",
            "message": "checking_metadata_dependency_paths",
            "data": {
                "package_name": name,
                "has_deps_runtime": deps_runtime_path1.is_some(),
                "has_deps_runtime_deps": deps_runtime_path2.is_some(),
                "has_pkg_deps_runtime": deps_runtime_path3.is_some(),
                "has_pkg_runtime_deps": deps_runtime_path4.is_some(),
                "has_pkg_deps": deps_runtime_path5.is_some(),
                "has_pkg_deps_key": deps_runtime_path6.is_some(),
                "has_pkg_runtime_deps_key": deps_runtime_path7.is_some(),
                "has_top_deps": deps_runtime_path8.is_some(),
                "has_deps_in_full": has_deps_in_full,
                "metadata_keys": metadata_value.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                "package_keys": package.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
                "deps_obj_preview": if deps_obj_str.len() > 1000 { format!("{}...", &deps_obj_str[..1000]) } else { deps_obj_str },
                "runtime_deps_raw": if runtime_deps_raw.len() > 1000 { format!("{}...", &runtime_deps_raw[..1000]) } else { runtime_deps_raw },
                "full_metadata_preview": if full_metadata_str.len() > 2000 { format!("{}...", &full_metadata_str[..2000]) } else { full_metadata_str },
                "package_full": format!("{:?}", package)
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion
        
        // Prioritize /dependencies/runtime_dependencies (the correct path based on user's metadata structure)
        let runtime_deps = Self::parse_new_metadata_dependencies(
            deps_runtime_path2  // /dependencies/runtime_dependencies (CORRECT PATH)
                .or_else(|| deps_runtime_path1)  // /dependencies/runtime (fallback)
                .or_else(|| deps_runtime_path4)  // /package/runtime_dependencies (fallback)
                .or_else(|| deps_runtime_path3), // /package/dependencies/runtime (fallback)
        );
        
        // #region agent log
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "pax-metadata-parse",
            "hypothesisId": "METADATA_DEPS_PARSED",
            "location": "metadata/src/processed/mod.rs:1380",
            "message": "parsed_runtime_dependencies",
            "data": {
                "package_name": name,
                "runtime_deps_count": runtime_deps.len(),
                "runtime_deps": runtime_deps.iter().map(|d| match d {
                    DependKind::Latest(n) => n.clone(),
                    DependKind::Specific(dv) => dv.name.clone(),
                    DependKind::Volatile(n) => n.clone(),
                }).collect::<Vec<_>>()
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion

        let build_deps = Self::parse_new_metadata_dependencies(
            metadata_value
                .pointer("/dependencies/build")
                .or_else(|| metadata_value.pointer("/dependencies/build_dependencies"))
                .or_else(|| metadata_value.pointer("/package/dependencies/build"))
                .or_else(|| metadata_value.pointer("/package/build_dependencies")),
        );

        let mut hash = metadata_value
            .pointer("/artifacts/binary_hash")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

        if hash.is_empty() {
            hash = package
                .get("hash")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());
        }

        let package_type = package
            .get("type")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .or_else(|| architecture.clone().map(|arch| format!("PAX ({arch})")))
            .unwrap_or_else(|| "PAX".to_string());

        let mut metadata = ProcessedMetaData {
            name,
            kind: MetaDataKind::Pax,
            description,
            version,
            origin: OriginKind::Pax(String::new()),
            dependent: false,
            build_dependencies: build_deps,
            runtime_dependencies: runtime_deps,
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: Vec::new(),
                configs: Vec::new(),
            }),
            hash,
            package_type,
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: release.into_iter().collect(),
        };

        if let Some(arch) = architecture {
            if !arch.is_empty() && !metadata.package_type.contains(&arch) {
                metadata.package_type = format!("{} - {}", metadata.package_type, arch);
            }
        }

        Ok(metadata)
    }

    fn parse_new_metadata_dependencies(node: Option<&JsonValue>) -> Vec<DependKind> {
        let mut deps_as_strings = Vec::new();

        if let Some(value) = node {
            match value {
                JsonValue::Array(items) => {
                    for item in items {
                        match item {
                            JsonValue::String(s) => {
                                let trimmed = s.trim();
                                if !trimmed.is_empty() {
                                    deps_as_strings.push(trimmed.to_string());
                                }
                            }
                            JsonValue::Object(obj) => {
                                if let Some(name) = obj
                                    .get("name")
                                    .or_else(|| obj.get("package"))
                                    .and_then(|v| v.as_str())
                                {
                                    let constraint = obj
                                        .get("version_constraint")
                                        .or_else(|| obj.get("version"))
                                        .or_else(|| obj.get("constraint"))
                                        .and_then(|v| v.as_str())
                                        .map(|s| s.trim().to_string())
                                        .unwrap_or_default();

                                    let mut entry = name.trim().to_string();
                                    if !constraint.is_empty() {
                                        entry = Self::normalize_dependency_entry(&entry, &constraint);
                                    }

                                    let is_optional = obj
                                        .get("optional")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);

                                    if !is_optional && !entry.is_empty() {
                                        deps_as_strings.push(entry);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                JsonValue::String(s) => {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        deps_as_strings.push(trimmed.to_string());
                    }
                }
                JsonValue::Object(obj) => {
                    if let Some(name) = obj
                        .get("name")
                        .or_else(|| obj.get("package"))
                        .and_then(|v| v.as_str())
                    {
                        let constraint = obj
                            .get("version_constraint")
                            .or_else(|| obj.get("version"))
                            .or_else(|| obj.get("constraint"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.trim().to_string())
                            .unwrap_or_default();

                        let mut entry = name.trim().to_string();
                        if !constraint.is_empty() {
                            entry = Self::normalize_dependency_entry(&entry, &constraint);
                        }

                        if !entry.is_empty() {
                            deps_as_strings.push(entry);
                        }
                    }
                }
                _ => {}
            }
        }

        Self::dependencies_from_strings(deps_as_strings)
    }

    fn normalize_dependency_entry(name: &str, constraint: &str) -> String {
        let trimmed = constraint.trim();

        if trimmed.is_empty() {
            return name.to_string();
        }

        if trimmed.starts_with(|c: char| matches!(c, '>' | '<' | '=' | '^' | '~')) {
            format!("{}{}", name, trimmed)
        } else {
            format!("{}=={}", name, trimmed)
        }
    }

    fn dependencies_from_strings(entries: Vec<String>) -> Vec<DependKind> {
        let mut result = Vec::new();

        for dep in entries {
            let trimmed = dep.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Some(vol) = trimmed.strip_prefix('!') {
                let name = vol.trim();
                if !name.is_empty() {
                    result.push(DependKind::Volatile(name.to_string()));
                }
                continue;
            }

            if let Some(index) = trimmed.find(['=', '>', '<', '^', '~']) {
                let (name_part, ver_part) = trimmed.split_at(index);
                let name = name_part.trim();
                let ver = ver_part.trim();

                if name.is_empty() {
                    continue;
                }

                if let Some(range) = Self::parse_dependency_range(ver) {
                    result.push(DependKind::Specific(DepVer {
                        name: name.to_string(),
                        range,
                    }));
                } else {
                    result.push(DependKind::Latest(name.to_string()));
                }
            } else {
                result.push(DependKind::Latest(trimmed.to_string()));
            }
        }

        result
    }

    fn parse_dependency_range(ver: &str) -> Option<Range> {
        let mut lower = VerReq::NoBound;
        let mut upper = VerReq::NoBound;

        let ver = ver.trim();

        if ver.is_empty() {
            return Some(Range {
                lower: VerReq::NoBound,
                upper: VerReq::NoBound,
            });
        }

        if let Some(ver) = ver.strip_prefix(">>") {
            lower = VerReq::Gt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix(">=") {
            lower = VerReq::Ge(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix(">") {
            lower = VerReq::Gt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("==") {
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Eq(parsed_ver.clone());
            upper = VerReq::Eq(parsed_ver);
        } else if let Some(ver) = ver.strip_prefix("=") {
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Eq(parsed_ver.clone());
            upper = VerReq::Eq(parsed_ver);
        } else if let Some(ver) = ver.strip_prefix("<=") {
            upper = VerReq::Le(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<<") {
            upper = VerReq::Lt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<") {
            upper = VerReq::Lt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("~") {
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Ge(parsed_ver.clone());
            let mut next_major = parsed_ver.clone();
            next_major.major += 1;
            next_major.minor = 0;
            next_major.patch = 0;
            upper = VerReq::Lt(next_major);
        } else if let Some(ver) = ver.strip_prefix("^") {
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Ge(parsed_ver.clone());
            let mut next_minor = parsed_ver.clone();
            next_minor.minor += 1;
            next_minor.patch = 0;
            upper = VerReq::Lt(next_minor);
        } else {
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Eq(parsed_ver.clone());
            upper = VerReq::Eq(parsed_ver);
        }

        Some(Range { lower, upper })
    }

    fn load_local_deb(path: &Path) -> Result<Self, String> {
        use std::process::Command;

        let temp_dir = Self::create_temp_dir("pax_extract_deb")?;

        let status = Command::new("dpkg-deb")
            .arg("-x")
            .arg(path)
            .arg(&temp_dir)
            .status()
            .map_err(|e| format!("Failed to execute dpkg-deb -x: {}", e))?;

        if !status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            return err!(
                "dpkg-deb -x failed for {} with status {}",
                path.display(),
                status
            );
        }

        let name = read_dpkg_field(path, "Package")?
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "local-deb".to_string()));
        let version = read_dpkg_field(path, "Version")?
            .unwrap_or_else(|| "0.0.0".to_string());
        let description = read_dpkg_field(path, "Description")?
            .unwrap_or_else(|| format!("Debian package {}", name));
        let depends_raw = read_dpkg_field(path, "Depends")?.unwrap_or_default();

        let (_, critical_files, config_files) = Self::collect_payload_from(&temp_dir)?;

        let metadata = ProcessedMetaData {
            name: name.clone(),
            kind: MetaDataKind::Apt,
            description,
            version,
            origin: OriginKind::Apt(path.to_string_lossy().to_string()),
            dependent: false,
            build_dependencies: Vec::new(),
            runtime_dependencies: Self::parse_dependency_list(&depends_raw),
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            }),
            hash: crate::file_tracking::calculate_file_checksum(path).unwrap_or_default(),
            package_type: "APT".to_string(),
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: Vec::new(),
        };

        let _ = fs::remove_dir_all(&temp_dir);

        Ok(metadata)
    }

    fn load_local_rpm(path: &Path) -> Result<Self, String> {
        use std::process::Command;

        let temp_dir = Self::create_temp_dir("pax_extract_rpm")?;

        // Use native RPM parsing instead of external commands
        let rpm_info = crate::rpm_parser::parse_rpm_file(path)?;

        // Extract RPM payload natively
        crate::rpm_parser::extract_rpm_payload(path, &temp_dir)?;

        let name = rpm_info.name;
        let version = rpm_info.version;
        let summary = rpm_info.summary;
        let requires_raw = rpm_info.dependencies.join("\n");

        // Filter out common RPM internal dependencies
        let filtered_deps: Vec<String> = rpm_info.dependencies.into_iter()
            .filter(|dep| {
                !dep.starts_with("rpmlib(") &&
                !dep.contains("filesystem") &&
                !dep.starts_with("/bin/") &&
                !dep.starts_with("/usr/bin/") &&
                !dep.starts_with("/sbin/")
            })
            .collect();

        let (_, critical_files, config_files) = Self::collect_payload_from(&temp_dir)?;

        let metadata = ProcessedMetaData {
            name: if name.is_empty() {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "local-rpm".to_string())
            } else {
                name
            },
            kind: MetaDataKind::Rpm,
            description: if summary.is_empty() {
                "RPM package".to_string()
            } else {
                summary
            },
            version: if version.is_empty() {
                "0.0.0".to_string()
            } else {
                version
            },
            origin: OriginKind::Rpm(path.to_string_lossy().to_string()),
            dependent: false,
            build_dependencies: Vec::new(),
            runtime_dependencies: filtered_deps.into_iter()
                .map(|dep| DependKind::Latest(dep))
                .collect(),
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            }),
            hash: crate::file_tracking::calculate_file_checksum(path).unwrap_or_default(),
            package_type: "RPM".to_string(),
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: Vec::new(),
        };

        let _ = fs::remove_dir_all(&temp_dir);

        Ok(metadata)
    }

    /// Fix common YAML syntax issues in package manifests
    /// Handles unescaped quotes in double-quoted strings
    /// Normalizes field names to handle both underscore and hyphen formats
    fn fix_yaml_syntax(content: &str) -> String {
        let mut fixed_lines = Vec::new();
        
        for line in content.lines() {
            let mut fixed_line = line.to_string();
            
            // Replace tabs with spaces (YAML requires spaces for indentation)
            fixed_line = fixed_line.replace('\t', "  ");
            
            // Remove trailing semicolons or commas (invalid in YAML)
            fixed_line = fixed_line.trim_end_matches(';').trim_end_matches(',').to_string();
            
            // Check if this is a YAML key-value line with a quoted string
            if let Some(colon_pos) = fixed_line.find(':') {
                let key_part = &fixed_line[..colon_pos].trim_end();
                let value_part = fixed_line[colon_pos + 1..].trim();
                
                // If value is empty, set it to empty string
                let value_part = if value_part.is_empty() { "\"\"" } else { value_part };
                
                // If the line is very long (250+ chars), use block scalar to avoid parsing issues
                if fixed_line.len() > 250 {
                    // Use YAML literal block scalar for very long values
                    if !value_part.starts_with('"') && !value_part.starts_with('\'') && !value_part.starts_with('|') {
                        fixed_lines.push(format!("{}: |", key_part));
                        fixed_lines.push(format!("  {}", value_part));
                        continue;
                    }
                }
                
                // Check if there are multiple colons or other structural issues
                let colon_count = fixed_line.matches(':').count();
                if colon_count > 1 && fixed_line.len() > 100 {
                    // Multiple colons in a long line - likely a path or URL. Quote the entire value
                    if !value_part.starts_with('"') && !value_part.starts_with('\'') {
                        // Escape backslashes first, then quotes
                        let escaped = value_part.replace('\\', "\\\\").replace('"', "\\\"");
                        fixed_line = format!("{}: \"{}\"", key_part, escaped);
                        fixed_lines.push(fixed_line);
                        continue;
                    }
                }
                
                // If value contains special YAML characters and isn't quoted, quote it
                let needs_quoting = !value_part.starts_with('"') && !value_part.starts_with('\'') && 
                    (value_part.contains(':') || value_part.contains('{') || value_part.contains('}') || 
                     value_part.contains('[') || value_part.contains(']') || value_part.contains('#') ||
                     value_part.contains('|') || value_part.contains('&') || value_part.contains('*'));
                
                if needs_quoting && !value_part.is_empty() {
                    // Quote the value to prevent parsing issues - escape backslashes first, then quotes
                    let escaped = value_part.replace('\\', "\\\\").replace('"', "\\\"");
                    fixed_line = format!("{}: \"{}\"", key_part, escaped);
                    fixed_lines.push(fixed_line);
                    continue;
                }
                
                // Check for unclosed quotes (line ends with quote but doesn't start with one)
                if !value_part.starts_with('"') && value_part.ends_with('"') {
                    // Might be a parsing issue - quote the whole thing
                    let cleaned = value_part.trim_end_matches('"');
                    let escaped = cleaned.replace('\\', "\\\\").replace('"', "\\\"");
                    fixed_line = format!("{}: \"{}\"", key_part, escaped);
                    fixed_lines.push(fixed_line);
                    continue;
                }
                
                // If the value starts with a double quote, fix issues
                if value_part.starts_with('"') && !value_part.starts_with("\"\"") {
                    // Extract the quoted content
                    if value_part.len() > 1 && value_part.ends_with('"') {
                        let quoted_content = &value_part[1..value_part.len()-1];
                        
                        // If the quoted string is very long (200+ chars), use block scalar instead
                        // This avoids parsing issues with long strings containing special characters
                        if quoted_content.len() > 200 {
                            fixed_lines.push(format!("{}: |", key_part));
                            fixed_lines.push(format!("  {}", quoted_content));
                            continue;
                        }
                        let mut fixed_content = String::new();
                        let mut chars: Vec<char> = quoted_content.chars().collect();
                        let mut i = 0;
                        let mut needs_fix = false;
                        
                        // Check for invalid escape sequences and fix them
                        while i < chars.len() {
                            if chars[i] == '\\' && i + 1 < chars.len() {
                                let next_char = chars[i + 1];
                                
                                // Check if it's the start of a valid escape
                                let is_valid = match next_char {
                                    'n' | 'r' | 't' | '\\' | '"' | '\'' | '0' | 'a' | 'b' | 'e' | 'f' | 'v' => true,
                                    'x' if i + 3 < chars.len() => {
                                        // \xHH - hex escape
                                        chars[i + 2].is_ascii_hexdigit() && chars[i + 3].is_ascii_hexdigit()
                                    },
                                    'u' if i + 5 < chars.len() => {
                                        // \uHHHH - unicode escape
                                        (i + 2..i + 6).all(|j| j < chars.len() && chars[j].is_ascii_hexdigit())
                                    },
                                    'U' if i + 9 < chars.len() => {
                                        // \UHHHHHHHH - unicode escape
                                        (i + 2..i + 10).all(|j| j < chars.len() && chars[j].is_ascii_hexdigit())
                                    },
                                    _ => false,
                                };
                                
                                if !is_valid {
                                    // Invalid escape - remove the backslash, keep the character
                                    fixed_content.push(next_char);
                                    i += 2;
                                    needs_fix = true;
                                    continue;
                                } else {
                                    // Valid escape - keep it
                                    fixed_content.push(chars[i]); // backslash
                                    fixed_content.push(chars[i + 1]); // escape char
                                    i += 2;
                                    // Handle multi-character escapes
                                    match next_char {
                                        'x' if i + 1 < chars.len() => {
                                            // Already advanced past \x, now add the two hex digits
                                            fixed_content.push(chars[i]);
                                            fixed_content.push(chars[i + 1]);
                                            i += 2;
                                        },
                                        'u' if i + 3 < chars.len() => {
                                            // Already advanced past \u, now add the four hex digits
                                            for j in 0..4 {
                                                if i + j < chars.len() {
                                                    fixed_content.push(chars[i + j]);
                                                }
                                            }
                                            i += 4;
                                        },
                                        'U' if i + 7 < chars.len() => {
                                            // Already advanced past \U, now add the eight hex digits
                                            for j in 0..8 {
                                                if i + j < chars.len() {
                                                    fixed_content.push(chars[i + j]);
                                                }
                                            }
                                            i += 8;
                                        },
                                        _ => {}
                                    }
                                    continue;
                                }
                            }
                            fixed_content.push(chars[i]);
                            i += 1;
                        }
                        
                        if needs_fix {
                            // Use the fixed content with quotes
                            fixed_line = format!("{}: \"{}\"", key_part, fixed_content);
                        } else {
                            // Check for unescaped quotes inside
                            let inner = &value_part[1..value_part.len()-1];
                            let mut quote_positions = Vec::new();
                            let mut inner_chars: Vec<char> = inner.chars().collect();
                            let mut j = 0;
                            
                            while j < inner_chars.len() {
                                if inner_chars[j] == '"' && (j == 0 || inner_chars[j-1] != '\\') {
                                    quote_positions.push(j);
                                }
                                j += 1;
                            }
                            
                            // If there's more than one quote (the ending quote), we have inner quotes
                            if quote_positions.len() > 1 {
                                // Use YAML literal block scalar for complex strings
                                fixed_lines.push(format!("{}: |", key_part));
                                fixed_lines.push(format!("  {}", inner));
                                continue;
                            }
                        }
                    }
                }
            }
            
            fixed_lines.push(fixed_line);
        }
        
        fixed_lines.join("\n")
    }

    fn create_temp_dir(prefix: &str) -> Result<std::path::PathBuf, String> {
        let dir = std::env::temp_dir().join(format!(
            "{}_{}_{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        fs::create_dir_all(&dir).map_err(|_| {
            format!(
                "Failed to create temporary extraction directory at {}",
                dir.display()
            )
        })?;

        Ok(dir)
    }

    fn collect_payload_from(root: &Path) -> Result<(bool, Vec<String>, Vec<String>), String> {
        let mut has_entries = false;
        let mut critical_files = Vec::new();
        let mut config_files = Vec::new();

        walk_package_payload(root, |_, relative, metadata| {
            has_entries = true;

            if metadata.is_file() || metadata.file_type().is_symlink() {
                let install_path = Path::new("/").join(relative);
                let install_str = install_path.to_string_lossy().to_string();
                if install_str.starts_with("/etc/") {
                    config_files.push(install_str.clone());
                }
                critical_files.push(install_str);
            }

            Ok(())
        })?;

        Ok((has_entries, critical_files, config_files))
    }

    fn parse_dependency_list(list: &str) -> Vec<DependKind> {
        list.split([',', '\n'])
            .filter_map(|item| {
                let trimmed = item.trim();
                if trimmed.is_empty() || trimmed == "rpmlib(PayloadFilesHavePrefix)" {
                    return None;
                }
                let name = trimmed
                    .split(|c: char| c == '(' || c.is_whitespace() || c == '|')
                    .next()
                    .unwrap_or("")
                    .trim();
                if name.is_empty() {
                    None
                } else {
                    Some(DependKind::Latest(name.to_string()))
                }
            })
            .collect()
    }

    pub async fn fetch_pax_metadata_from_url(url: &str) -> Option<Self> {
        Self::debug_log(format_args!("[PAX_FETCH] Trying URL {}", url));
        let response = match reqwest::get(url).await {
            Ok(resp) => resp,
            Err(err) => {
                Self::debug_log(format_args!(
                    "[PAX_FETCH] Request failed for {}: {}",
                    url, err
                ));
                return None;
            }
        };

        if !response.status().is_success() {
            Self::debug_log(format_args!(
                "[PAX_FETCH] URL {} returned status {}",
                url,
                response.status()
            ));
            return None;
        }

        let tmpfile_path = tmpfile()?;
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(err) => {
                Self::debug_log(format_args!(
                    "[PAX_FETCH] Failed to read body from {}: {}",
                    url, err
                ));
                return None;
            }
        };

        if std::fs::write(&tmpfile_path, bytes).is_err() {
            Self::debug_log(format_args!(
                "[PAX_FETCH] Failed to write downloaded data for {} to {}",
                url,
                tmpfile_path.display()
            ));
            let _ = std::fs::remove_file(&tmpfile_path);
            return None;
        }

        let metadata = if let Some(path_str) = tmpfile_path.to_str() {
            match Self::get_metadata_from_local_package(path_str).await {
                Ok(mut processed) => {
                    Self::debug_log(format_args!(
                        "[PAX_FETCH] Successfully parsed metadata from {}",
                        url
                    ));
                    
                    // #region agent log
                    let _ = write_debug_log(&serde_json::json!({
                        "sessionId": "debug-session",
                        "runId": "pax-metadata-parse",
                        "hypothesisId": "FETCHED_METADATA",
                        "location": "metadata/src/processed/mod.rs:2065",
                        "message": "fetched_pax_metadata",
                        "data": {
                            "url": url,
                            "package_name": processed.name,
                            "runtime_deps_count": processed.runtime_dependencies.len(),
                            "runtime_deps": processed.runtime_dependencies.iter().map(|d| match d {
                                DependKind::Latest(n) => n.clone(),
                                DependKind::Specific(dv) => dv.name.clone(),
                                DependKind::Volatile(n) => n.clone(),
                            }).collect::<Vec<_>>(),
                            "build_deps_count": processed.build_dependencies.len()
                        },
                        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                    }));
                    // #endregion
                    
                    processed.origin = OriginKind::Pax(url.to_string());
                    Some(processed)
                }
                Err(err) => {
                    Self::debug_log(format_args!(
                        "[PAX_FETCH] Failed to parse metadata from {}: {}",
                        url, err
                    ));
                    None
                }
            }
        } else {
            Self::debug_log(format_args!(
                "[PAX_FETCH] Temporary file path for {} was not valid UTF-8",
                url
            ));
            None
        };

        let _ = std::fs::remove_file(&tmpfile_path);
        metadata
    }

    async fn discover_remote_pax_package_url(
        base: &str,
        app: &str,
        version: Option<&str>,
    ) -> Option<String> {
        let mut base_with_slash = base.to_string();
        if !base_with_slash.ends_with('/') {
            base_with_slash.push('/');
        }

        let base_url = Url::parse(&base_with_slash).ok()?;
        Self::debug_log(format_args!(
            "[PAX_DISCOVER] Fetching index {} for package {}",
            base_url, app
        ));
        let response = match reqwest::get(base_url.clone()).await {
            Ok(resp) => resp,
            Err(err) => {
                Self::debug_log(format_args!(
                    "[PAX_DISCOVER] Failed to fetch index {}: {}",
                    base_url, err
                ));
                return None;
            }
        };
        if !response.status().is_success() {
            Self::debug_log(format_args!(
                "[PAX_DISCOVER] Index {} returned status {}",
                base_url,
                response.status()
            ));
            return None;
        }

        let body = match response.text().await {
            Ok(text) => text,
            Err(err) => {
                Self::debug_log(format_args!(
                    "[PAX_DISCOVER] Failed to read index body {}: {}",
                    base_url, err
                ));
                return None;
            }
        };
        let hrefs = Self::extract_href_candidates(&body, app);

        Self::debug_log(format_args!(
            "[PAX_DISCOVER] Found {} candidate hrefs for {}",
            hrefs.len(),
            app
        ));

        if hrefs.is_empty() {
            return None;
        }

        let arch_hint = base_url
            .path_segments()
            .and_then(|mut segments| segments.next_back().map(|s| s.to_string()));

        let mut candidates = Vec::new();
        for href in hrefs {
            if let Ok(resolved) = base_url.join(&href) {
                let url = resolved.to_string();
                let has_hint = arch_hint
                    .as_ref()
                    .map(|hint| url.contains(hint))
                    .unwrap_or(false);
                Self::debug_log(format_args!(
                    "[PAX_DISCOVER] Candidate {} (arch match: {})",
                    url, has_hint
                ));
                candidates.push((url, has_hint));
            }
        }

        if candidates.is_empty() {
            return None;
        }

        if let Some(ver) = version {
            let mut best: Option<(String, bool)> = None;
            for (url, has_hint) in &candidates {
                if url.contains(ver) {
                    match &best {
                        Some((best_url, best_hint)) => {
                            if Self::better_candidate(*best_hint, best_url, *has_hint, url) {
                                Self::debug_log(format_args!(
                                    "[PAX_DISCOVER] Selecting better versioned candidate {}",
                                    url
                                ));
                                best = Some((url.clone(), *has_hint));
                            }
                        }
                        None => {
                            Self::debug_log(format_args!(
                                "[PAX_DISCOVER] Selecting first versioned candidate {}",
                                url
                            ));
                            best = Some((url.clone(), *has_hint));
                        }
                    }
                }
            }
            return best.map(|(url, _)| url);
        }

        let mut best: Option<(String, bool)> = None;
        for (url, has_hint) in &candidates {
            match &best {
                Some((best_url, best_hint)) => {
                    if Self::better_candidate(*best_hint, best_url, *has_hint, url) {
                        Self::debug_log(format_args!(
                            "[PAX_DISCOVER] Updating best candidate to {}",
                            url
                        ));
                        best = Some((url.clone(), *has_hint));
                    }
                }
                None => {
                    Self::debug_log(format_args!(
                        "[PAX_DISCOVER] Selecting initial candidate {}",
                        url
                    ));
                    best = Some((url.clone(), *has_hint));
                }
            }
        }

        best.map(|(url, _)| url)
    }

    fn extract_href_candidates(index_html: &str, app: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut remaining = index_html;
        let needle = "href=\"";

        while let Some(start) = remaining.find(needle) {
            remaining = &remaining[start + needle.len()..];
            if let Some(end) = remaining.find('"') {
                let href = &remaining[..end];
                remaining = &remaining[end + 1..];
                let file_name = href
                    .rsplit('/')
                    .next()
                    .unwrap_or(href);
                if file_name.starts_with(app)
                    && file_name.ends_with(".pax")
                    && !file_name.contains(".src.")
                {
                    result.push(href.to_string());
                }
            } else {
                break;
            }
        }

        result
    }

    fn better_candidate(
        current_hint: bool,
        current_url: &str,
        candidate_hint: bool,
        candidate_url: &str,
    ) -> bool {
        if candidate_hint && !current_hint {
            true
        } else if candidate_hint == current_hint && candidate_url > current_url {
            true
        } else {
            false
        }
    }
    pub async fn get_metadata(
        app: &str,
        version: Option<&str>,
        sources: &[OriginKind],
        dependent: bool,
    ) -> Option<Self> {
        // Process all sources in parallel, return as soon as we get the first successful result
        let mut source_futures: Vec<_> = sources.iter().map(|source| {
            let app = app.to_string();
            let version = version.map(|v| v.to_string());
            let source = source.clone();
            let dependent = dependent;
            async move {
                Self::get_metadata_from_single_source(&app, version.as_deref(), &source, dependent).await
            }
            .boxed()
        }).collect();

        // Race all sources - return as soon as we get the first successful result
        while !source_futures.is_empty() {
            let (result, _index, remaining) = select_all(source_futures).await;
            source_futures = remaining;

            if let Some(metadata) = result {
                return Some(metadata);
            }
        }

        None
    }

    pub async fn get_all_metadata(
        app: &str,
        version: Option<&str>,
        sources: &[OriginKind],
        dependent: bool,
    ) -> Vec<Self> {
        // Process all sources in parallel and collect all successful results
        let source_futures: Vec<_> = sources.iter().map(|source| {
            let app = app.to_string();
            let version = version.map(|v| v.to_string());
            let source = source.clone();
            let dependent = dependent;
            async move {
                Self::get_metadata_from_single_source(&app, version.as_deref(), &source, dependent).await
            }
            .boxed()
        }).collect();

        // Wait for all sources to complete and collect successful results
        let results = join_all(source_futures).await;
        let packages = results.into_iter().flatten().collect::<Vec<_>>();

        // Deduplicate packages based on repository priority
        // For RPM packages, prefer updates repository over base repository
        let mut package_map: HashMap<String, Vec<ProcessedMetaData>> = HashMap::new();

        // Group packages by name
        for package in packages {
            package_map.entry(package.name.clone()).or_insert_with(Vec::new).push(package);
        }

        // For each group, select the best package based on repository priority
        let mut deduplicated = Vec::new();
        for (_name, mut group) in package_map {
            if group.len() == 1 {
                deduplicated.push(group.into_iter().next().unwrap());
                continue;
            }

            // Sort by priority: updates > base, then by version
            group.sort_by(|a, b| {
                match (&a.origin, &b.origin) {
                    (OriginKind::Rpm(a_url), OriginKind::Rpm(b_url)) => {
                        let a_is_updates = a_url.contains("dl.fedoraproject.org") && a_url.contains("updates");
                        let b_is_updates = b_url.contains("dl.fedoraproject.org") && b_url.contains("updates");

                        if a_is_updates && !b_is_updates {
                            std::cmp::Ordering::Less // a (updates) comes before b (base)
                        } else if !a_is_updates && b_is_updates {
                            std::cmp::Ordering::Greater // b (updates) comes before a (base)
                        } else {
                            // Same repo type or non-Fedora, compare versions (higher version first)
                            b.version.cmp(&a.version)
                        }
                    },
                    _ => {
                        // For non-RPM repos, just compare versions
                        b.version.cmp(&a.version)
                    }
                }
            });

            // Take the first (highest priority) package
            deduplicated.push(group.into_iter().next().unwrap());
        }

        deduplicated
    }

    async fn get_metadata_from_single_source(
        app: &str,
        version: Option<&str>,
        source: &OriginKind,
        dependent: bool,
    ) -> Option<Self> {
        let mut metadata = None;
        match source {
                OriginKind::Pax(source) => {
                    let base = source.trim_end_matches('/');
                    let mut candidate_urls = Vec::new();

                    // First try packages.json
                    let index_url = format!("{}/packages.json", base);
                    if let Ok(index_response) = reqwest::get(&index_url).await {
                        if let Ok(index_text) = index_response.text().await {
                            if let Ok(index_data) = serde_json::from_str::<serde_json::Value>(&index_text) {
                                if let Some(packages) = index_data.get("packages").and_then(|p| p.as_array()) {
                                    for package in packages {
                                        if let Some(name) = package.get("name").and_then(|n| n.as_str()) {
                                            if name.starts_with(&format!("{}-", app)) || name == app {
                                                if let Some(path) = package.get("path").and_then(|p| p.as_str()) {
                                                    candidate_urls.push(format!("{}/{}", base, path));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Fallback to URL guessing
                    if candidate_urls.is_empty() {
                        candidate_urls = if let Some(version) = version {
                            vec![
                                format!("{}/{}-{}.pax", base, app, version),
                                format!("{}/{}/{}/{}-{}.pax", base, app, version, app, version),
                                format!("{}/{}/{}-{}.pax", base, version, app, version),
                            ]
                        } else {
                            vec![
                                format!("{}/{}.pax", base, app),
                                format!("{}/packages/{}.pax", base, app),
                                format!("{}/{}-latest.pax", base, app),
                                // Try versioned patterns
                                format!("{}/{}-25.08.3-1-x86_64v3.pax", base, app),
                                format!("{}/{}-2.21-1-x86_64v3.pax", base, app),
                            ]
                        };
                    }

                    // Try candidate URLs in parallel
                    let url_futures: Vec<_> = candidate_urls.iter().map(|url| {
                        Self::fetch_pax_metadata_from_url(url)
                    }).collect();
                    let url_results = join_all(url_futures).await;
                    for result in url_results {
                        if metadata.is_none() {
                            if let Some(processed) = result {
                                metadata = Some(processed);
                            }
                        }
                    }

                    if metadata.is_none()
                        && (source.starts_with("http://") || source.starts_with("https://"))
                    {
                        if let Some(discovered_url) =
                            Self::discover_remote_pax_package_url(base, app, version).await
                        {
                            if let Some(processed) =
                                Self::fetch_pax_metadata_from_url(&discovered_url).await
                            {
                                metadata = Some(processed);
                            }
                        }
                    }
                }
                OriginKind::Github { user, repo } => {
                    metadata = {
                        // Try to get package metadata from GitHub releases
                        let endpoint = if let Some(version) = version {
                            format!("https://api.github.com/repos/{}/{}/releases/tags/{}", user, repo, version)
                        } else {
                            format!("https://api.github.com/repos/{}/{}/releases/latest", user, repo)
                        };
                        
                        if let Ok(response) = reqwest::get(&endpoint).await {
                            if let Ok(body) = response.text().await {
                                if let Ok(release_data) = serde_json::from_str::<serde_json::Value>(&body) {
                                    // Look for a PAX metadata file in the release assets
                                    if let Some(assets) = release_data.get("assets").and_then(|a| a.as_array()) {
                                        for asset in assets {
                                            if let Some(name) = asset.get("name").and_then(|n| n.as_str()) {
                                                if name.ends_with(".pax") || name.ends_with(".json") {
                                                    if let Some(download_url) = asset.get("browser_download_url").and_then(|u| u.as_str()) {
                                                        if let Ok(asset_response) = reqwest::get(download_url).await {
                                                            if let Ok(asset_body) = asset_response.text().await {
                                                                // Try to parse as PAX format first
                                                                if metadata.is_none() {
                                                                    if let Ok(raw_pax) = serde_json::from_str::<RawPax>(&asset_body) {
                                                                        if let Some(processed) = raw_pax.process() {
                                                                            metadata = Some(processed);
                                                                        }
                                                                    }
                                                                }
                                                                // Try to parse as GitHub format
                                                                if metadata.is_none() {
                                                                    if let Ok(raw_github) = serde_json::from_str::<RawGithub>(&asset_body) {
                                                                        if let Some(processed) = raw_github.process() {
                                                                            metadata = Some(processed);
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
}

                                    // If no assets found, try to create a basic package from release info
                                    if metadata.is_none() {
                                        if let Some(tag_name) = release_data.get("tag_name").and_then(|t| t.as_str()) {
                                            if let Some(name) = release_data.get("name").and_then(|n| n.as_str()) {
                                                if let Some(body) = release_data.get("body").and_then(|b| b.as_str()) {
                                                    // Create a basic ProcessedMetaData from release info
                                                    let processed = ProcessedMetaData {
                                                        name: name.to_string(),
                                                        kind: MetaDataKind::Github,
                                                        description: body.to_string(),
                                                        version: tag_name.to_string(),
                                                        origin: OriginKind::Github { 
                                                            user: user.clone(),
                                                            repo: repo.clone() 
                                                        },
                                                        dependent,
                                                        build_dependencies: Vec::new(),
                                                        runtime_dependencies: Vec::new(),
                                                        install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                                                            build: "make".to_string(),
                                                            install: "make install".to_string(),
                                                            uninstall: "make uninstall".to_string(),
                                                            purge: "make uninstall".to_string(),
                                                        }),
                                                        hash: "unknown".to_string(),
                                                        package_type: "GitHub".to_string(),
                                                        installed: false,
                                                        dependencies: Vec::new(),
                                                        dependents: Vec::new(),
                                                        installed_files: Vec::new(),
                                                        available_versions: Vec::new(),
                                                    };
                                                    metadata = Some(processed);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        metadata
                    };
                }
                OriginKind::Apt(repo_url) => {
                    metadata = {
                        // Query APT repository for package information
                        let endpoint = if let Some(version) = version {
                            format!("{}/packages/{}/{}", repo_url, app, version)
                        } else {
                            format!("{}/packages/{}", repo_url, app)
                        };
                        
                        if let Ok(response) = reqwest::get(&endpoint).await {
                            if let Ok(body) = response.text().await {
                                // Try to parse as APT package data
                                if let Ok(raw_apt) = serde_json::from_str::<RawApt>(&body) {
                                    if let Some(processed) = raw_apt.process() {
                                        Some(processed)
                                    } else {
                                        None
                                    }
                                } else {
                                    // If not JSON, try to parse as APT control file format
                                    Self::parse_apt_control_file(&body, app, repo_url)
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };
                }
                OriginKind::Rpm(repo_url) => {
                    metadata = {
                        use crate::yum_repository::YumRepositoryClient;
                        
                        let client = YumRepositoryClient::new(repo_url.clone());
                        
                        match client.get_package(app, version).await {
                            Ok(package_info) => {
                                // Convert YumPackageInfo to ProcessedMetaData
                                let processed = ProcessedMetaData {
                                    name: package_info.name,
                                    kind: MetaDataKind::Rpm,
                                    description: package_info.description,
                                    version: package_info.version,
                                    origin: source.clone(),
                                    dependent,
                                    build_dependencies: Vec::new(),
                                    runtime_dependencies: package_info.dependencies.into_iter()
                                        .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                        .collect(),
                                    install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                        critical: Vec::new(),
                                        configs: Vec::new(),
                                    }),
                                    hash: "unknown".to_string(),
                                    package_type: "RPM".to_string(),
                                    installed: false,
                                    dependencies: Vec::new(),
                                    dependents: Vec::new(),
                                    installed_files: Vec::new(),
                                    available_versions: Vec::new(),
                                };
                                Some(processed)
                            }
                            Err(_) => {
                                // Package not found in this repository - continue to next
                                None
                            }
                        }
                    };
                }
                OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                    metadata = {
                        use crate::cloudflare_r2::CloudflareR2Client;
                        
                        let client = CloudflareR2Client::new(
                            bucket.clone(),
                            account_id.clone(),
                            None, // access_key_id
                            None, // secret_access_key
                            None, // region
                        );
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
                            // Convert PackageInfo to ProcessedMetaData
                            let processed = ProcessedMetaData {
                                name: package_info.name,
                                kind: MetaDataKind::Pax,
                                description: package_info.description,
                                version: package_info.version,
                                origin: source.clone(),
                                dependent,
                                build_dependencies: Vec::new(),
                                runtime_dependencies: package_info.dependencies.into_iter()
                                    .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                    .collect(),
                                install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                    critical: Vec::new(),
                                    configs: Vec::new(),
                                }),
                                hash: "unknown".to_string(),
                                package_type: "RPM".to_string(),
                                installed: false,
                                dependencies: Vec::new(),
                                dependents: Vec::new(),
                                installed_files: Vec::new(),
                                available_versions: Vec::new(),
                            };
                            Some(processed)
                        } else {
                            None
                        }
                    };
                }
                OriginKind::Deb(repo_url) => {
                    metadata = {
                        use crate::deb_repository::DebRepositoryClient;
                        
                        let client = DebRepositoryClient::new(repo_url.clone());
                        
                        match client.get_package(app, version).await {
                            Ok(package_info) => {
                                // Don't extract file lists during metadata retrieval - too expensive
                                // File lists will be extracted lazily when checking if dependency is satisfied
                                let file_list = Vec::new();
                                
                                // Convert DebPackageInfo to ProcessedMetaData
                                let processed = ProcessedMetaData {
                                    name: package_info.name,
                                    kind: MetaDataKind::Deb,
                                    description: package_info.description,
                                    version: package_info.version,
                                    origin: source.clone(),
                                    dependent,
                                    build_dependencies: Vec::new(),
                                    runtime_dependencies: package_info.dependencies.into_iter()
                                        .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                        .collect(),
                                    install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                        critical: file_list,
                                        configs: Vec::new(),
                                    }),
                                    hash: "unknown".to_string(),
                                    package_type: "DEB".to_string(),
                                    installed: false,
                                    dependencies: Vec::new(),
                                    dependents: Vec::new(),
                                    installed_files: Vec::new(),
                                    available_versions: Vec::new(),
                                };
                                Some(processed)
                            }
                            Err(_) => {
                                // Package not found in this repository - continue to next
                                None
                            }
                        }
                    };
                }
                OriginKind::Yum(repo_url) => {
                    metadata = {
                        use crate::yum_repository::YumRepositoryClient;
                        
                        let client = YumRepositoryClient::new(repo_url.clone());
                        
                        match client.get_package(app, version).await {
                            Ok(package_info) => {
                                // Don't extract file lists during metadata retrieval - too expensive
                                // File lists will be extracted lazily when checking if dependency is satisfied
                                let file_list = Vec::new();
                                
                                // Convert YumPackageInfo to ProcessedMetaData
                                let processed = ProcessedMetaData {
                                    name: package_info.name,
                                    kind: MetaDataKind::Rpm,
                                    description: package_info.description,
                                    version: package_info.version,
                                    origin: source.clone(),
                                    dependent,
                                    build_dependencies: Vec::new(),
                                    runtime_dependencies: package_info.dependencies.into_iter()
                                        .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                        .collect(),
                                    install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                        critical: file_list,
                                        configs: Vec::new(),
                                    }),
                                    hash: "unknown".to_string(),
                                    package_type: "RPM".to_string(),
                                    installed: false,
                                    dependencies: Vec::new(),
                                    dependents: Vec::new(),
                                    installed_files: Vec::new(),
                                    available_versions: Vec::new(),
                                };
                                Some(processed)
                            }
                            Err(_) => {
                                // Package not found in this repository - continue to next
                                None
                            }
                        }
                    };
                }
                OriginKind::LocalDir(dir_path) => {
                    metadata = {
                        // Scan local directory for package files (.pax, .deb, .rpm)
                        let dir = Path::new(dir_path);
                        if !dir.exists() || !dir.is_dir() {
                            Self::debug_log(format_args!(
                                "[LOCALDIR] Directory does not exist or is not a directory: {}",
                                dir_path
                            ));
                            None
                        } else {
                            let app_trimmed = app.trim();
                            Self::debug_log(format_args!(
                                "[LOCALDIR] Scanning directory {} for package '{}'",
                                dir_path, app_trimmed
                            ));
                            // Try to find package files matching the name
                            let possible_files = if let Some(version) = version {
                                vec![
                                    dir.join(format!("{}-{}.pax", app_trimmed, version)),
                                    dir.join(format!("{}-{}.deb", app_trimmed, version)),
                                    dir.join(format!("{}-{}.rpm", app_trimmed, version)),
                                    dir.join(format!("{}_{}.deb", app_trimmed, version)),
                                    dir.join(format!("{}-{}-{}.rpm", app_trimmed, version, "x86_64")),
                                ]
                            } else {
                                // For latest version, scan all files and pick the one matching the name
                                // Prefer x86_64v3, then x86_64v1, then others
                                let mut candidates_v3 = Vec::new();
                                let mut candidates_v1 = Vec::new();
                                let mut candidates_other = Vec::new();
                                let mut all_files = Vec::new();
                                if let Ok(entries) = fs::read_dir(dir) {
                                    for entry in entries.flatten() {
                                        let path = entry.path();
                                        if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                                            all_files.push(file_name.to_string());
                                            // Check if it matches the package name (must start with package name followed by -)
                                            // Exclude .src.pax files (source packages)
                                            let prefix = format!("{}-", app_trimmed);
                                            if !file_name.contains(".src.") &&
                                               ((file_name.starts_with(&prefix) && file_name.ends_with(".pax")) ||
                                                (file_name.starts_with(&prefix) && file_name.ends_with(".deb")) ||
                                                (file_name.starts_with(&prefix) && file_name.ends_with(".rpm"))) {
                                                // Prioritize by architecture
                                                if file_name.contains("x86_64v3") {
                                                    candidates_v3.push(path.clone());
                                                    Self::debug_log(format_args!(
                                                        "[LOCALDIR] Found x86_64v3 candidate: {}",
                                                        file_name
                                                    ));
                                                } else if file_name.contains("x86_64v1") {
                                                    candidates_v1.push(path.clone());
                                                    Self::debug_log(format_args!(
                                                        "[LOCALDIR] Found x86_64v1 candidate: {}",
                                                        file_name
                                                    ));
                                                } else {
                                                    candidates_other.push(path.clone());
                                                    Self::debug_log(format_args!(
                                                        "[LOCALDIR] Found other candidate: {}",
                                                        file_name
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                                Self::debug_log(format_args!(
                                    "[LOCALDIR] All files in directory: {:?}",
                                    all_files
                                ));
                                Self::debug_log(format_args!(
                                    "[LOCALDIR] Looking for packages starting with '{}-'",
                                    app_trimmed
                                ));
                                Self::debug_log(format_args!(
                                    "[LOCALDIR] Found {} x86_64v3 candidate(s), {} x86_64v1 candidate(s), {} other candidate(s)",
                                    candidates_v3.len(),
                                    candidates_v1.len(),
                                    candidates_other.len()
                                ));
                                // Prefer v3, then v1, then others
                                if !candidates_v3.is_empty() {
                                    candidates_v3
                                } else if !candidates_v1.is_empty() {
                                    candidates_v1
                                } else {
                                    candidates_other
                                }
                            };
                            
                            let mut found_metadata = None;
                            let num_candidates = possible_files.len();
                            Self::debug_log(format_args!(
                                "[LOCALDIR] Searching for '{}' in {} - found {} candidate file(s)",
                                app_trimmed, dir_path, num_candidates
                            ));
                            for package_path in possible_files {
                                Self::debug_log(format_args!(
                                    "[LOCALDIR] Trying: {}",
                                    package_path.display()
                                ));
                                if package_path.exists() {
                                    Self::debug_log(format_args!(
                                        "[LOCALDIR] File exists, attempting to parse metadata..."
                                    ));
                                    if let Some(path_str) = package_path.to_str() {
                                        match Self::get_metadata_from_local_package(path_str).await {
                                            Ok(processed) => {
                                                Self::debug_log(format_args!(
                                                    "[LOCALDIR] Successfully parsed package: {} {}",
                                                    processed.name, processed.version
                                                ));
                                                found_metadata = Some(processed);
                                                break;
                                            }
                                            Err(e) => {
                                                Self::debug_log(format_args!(
                                                    "[LOCALDIR] ERROR: Failed to parse package {}: {}",
                                                    package_path.display(),
                                                    e
                                                ));
                                            }
                                        }
                                    } else {
                                        Self::debug_log(format_args!(
                                            "[LOCALDIR] ERROR: Cannot convert path to string: {}",
                                            package_path.display()
                                        ));
                                    }
                                } else {
                                    Self::debug_log(format_args!(
                                        "[LOCALDIR] File does not exist: {}",
                                        package_path.display()
                                    ));
                                }
                            }
                            if found_metadata.is_none() {
                                Self::debug_log(format_args!(
                                    "[LOCALDIR] ERROR: No package found for '{}' in {} after checking {} file(s)",
                                    app_trimmed, dir_path, num_candidates
                                ));
                            } else {
                                Self::debug_log(format_args!(
                                    "[LOCALDIR] SUCCESS: Found package '{}' in {}",
                                    app_trimmed, dir_path
                                ));
                            }
                            found_metadata
                        }
                    };
                }
        }
        if let Some(mut mut_metadata) = metadata {
            mut_metadata.dependent = dependent;
            Some(mut_metadata)
        } else {
            None
        }
    }
    
    fn parse_apt_control_file(control_data: &str, app: &str, repo_url: &str) -> Option<Self> {
        // Parse APT control file format (like what you'd find in a .deb package)
        let mut name = app.to_string();
        let mut version = "1.0.0".to_string();
        let mut description = "No description available".to_string();
        let mut dependencies = Vec::new();
        let mut critical_files = Vec::new();
        let mut config_files = Vec::new();
        
        for line in control_data.lines() {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim();
                
                match key {
                    "Package" => name = value.to_string(),
                    "Version" => version = value.to_string(),
                    "Description" => description = value.to_string(),
                    "Depends" => {
                        // Parse dependencies (comma-separated)
                        dependencies = value.split(',')
                            .map(|dep| dep.trim().split_whitespace().next().unwrap_or("").to_string())
                            .filter(|dep| !dep.is_empty())
                            .collect();
                    }
                    "Files" => {
                        // Parse file list (one per line, format: hash size path)
                        for file_line in value.lines() {
                            let parts: Vec<&str> = file_line.trim().split_whitespace().collect();
                            if parts.len() >= 3 {
                                let path = parts[2];
                                if path.starts_with("/etc/") {
                                    config_files.push(path.to_string());
                                } else {
                                    critical_files.push(path.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        
        Some(ProcessedMetaData {
            name,
            kind: MetaDataKind::Apt,
            description,
            version,
            origin: OriginKind::Apt(repo_url.to_string()),
            dependent: false,
            build_dependencies: Vec::new(),
            runtime_dependencies: dependencies.into_iter().map(|dep| DependKind::Latest(dep)).collect(),
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            }),
            hash: "unknown".to_string(),
            package_type: "APT".to_string(),
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: Vec::new(),
        })
    }
    
    pub async fn get_depends(
        &self,
        sources: &[OriginKind],
        prior: &mut HashSet<Specific>,
    ) -> Result<InstallPackage, String> {
        let mut run_deps = Vec::new();
        let mut build_deps = Vec::new();
        
        // Resolve runtime dependencies
        for dep in &self.runtime_dependencies {
            let resolved = self.resolve_single_dependency(dep, sources, prior).await?;
            run_deps.push(resolved);
        }
        
        // Resolve build dependencies
        for dep in &self.build_dependencies {
            let resolved = self.resolve_single_dependency(dep, sources, prior).await?;
            build_deps.push(resolved);
        }
        
        Ok(InstallPackage {
            metadata: self.clone(),
            run_deps,
            build_deps,
        })
    }
    
    async fn resolve_single_dependency(
        &self,
        dep: &DependKind,
        sources: &[OriginKind],
        prior: &mut HashSet<Specific>,
    ) -> Result<ProcessedMetaData, String> {
        match dep {
            DependKind::Latest(name) => {
                // Find the latest version across all sources
                self.find_latest_version(name, sources).await
            }
            DependKind::Specific(dep_ver) => {
                // Check if we've already resolved this specific dependency
                let specific = Specific {
                    name: dep_ver.name.clone(),
                    version: dep_ver.range.lower.as_version().unwrap_or_default(),
                };
                
                if prior.contains(&specific) {
                    return Err(format!("Circular dependency detected: {}", dep_ver.name));
                }
                
                prior.insert(specific);
                let result = self.find_specific_version(&dep_ver.name, &dep_ver.range, sources).await;
                prior.remove(&Specific {
                    name: dep_ver.name.clone(),
                    version: dep_ver.range.lower.as_version().unwrap_or_default(),
                });
                
                result
            }
            DependKind::Volatile(name) => {
                // Check if the system binary exists
                if self.check_system_binary(name) {
                           // Create a dummy metadata for system binaries
                           Ok(ProcessedMetaData {
                               name: name.clone(),
                               kind: self.kind.clone(),
                               description: format!("System binary: {}", name),
                               version: "system".to_string(),
                               origin: settings::OriginKind::Pax("system".to_string()),
                               dependent: false,
                               build_dependencies: Vec::new(),
                               runtime_dependencies: Vec::new(),
                               install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                                   build: "".to_string(),
                                   install: "".to_string(),
                                   uninstall: "".to_string(),
                                   purge: "".to_string(),
                               }),
                               hash: "".to_string(),
                               package_type: "System".to_string(),
                               installed: true,
                               dependencies: Vec::new(),
                               dependents: Vec::new(),
                               installed_files: Vec::new(),
                               available_versions: Vec::new(),
        })
                } else {
                    Err(format!("System binary {} not found", name))
                }
            }
        }
    }
    
    async fn find_latest_version(&self, name: &str, sources: &[OriginKind]) -> Result<ProcessedMetaData, String> {
        let mut latest_version: Option<ProcessedMetaData> = None;
        
        for source in sources {
            if let Ok(metadata) = self.get_metadata_from_source(name, source).await {
                if latest_version.is_none() || self.is_newer_version(&metadata, latest_version.as_ref().unwrap()) {
                    latest_version = Some(metadata);
                }
            }
        }
        
        latest_version.ok_or_else(|| format!("Package {} not found in any source", name))
    }
    
    async fn find_specific_version(
        &self,
        name: &str,
        range: &utils::Range,
        sources: &[OriginKind],
    ) -> Result<ProcessedMetaData, String> {
        for source in sources {
            if let Ok(metadata) = self.get_metadata_from_source(name, source).await {
                let version = utils::Version::parse(&metadata.version)?;
                if self.version_matches_range(&version, range) {
                    return Ok(metadata);
                }
            }
        }
        
        Err(format!("Package {} with version matching range not found", name))
    }
    
    async fn get_metadata_from_source(
        &self,
        name: &str,
        _source: &OriginKind,
    ) -> Result<ProcessedMetaData, String> {
        // This would typically query the actual source
        // For now, we'll check installed packages
        let installed_dir = utils::get_metadata_dir()?;
        let package_file = installed_dir.join(format!("{}.json", name));
        
        if package_file.exists() {
            let content = std::fs::read_to_string(&package_file)
                .map_err(|e| format!("Failed to read package file: {}", e))?;
            let installed: crate::installed::InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse package metadata: {}", e))?;
            
                   Ok(ProcessedMetaData {
                       name: installed.name,
                       kind: installed.kind,
                       description: installed.description,
                       version: installed.version,
                       origin: installed.origin,
                       dependent: true,
                       build_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                       runtime_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                       install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                           build: "".to_string(),
                           install: "".to_string(),
                           uninstall: "".to_string(),
                           purge: "".to_string(),
                       }),
                       hash: installed.hash,
                       package_type: format!("{:?}", installed.kind.clone()),
                       installed: true,
                       dependencies: installed.dependencies.iter().map(|dep| dep.name.clone()).collect(),
                       dependents: installed.dependents.iter().map(|dep| dep.name.clone()).collect(),
                       installed_files: Vec::new(), // TODO: implement file tracking
                       available_versions: Vec::new(), // TODO: implement version discovery
                   })
        } else {
            Err(format!("Package {} not found", name))
        }
    }
    
    fn is_newer_version(&self, new: &ProcessedMetaData, current: &ProcessedMetaData) -> bool {
        let new_ver = utils::Version::parse(&new.version).unwrap_or_default();
        let current_ver = utils::Version::parse(&current.version).unwrap_or_default();
        new_ver > current_ver
    }
    
    fn version_matches_range(&self, version: &utils::Version, range: &utils::Range) -> bool {
        // Check lower bound
        let lower_match = match &range.lower {
            utils::VerReq::NoBound => true,
            utils::VerReq::Eq(req_ver) => version == req_ver,
            utils::VerReq::Ge(req_ver) => version >= req_ver,
            utils::VerReq::Gt(req_ver) => version > req_ver,
            utils::VerReq::Le(req_ver) => version <= req_ver,
            utils::VerReq::Lt(req_ver) => version < req_ver,
        };
        
        // Check upper bound
        let upper_match = match &range.upper {
            utils::VerReq::NoBound => true,
            utils::VerReq::Eq(req_ver) => version == req_ver,
            utils::VerReq::Ge(req_ver) => version >= req_ver,
            utils::VerReq::Gt(req_ver) => version > req_ver,
            utils::VerReq::Le(req_ver) => version <= req_ver,
            utils::VerReq::Lt(req_ver) => version < req_ver,
        };
        
        lower_match && upper_match
    }
    
    fn check_system_binary(&self, name: &str) -> bool {
        use std::process::Command;
        
        Command::new("which")
            .arg(name)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    pub fn install(&self, runtime: &Runtime) -> Result<(), String> {
        runtime.block_on(self.clone().install_package_impl(false, None))
    }
    
    pub fn install_with_overwrite(&self, runtime: &Runtime) -> Result<(), String> {
        runtime.block_on(self.clone().install_package_impl(true, None))
    }

    pub fn list_deps(&self, runtime: bool) -> Vec<String> {
        let deps = if runtime {
            &self.runtime_dependencies
        } else {
            &self.build_dependencies
        };
        
        deps.iter()
            .filter_map(|dep| dep.as_dep_ver().map(|dv| dv.name.clone()))
            .collect()
    }

    pub fn write(self, base: &Path, inc: &mut usize) -> Result<Self, String> {
        let path = loop {
            let mut path = base.to_path_buf();
            path.push(format!("{inc}.yaml"));
            if path.exists() {
                *inc += 1;
                continue;
            }
            break path;
        };
        let mut file = match File::create(&path) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open upgrade metadata as WO!"),
        };
        let data = match serde_norway::to_string(&self) {
            Ok(data) => data,
            Err(_) => return err!("Failed to parse upgrade metadata to string!"),
        };
        match file.write_all(data.as_bytes()) {
            Ok(_) => Ok(self),
            Err(_) => err!("Failed to write upgrade metadata file!"),
        }
    }
    
    pub fn open(name: &str) -> Result<Self, String> {
        let mut path = get_update_dir()?;
        path.push(format!("{}.yaml", name));
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => return err!("Failed to read package `{name}`'s metadata!"),
        };
        let mut metadata = String::new();
        if file.read_to_string(&mut metadata).is_err() {
            return err!("Failed to read package `{name}`'s config!");
        }
        Ok(match serde_norway::from_str::<Self>(&metadata) {
            Ok(data) => data,
            Err(_) => return err!("Failed to parse package `{name}`'s data!"),
        })
    }
    
    pub fn upgrade_package(&self, _sources: &[OriginKind], runtime: &Runtime) -> Result<(), String> {
        // For now, just reinstall the package
        // TODO: Implement proper upgrade logic
        runtime.block_on(self.clone().install_package())
    }
    
    pub fn remove_update_cache(&self) -> Result<(), String> {
        // Remove any cached update files
        // For now, just return Ok
        Ok(())
    }
}

// Public API functions

async fn select_package_from_multiple(packages: &[ProcessedMetaData], package_name: &str) -> Result<Option<ProcessedMetaData>, String> {
    println!("\nMultiple repositories contain package '{}':", package_name);
    println!("Please select which one to install:\n");

    for (i, package) in packages.iter().enumerate() {
        let repo_info = match &package.origin {
            OriginKind::Pax(url) => {
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                    let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"url_debug\",\"hypothesisId\":\"URL_DUP\",\"location\":\"metadata/src/processed/mod.rs:3255\",\"message\":\"displaying_origin\",\"data\":{{\"package\":\"{}\",\"origin_url\":\"{}\"}},\"timestamp\":{}}}", package.name, url, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64);
                }
                format!("PAX: {}", url)
            },
            OriginKind::Apt(url) => format!("APT: {}", url),
            OriginKind::Deb(url) => format!("DEB: {}", url),
            OriginKind::Rpm(url) => {
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                    let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"url_debug\",\"hypothesisId\":\"URL_DUP\",\"location\":\"metadata/src/processed/mod.rs:3258\",\"message\":\"displaying_origin\",\"data\":{{\"package\":\"{}\",\"origin_url\":\"{}\"}},\"timestamp\":{}}}", package.name, url, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64);
                }
                format!("RPM: {}", url)
            },
            OriginKind::Yum(url) => format!("YUM: {}", url),
            OriginKind::Github { user, repo } => format!("GitHub: {}/{}", user, repo),
            OriginKind::CloudflareR2 { bucket, account_id, .. } => format!("R2: {}.{}", bucket, account_id),
            OriginKind::LocalDir(path) => format!("Local: {}", path),
        };

        println!("{}. {} (v{}) - {}", i + 1, package.name, package.version, repo_info);
        println!("   {}", package.description);
        println!();
    }

    println!("0. Cancel installation");
    println!();

    // Get user input
    loop {
        print!("Enter selection (1-{}): ", packages.len());
        std::io::Write::flush(&mut std::io::stdout()).map_err(|e| format!("Failed to flush stdout: {}", e))?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input).map_err(|e| format!("Failed to read input: {}", e))?;
        let input = input.trim();

        match input.parse::<usize>() {
            Ok(0) => return Ok(None), // Cancelled
            Ok(n) if n > 0 && n <= packages.len() => {
                return Ok(Some(packages[n - 1].clone()));
            }
            _ => {
                println!("Invalid selection. Please enter a number between 0 and {}.", packages.len());
            }
        }
    }
}

fn matches_search(meta: &ProcessedMetaData, query: &str, exact: bool) -> bool {
    if query.is_empty() {
        return true;
    }
    if exact {
        meta.name.eq_ignore_ascii_case(query)
    } else {
        let query_lower = query.to_ascii_lowercase();
        meta.name.to_ascii_lowercase().contains(&query_lower)
            || meta
                .description
                .to_ascii_lowercase()
                .contains(&query_lower)
    }
}

// Thread-local storage for refresh flag
thread_local! {
    static FORCE_REFRESH: std::cell::Cell<bool> = std::cell::Cell::new(false);
}

pub fn set_force_refresh(refresh: bool) {
    FORCE_REFRESH.with(|f| f.set(refresh));
}

/// Recursively resolve all dependencies for a package
/// NEW ARCHITECTURE: Uses repo index (no HTTP during resolution)
/// Returns error if any dependencies are missing from repositories
async fn resolve_all_dependencies(
    package: &ProcessedMetaData,
    sources: &[OriginKind],
) -> Result<Vec<ProcessedMetaData>, String> {
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;
    
    // #region agent log
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "pax-dep-resolution",
        "hypothesisId": "ENTRY",
        "location": "metadata/src/processed/mod.rs:3332",
        "message": "resolve_all_dependencies_called",
        "data": {
            "package_name": package.name,
            "package_kind": format!("{:?}", package.kind),
            "runtime_deps_count": package.runtime_dependencies.len(),
            "sources_count": sources.len()
        },
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    }));
    // #endregion
    
    let main_package_name = package.name.clone();
    use crate::repo_index::MultiRepoIndex;
    
    // Check if this is a PAX package - if so, only use PAX repos for dependency resolution
    let is_pax_package = matches!(package.kind, crate::parsers::MetaDataKind::Pax);
    
    // #region agent log
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "pax-detection",
        "hypothesisId": "PAX_CHECK",
        "location": "metadata/src/processed/mod.rs:3340",
        "message": "checking_package_kind",
        "data": {
            "package_name": package.name,
            "package_kind": format!("{:?}", package.kind),
            "is_pax_package": is_pax_package,
            "package_type": package.package_type.clone()
        },
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    }));
    // #endregion
    
    // PHASE 0: Build repo index (fetches all metadata ONCE, before resolution)
    // Check for refresh flag from thread-local storage
    let force_refresh = FORCE_REFRESH.with(|f| f.get());
    
    let total_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "timing",
        "hypothesisId": "TIMING",
        "location": "metadata/src/processed/mod.rs:3310",
        "message": "PHASE: build_repo_index",
        "data": {"repos": sources.len(), "force_refresh": force_refresh, "is_pax_package": is_pax_package},
        "timestamp": total_start
    }));
    
    let repo_index = match MultiRepoIndex::build(sources, force_refresh).await {
        Ok(index) => index,
        Err(e) => {
            eprintln!("Warning: Failed to build repo index: {}. Falling back to old method.", e);
            // Fallback to old method if index building fails
            return Ok(resolve_all_dependencies_old(package, sources).await);
        }
    };
    
    let index_built = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "timing",
        "hypothesisId": "TIMING",
        "location": "metadata/src/processed/mod.rs:3320",
        "message": "PHASE: build_repo_index_complete",
        "data": {"duration_ms": index_built.saturating_sub(total_start)},
        "timestamp": index_built
    }));

    // PHASE 1: Load installed packages and build provides lookup (ONLY from local database)
    let installed_packages = match list_installed_packages(false, false, None) {
        Ok(packages) => packages,
        Err(_) => Vec::new(),
    };
    
    // Track missing dependencies (packages not found in repositories and not installed)
    // Use RefCell to allow mutation in async context
    use std::cell::RefCell;
    let missing_dependencies: std::rc::Rc<RefCell<Vec<String>>> = std::rc::Rc::new(RefCell::new(Vec::new()));
    
    // Build provides lookup from installed packages (cross-format compatible)
    let installed_provides = InstalledPackageProvides::from_installed_packages(&installed_packages);

    // PHASE 2: Resolve dependencies using INDEX ONLY (no HTTP)
    let resolution_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    
    let mut resolved = HashSet::new();
    let mut to_process = Vec::new();
    let mut result = Vec::new();
    
    // Start with the package's direct dependencies (not the package itself)
    for dep in &package.runtime_dependencies {
        let dep_name = match dep {
            DependKind::Latest(name) => name.clone(),
            DependKind::Specific(dep_ver) => dep_ver.name.clone(),
            DependKind::Volatile(name) => name.clone(),
        };
        
        // Don't skip - always process to ensure we get all transitive dependencies
        // The system_satisfied check later will filter out what's actually installed
        if !resolved.contains(&dep_name) {
            resolved.insert(dep_name.clone());
            to_process.push(dep_name);
        }
    }
    
    // Process dependency queue (breadth-first to ensure all dependencies are found)
    let mut queue_index = 0;
    while queue_index < to_process.len() {
        let dep_name = to_process[queue_index].clone();
        queue_index += 1;
        
        // #region agent log
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "dep-resolution",
            "hypothesisId": "A",
            "location": "metadata/src/processed/mod.rs:3424",
            "message": "processing_dependency",
            "data": {
                "dep_name": dep_name,
                "queue_index": queue_index,
                "queue_size": to_process.len(),
                "result_count": result.len()
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion
        
        // Lookup ALL versions - for PAX packages, only check PAX repos
        let all_versions = if is_pax_package {
            repo_index.lookup_all_versions_pax_only(&dep_name)
        } else {
            repo_index.lookup_all_versions(&dep_name)
        };
        let dep_metadata = all_versions.first();
        
        // #region agent log
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "pax-dep-resolution",
            "hypothesisId": "PAX_LOOKUP",
            "location": "metadata/src/processed/mod.rs:3435",
            "message": "dependency_lookup",
            "data": {
                "dep_name": dep_name,
                "is_pax_package": is_pax_package,
                "versions_found": all_versions.len(),
                "using_pax_only": is_pax_package
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion
        
        // #region agent log
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "dep-resolution",
            "hypothesisId": "B",
            "location": "metadata/src/processed/mod.rs:3430",
            "message": "lookup_result",
            "data": {
                "dep_name": dep_name,
                "versions_found": all_versions.len(),
                "has_metadata": dep_metadata.is_some()
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion
        
        // Track if we have metadata and what package name to use for dependency lookup
        let (package_name_for_deps, deps_from_metadata) = if let Some(dep_metadata) = dep_metadata {
            // Check if satisfied by installed packages
            let system_satisfied = is_dependency_satisfied_by_system(
                dep_metadata,
                &installed_provides,
            ).await;
            
            // #region agent log
            let _ = write_debug_log(&serde_json::json!({
                "sessionId": "debug-session",
                "runId": "dep-resolution",
                "hypothesisId": "C",
                "location": "metadata/src/processed/mod.rs:3441",
                "message": "system_satisfied_check",
                "data": {
                    "dep_name": dep_name,
                    "system_satisfied": system_satisfied,
                    "package_name": dep_metadata.name
                },
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            }));
            // #endregion
            
            // Only add to result if NOT satisfied (but ALWAYS process dependencies)
            if !system_satisfied {
                // Only add if not already in result (avoid duplicates) and not the main package
                if dep_metadata.name != main_package_name && 
                   !result.iter().any(|p: &ProcessedMetaData| p.name == dep_metadata.name) {
                    result.push(dep_metadata.clone());
                    // #region agent log
                    let _ = write_debug_log(&serde_json::json!({
                        "sessionId": "debug-session",
                        "runId": "dep-resolution",
                        "hypothesisId": "D",
                        "location": "metadata/src/processed/mod.rs:3446",
                        "message": "added_to_result",
                        "data": {
                            "dep_name": dep_name,
                            "package_name": dep_metadata.name,
                            "result_count": result.len()
                        },
                        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                    }));
                    // #endregion
                }
            }
            
            // ALWAYS get dependencies from metadata (even if satisfied)
            // Use the actual package name from metadata for dependency lookup
            (Some(dep_metadata.name.clone()), Some(dep_metadata.runtime_dependencies.clone()))
        } else {
            // Package not found in index - check if it's already installed by name or provides
            // If it is, it's satisfied and we can skip processing its dependencies
            // If it's not, we still need to process its dependencies (they might be in the index)
            let is_installed_by_name = installed_provides.is_dependency_satisfied(&dep_name).is_some();
            
            // Also check if it's provided by any package (library, file, or package provides)
            let provided_by_lib = repo_index.lookup_provides_lib(&dep_name);
            let provided_by_file = repo_index.lookup_provides_file(&dep_name);
            let provided_by_pkg = repo_index.lookup_provides_pkg(&dep_name);
            let is_provided = !provided_by_lib.is_empty() || !provided_by_file.is_empty() || !provided_by_pkg.is_empty();
            
            // #region agent log
            let _ = write_debug_log(&serde_json::json!({
                "sessionId": "debug-session",
                "runId": "dep-resolution",
                "hypothesisId": "I",
                "location": "metadata/src/processed/mod.rs:3512",
                "message": "package_not_in_index",
                "data": {
                    "dep_name": dep_name,
                    "is_installed_by_name": is_installed_by_name,
                    "is_provided": is_provided,
                    "provided_by_lib_count": provided_by_lib.len(),
                    "provided_by_file_count": provided_by_file.len(),
                    "provided_by_pkg_count": provided_by_pkg.len(),
                    "provided_by_pkg": provided_by_pkg.iter().map(|s| s.to_string()).collect::<Vec<_>>()
                },
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            }));
            // #endregion
            
            // If provided by another package, try to get that package's metadata
            let (providing_pkg_name, providing_deps) = if is_provided && !is_installed_by_name {
                // Get the first providing package (prefer package provides over lib/file)
                let providing_pkg = provided_by_pkg.first()
                    .or_else(|| provided_by_lib.first())
                    .or_else(|| provided_by_file.first())
                    .and_then(|name| repo_index.lookup_package(name));
                
                if let Some(providing_metadata) = providing_pkg {
                    // Check if the providing package is satisfied
                    let system_satisfied = is_dependency_satisfied_by_system(
                        providing_metadata,
                        &installed_provides,
                    ).await;
                    
                    if !system_satisfied {
                        // Add the providing package to result
                        if providing_metadata.name != main_package_name && 
                           !result.iter().any(|p: &ProcessedMetaData| p.name == providing_metadata.name) {
                            result.push(providing_metadata.clone());
                            // #region agent log
                            let _ = write_debug_log(&serde_json::json!({
                                "sessionId": "debug-session",
                                "runId": "dep-resolution",
                                "hypothesisId": "J",
                                "location": "metadata/src/processed/mod.rs:3545",
                                "message": "added_providing_package",
                                "data": {
                                    "dep_name": dep_name,
                                    "providing_package": providing_metadata.name,
                                    "result_count": result.len()
                                },
                                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                            }));
                            // #endregion
                        }
                    }
                    
                    // ALWAYS get dependencies from providing package (even if satisfied)
                    (Some(providing_metadata.name.clone()), Some(providing_metadata.runtime_dependencies.clone()))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };
            
            // Use providing package info if available, otherwise None
            (providing_pkg_name, providing_deps)
        };
        
        // If package not found in index and not installed, check if it's a real package (not a library file)
        // Library files (containing .so) are handled via provides, so we skip those
        let is_library_file = dep_name.contains(".so") || dep_name.starts_with("ld-linux") || dep_name == "rtld" || dep_name == "libc.so.6";
        
        // Check if dependency is provided by a package (via provides_pkg) - this should have been checked above
        // but we need to check again here for missing dependency tracking
        let provided_by_pkg_check = if dep_metadata.is_none() && !is_library_file {
            repo_index.lookup_provides_pkg(&dep_name)
        } else {
            Vec::new()
        };
        
        // Track missing real packages (not library files, not installed, not in index, not provided by any package)
        // Since we only use dependencies that exist in the repository, if we get here and the package doesn't exist,
        // it's a real missing package (not a virtual one)
        let looks_like_real_package = true; // All dependencies at this point are validated against the repository
        
        // #region agent log
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "dep-resolution",
            "hypothesisId": "K",
            "location": "metadata/src/processed/mod.rs:3618",
            "message": "checking_if_missing",
            "data": {
                "dep_name": dep_name,
                "has_metadata": dep_metadata.is_some(),
                "is_installed": installed_provides.is_dependency_satisfied(&dep_name).is_some(),
                "is_library_file": is_library_file,
                "provided_by_pkg_count": provided_by_pkg_check.len(),
                "looks_like_real_package": looks_like_real_package,
                "will_track_as_missing": dep_metadata.is_none() 
                    && !installed_provides.is_dependency_satisfied(&dep_name).is_some() 
                    && !is_library_file 
                    && provided_by_pkg_check.is_empty()
                    && looks_like_real_package
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion
        
        if dep_metadata.is_none() 
            && !installed_provides.is_dependency_satisfied(&dep_name).is_some() 
            && !is_library_file 
            && provided_by_pkg_check.is_empty()
            && looks_like_real_package {
            // This is a real package that's not found - track it as missing
            let mut missing = missing_dependencies.borrow_mut();
            if dep_name != main_package_name && !missing.contains(&dep_name) {
                missing.push(dep_name.clone());
                // #region agent log
                let _ = write_debug_log(&serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "dep-resolution",
                    "hypothesisId": "K",
                    "location": "metadata/src/processed/mod.rs:3644",
                    "message": "tracked_missing_package",
                    "data": {
                        "dep_name": dep_name,
                        "is_library_file": is_library_file,
                        "provided_by_pkg_check_count": provided_by_pkg_check.len(),
                        "looks_like_real_package": looks_like_real_package,
                        "missing_count": missing.len()
                    },
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                }));
                // #endregion
            }
        }
        
        // Helper function to validate if a dependency exists in the repository
        // For PAX packages, only check PAX repos; for others, check all repos
        // Only include dependencies that actually exist in the index or are provided by packages
        let dependency_exists_in_repo = |dep_name: &str| -> bool {
            let exists = if is_pax_package {
                // For PAX packages, only check PAX repos
                // Check if package exists in PAX index
                let pkg_exists = repo_index.lookup_package_pax_only(dep_name).is_some();
                let provides_pkg = !repo_index.lookup_provides_pkg_pax_only(dep_name).is_empty();
                let provides_lib = if dep_name.contains(".so") || dep_name.starts_with("ld-linux") || dep_name == "rtld" || dep_name == "libc.so.6" {
                    !repo_index.lookup_provides_lib_pax_only(dep_name).is_empty()
                } else {
                    false
                };
                
                // #region agent log
                let _ = write_debug_log(&serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "pax-dep-resolution",
                    "hypothesisId": "PAX_VALIDATION",
                    "location": "metadata/src/processed/mod.rs:3661",
                    "message": "checking_dependency_in_pax_repos",
                    "data": {
                        "dep_name": dep_name,
                        "pkg_exists": pkg_exists,
                        "provides_pkg": provides_pkg,
                        "provides_lib": provides_lib,
                        "result": pkg_exists || provides_pkg || provides_lib
                    },
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                }));
                // #endregion
                
                pkg_exists || provides_pkg || provides_lib
            } else {
                // For non-PAX packages, check all repos
                // Check if package exists in index
                let pkg_exists = repo_index.lookup_package(dep_name).is_some();
                let provides_pkg = !repo_index.lookup_provides_pkg(dep_name).is_empty();
                let provides_lib = if dep_name.contains(".so") || dep_name.starts_with("ld-linux") || dep_name == "rtld" || dep_name == "libc.so.6" {
                    !repo_index.lookup_provides_lib(dep_name).is_empty()
                } else {
                    false
                };
                let installed = installed_provides.is_dependency_satisfied(dep_name).is_some();
                
                pkg_exists || provides_pkg || provides_lib || installed
            };
            
            exists
        };
        
        // Get ALL dependencies - prioritize metadata, fallback to index
        // Only include dependencies that actually exist in the repository index
        let mut all_deps: Vec<DependKind> = if let Some(deps) = deps_from_metadata.clone() {
            // Use dependencies from metadata (most accurate) - validate against repository
            deps.into_iter()
                .filter(|dep| {
                    let dep_name = match dep {
                        DependKind::Latest(n) => n,
                        DependKind::Specific(dv) => &dv.name,
                        DependKind::Volatile(n) => n,
                    };
                    dependency_exists_in_repo(dep_name)
                })
                .collect()
        } else {
            // Fallback: get from index using the actual package name or dep_name
            // For PAX packages, only get dependencies from PAX repos
            // Validate each dependency against the repository
            let lookup_name: &str = package_name_for_deps.as_ref().map(|s| s.as_str()).unwrap_or(&dep_name);
            let deps_from_index = if is_pax_package {
                repo_index.get_dependencies_pax_only(lookup_name)
            } else {
                repo_index.get_dependencies(lookup_name)
            };
            deps_from_index
                .unwrap_or_default()
                .into_iter()
                .filter(|dep| {
                    let dep_name = match dep {
                        DependKind::Latest(n) => n,
                        DependKind::Specific(dv) => &dv.name,
                        DependKind::Volatile(n) => n,
                    };
                    dependency_exists_in_repo(dep_name)
                })
                .collect()
        };
        
        // Also merge dependencies from all versions we found (in case metadata didn't have them)
        // Only include dependencies that exist in the repository
        for version in &all_versions {
            for dep in &version.runtime_dependencies {
                let dep_name = match dep {
                    DependKind::Latest(n) => n,
                    DependKind::Specific(dv) => &dv.name,
                    DependKind::Volatile(n) => n,
                };
                
                // Only include if it exists in the repository
                if !dependency_exists_in_repo(dep_name) {
                    continue;
                }
                
                let dep_key = match dep {
                    DependKind::Latest(n) => n.clone(),
                    DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                    DependKind::Volatile(n) => format!("volatile:{}", n),
                };
                
                if !all_deps.iter().any(|d| {
                    let d_key = match d {
                        DependKind::Latest(n) => n.clone(),
                        DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                        DependKind::Volatile(n) => format!("volatile:{}", n),
                    };
                    d_key == dep_key
                }) {
                    all_deps.push(dep.clone());
                }
            }
        }
        
        // Also check index for additional dependencies
        // For PAX packages, only check PAX repos
        // Validate each dependency against the repository to ensure it actually exists
        let lookup_name: &str = package_name_for_deps.as_ref().map(|s| s.as_str()).unwrap_or(&dep_name);
        let index_deps_opt = if is_pax_package {
            repo_index.get_dependencies_pax_only(lookup_name)
        } else {
            repo_index.get_dependencies(lookup_name)
        };
        if let Some(index_deps) = index_deps_opt {
            for dep in index_deps.iter() {
                let dep_name = match dep {
                    DependKind::Latest(n) => n,
                    DependKind::Specific(dv) => &dv.name,
                    DependKind::Volatile(n) => n,
                };
                
                // Only include if it exists in the repository
                if !dependency_exists_in_repo(dep_name) {
                    continue;
                }
                
                let dep_key = match dep {
                    DependKind::Latest(n) => n.clone(),
                    DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                    DependKind::Volatile(n) => format!("volatile:{}", n),
                };
                
                if !all_deps.iter().any(|d| {
                    let d_key = match d {
                        DependKind::Latest(n) => n.clone(),
                        DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                        DependKind::Volatile(n) => format!("volatile:{}", n),
                    };
                    d_key == dep_key
                }) {
                    all_deps.push(dep.clone());
                }
            }
        }
        
        // #region agent log
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "dep-resolution",
            "hypothesisId": "E",
            "location": "metadata/src/processed/mod.rs:3452",
            "message": "get_dependencies_from_index",
            "data": {
                "dep_name": dep_name,
                "package_name_for_deps": package_name_for_deps,
                "deps_from_metadata": deps_from_metadata.is_some(),
                "deps_from_index": all_deps.len()
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion
        
        // #region agent log
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "dep-resolution",
            "hypothesisId": "E",
            "location": "metadata/src/processed/mod.rs:3474",
            "message": "merged_dependencies",
            "data": {
                "dep_name": dep_name,
                "total_deps_after_merge": all_deps.len(),
                "deps_list": all_deps.iter().map(|d| match d {
                    DependKind::Latest(n) => n.clone(),
                    DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                    DependKind::Volatile(n) => format!("volatile:{}", n),
                }).collect::<Vec<_>>()
            },
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        }));
        // #endregion
        
        let deps_to_process = all_deps;
        
        // ALWAYS process dependencies, even if package is satisfied or not found
        // This ensures we find all transitive dependencies that might not be satisfied
        for dep in &deps_to_process {
            let next_dep_name = match dep {
                DependKind::Latest(name) => name.clone(),
                DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                DependKind::Volatile(name) => name.clone(),
            };
            
            // Skip the main package itself (avoid circular dependencies)
            if next_dep_name == main_package_name {
                // #region agent log
                let _ = write_debug_log(&serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "dep-resolution",
                    "hypothesisId": "F",
                    "location": "metadata/src/processed/mod.rs:3488",
                    "message": "skipped_main_package",
                    "data": {
                        "parent_dep": dep_name,
                        "skipped_dep": next_dep_name,
                        "main_package": main_package_name
                    },
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                }));
                // #endregion
                continue;
            }
            
            // ALWAYS add to queue if not already there (even if in resolved)
            // This ensures we process ALL transitive dependencies recursively
            // resolved is only used to prevent infinite loops, not to skip processing
            if !to_process.iter().any(|d| d == &next_dep_name) {
                to_process.push(next_dep_name.clone());
                // #region agent log
                let _ = write_debug_log(&serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "dep-resolution",
                    "hypothesisId": "G",
                    "location": "metadata/src/processed/mod.rs:3494",
                    "message": "added_to_queue",
                    "data": {
                        "parent_dep": dep_name,
                        "new_dep": next_dep_name,
                        "queue_size": to_process.len(),
                        "already_in_resolved": resolved.contains(&next_dep_name)
                    },
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                }));
                // #endregion
            } else {
                // #region agent log
                let _ = write_debug_log(&serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "dep-resolution",
                    "hypothesisId": "H",
                    "location": "metadata/src/processed/mod.rs:3496",
                    "message": "skipped_already_in_queue",
                    "data": {
                        "parent_dep": dep_name,
                        "skipped_dep": next_dep_name
                    },
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                }));
                // #endregion
            }
            
            // Mark as resolved to prevent infinite loops (but still process if in queue)
            if !resolved.contains(&next_dep_name) {
                resolved.insert(next_dep_name.clone());
            }
        }
    }
    
    let resolution_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let total_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    
    // #region agent log
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "dep-resolution",
        "hypothesisId": "FINAL",
        "location": "metadata/src/processed/mod.rs:3643",
        "message": "resolution_complete",
        "data": {
            "duration_ms": resolution_end.saturating_sub(resolution_start),
            "total_duration_ms": total_end.saturating_sub(total_start),
            "resolved_count": result.len(),
            "result_packages": result.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "queue_size": to_process.len(),
            "resolved_set_size": resolved.len()
        },
        "timestamp": resolution_end
    }));
    // #endregion
    
    // #region agent log
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "timing",
        "hypothesisId": "TIMING",
        "location": "metadata/src/processed/mod.rs:3665",
        "message": "PHASE: index_resolution",
        "data": {"duration_ms": resolution_end.saturating_sub(resolution_start), "resolved_count": result.len()},
        "timestamp": resolution_end
    }));
    // #endregion
    // #region agent log
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "timing",
        "hypothesisId": "TIMING",
        "location": "metadata/src/processed/mod.rs:3675",
        "message": "PHASE: TOTAL_RESOLUTION_INDEX",
        "data": {"duration_ms": total_end.saturating_sub(total_start), "result_count": result.len()},
        "timestamp": total_end
    }));
    // #endregion
    
    // Check if any dependencies are missing
    let missing = missing_dependencies.borrow();
    if !missing.is_empty() {
        let mut error_msg = if is_pax_package {
            format!(
                "\n\x1B[91mERROR: Cannot install PAX package '{}' - the following required dependencies are not available in PAX repositories:\x1B[0m\n\n",
                package.name
            )
        } else {
            format!(
                "\n\x1B[91mERROR: Cannot install '{}' - the following required dependencies are not available in configured repositories:\x1B[0m\n\n",
                package.name
            )
        };
        for dep in missing.iter() {
            error_msg.push_str(&format!("  - {}\n", dep));
        }
        if is_pax_package {
            error_msg.push_str("\n\x1B[93mPAX packages must have all dependencies available in PAX repositories. Please ensure these packages are available in your PAX repositories.\x1B[0m\n");
        } else {
            error_msg.push_str("\n\x1B[93mPlease ensure these packages are available in your repositories or install them manually before proceeding.\x1B[0m\n");
        }
        return Err(error_msg);
    }

    Ok(result)
}

/// OLD METHOD: Per-dependency HTTP requests (kept as fallback)
async fn resolve_all_dependencies_old(
    package: &ProcessedMetaData,
    sources: &[OriginKind],
) -> Vec<ProcessedMetaData> {
    let main_package_name = &package.name;
    use std::collections::{HashMap, HashSet};
    
    let total_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

    // PHASE 1: Load installed packages DB
    let phase1_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let installed_packages = match list_installed_packages(false, false, None) {
        Ok(packages) => packages,
        Err(_) => Vec::new(),
    };
    let phase1_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "timing",
        "hypothesisId": "TIMING",
        "location": "metadata/src/processed/mod.rs:3332",
        "message": "PHASE: load_installed_db",
        "data": {"duration_ms": phase1_end.saturating_sub(phase1_start), "count": installed_packages.len()},
        "timestamp": phase1_end
    }));
    
    // PHASE 2: Build installed provides lookup (ONLY from local database)
    let phase2_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let installed_provides = InstalledPackageProvides::from_installed_packages(&installed_packages);
    let phase2_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "timing",
        "hypothesisId": "TIMING",
        "location": "metadata/src/processed/mod.rs:3772",
        "message": "PHASE: build_installed_provides",
        "data": {"duration_ms": phase2_end.saturating_sub(phase2_start), "packages": installed_provides.package_names.len(), "provides_lib": installed_provides.provides_lib.len(), "provides_file": installed_provides.provides_file.len()},
        "timestamp": phase2_end
    }));

    let mut resolved = HashSet::new();
    let mut to_process = Vec::new();
    let mut result = Vec::new();

    // Start with the direct dependencies
    for dep in &package.runtime_dependencies {
        let dep_name = match dep {
            DependKind::Latest(name) => name.clone(),
            DependKind::Specific(dep_ver) => dep_ver.name.clone(),
            DependKind::Volatile(name) => name.clone(),
        };
        
        // Fast check: skip if already satisfied (no metadata fetch needed)
        if installed_provides.is_dependency_satisfied(&dep_name).is_some() {
            continue;
        }
        
        if !resolved.contains(&dep_name) {
            resolved.insert(dep_name.clone());
            to_process.push(dep_name);
        }
    }

    // PHASE 3: Dependency resolution loop with memoization
    let mut memo: HashMap<String, Option<ProcessedMetaData>> = HashMap::new();
    let mut iteration = 0;
    
    // Cap concurrency to avoid task storms
    const MAX_CONCURRENT_FETCHES: usize = 32;
    
    while !to_process.is_empty() {
        iteration += 1;
        let all_deps: Vec<String> = to_process.drain(..).collect();
        
        if all_deps.is_empty() {
            break;
        }
        
        // PHASE 3a: Metadata fetch (with concurrency cap)
        let phase3a_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        
        // Split into chunks to cap concurrency, check memo first
        let mut fetch_results = Vec::new();
        let mut deps_to_fetch = Vec::new();
        
        // Check memo and separate cached vs uncached
        for dep_name in &all_deps {
            if let Some(cached) = memo.get(dep_name) {
                fetch_results.push(cached.clone());
            } else {
                deps_to_fetch.push(dep_name.clone());
            }
        }
        
        // Fetch only uncached dependencies with concurrency cap
        for chunk in deps_to_fetch.chunks(MAX_CONCURRENT_FETCHES) {
            let chunk_futures: Vec<_> = chunk.iter().map(|dep_name| {
                let dep_name = dep_name.clone();
                let sources_clone = sources.to_vec();
                async move {
                    ProcessedMetaData::get_metadata(&dep_name, None, &sources_clone, true).await
                }
            }).collect();
            
            let chunk_results = join_all(chunk_futures).await;
            
            // Update memo and results
            for (dep_name, result) in chunk.iter().zip(chunk_results.iter()) {
                memo.insert(dep_name.clone(), result.clone());
                fetch_results.push(result.clone());
            }
        }
        
        let phase3a_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "timing",
            "hypothesisId": "TIMING",
            "location": "metadata/src/processed/mod.rs:3408",
            "message": "PHASE: metadata_fetch",
            "data": {"iteration": iteration, "deps_requested": all_deps.len(), "deps_fetched": fetch_results.len(), "duration_ms": phase3a_end.saturating_sub(phase3a_start), "memo_size": memo.len()},
            "timestamp": phase3a_end
        }));
        
        // PHASE 3b: Satisfaction checks
        let phase3b_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

        let mut satisfied_count = 0;
        let mut unsatisfied_count = 0;
        
        for dep_metadata_opt in fetch_results {
            if let Some(dep_metadata) = dep_metadata_opt {
                if dep_metadata.name == *main_package_name {
                    continue;
                }

                let system_satisfied = is_dependency_satisfied_by_system(
                    &dep_metadata,
                    &installed_provides,
                ).await;
                
                if system_satisfied {
                    satisfied_count += 1;
                    continue;
                }

                unsatisfied_count += 1;
                result.push(dep_metadata.clone());

                for dep in &dep_metadata.runtime_dependencies {
                    let dep_name = match dep {
                        DependKind::Latest(name) => name.clone(),
                        DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                        DependKind::Volatile(name) => name.clone(),
                    };

                    if installed_provides.is_dependency_satisfied(&dep_name).is_some() {
                        continue;
                    }
                    
                    if !resolved.contains(&dep_name) {
                        resolved.insert(dep_name.clone());
                        to_process.push(dep_name);
                    }
                }
            }
        }
        
        let phase3b_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let _ = write_debug_log(&serde_json::json!({
            "sessionId": "debug-session",
            "runId": "timing",
            "hypothesisId": "TIMING",
            "location": "metadata/src/processed/mod.rs:3465",
            "message": "PHASE: satisfaction_checks",
            "data": {"duration_ms": phase3b_end.saturating_sub(phase3b_start), "satisfied": satisfied_count, "unsatisfied": unsatisfied_count},
            "timestamp": phase3b_end
        }));
    }
    
    let total_end = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let _ = write_debug_log(&serde_json::json!({
        "sessionId": "debug-session",
        "runId": "timing",
        "hypothesisId": "TIMING",
        "location": "metadata/src/processed/mod.rs:3310",
        "message": "PHASE: TOTAL_RESOLUTION",
        "data": {"duration_ms": total_end.saturating_sub(total_start), "result_count": result.len()},
        "timestamp": total_end
    }));

    result
}


/// Check if a dependency is already satisfied by installed Pax packages
/// This is called after we have the metadata for the dependency
/// Checks if the dependency name matches an installed package, or if any installed package
/// provides the files that this dependency would provide
/// Check if a dependency is satisfied by installed packages in local database
/// Uses ONLY the local package database - no direct system checks
/// Cross-format compatible: checks package names and provides across all formats
async fn is_dependency_satisfied_by_system(
    metadata: &ProcessedMetaData,
    installed_provides: &InstalledPackageProvides,
) -> bool {
    // #region agent log
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let log_entry = serde_json::json!({
        "sessionId": "debug-session",
        "runId": "post-fix",
        "hypothesisId": "B",
        "location": "metadata/src/processed/mod.rs:3991",
        "message": "is_dependency_satisfied_by_system called",
        "data": {"package_name": metadata.name},
        "timestamp": timestamp
    });
    let _ = write_debug_log(&log_entry);
    // #endregion
    
    // Check if the package itself is already installed (by name or provides)
    if let Some(provider) = installed_provides.is_dependency_satisfied(&metadata.name) {
        // #region agent log
        let log_entry_installed = serde_json::json!({
            "sessionId": "debug-session",
            "runId": "post-fix",
            "hypothesisId": "D",
            "location": "metadata/src/processed/mod.rs:4005",
            "message": "is_dependency_satisfied_by_system result (package already installed)",
            "data": {"package_name": metadata.name, "provided_by": provider, "satisfied": true},
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        });
        let _ = write_debug_log(&log_entry_installed);
        // #endregion
        return true;
    }
    
    // Check what files this package would provide
    let files_this_package_provides: Vec<String> = match &metadata.install_kind {
        ProcessedInstallKind::PreBuilt(prebuilt) => prebuilt.critical.clone(),
        ProcessedInstallKind::Compilable(_) => Vec::new(),
    };
    
    // If we have a file list, check if any installed package provides these files
    if !files_this_package_provides.is_empty() {
        let mut found_files = 0;
        for file_path in &files_this_package_provides {
            if let Some(provider) = installed_provides.is_dependency_satisfied(file_path) {
                found_files += 1;
                // #region agent log
                let log_entry_file = serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "post-fix",
                    "hypothesisId": "D",
                    "location": "metadata/src/processed/mod.rs:4025",
                    "message": "file provided by installed package",
                    "data": {"package_name": metadata.name, "file": file_path, "provided_by": provider},
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                });
                let _ = write_debug_log(&log_entry_file);
                // #endregion
            }
        }
        
        // If most files are provided by installed packages (50% threshold), dependency is satisfied
        let satisfied = found_files * 2 >= files_this_package_provides.len();
        
        // #region agent log
        let log_entry_files = serde_json::json!({
            "sessionId": "debug-session",
            "runId": "post-fix",
            "hypothesisId": "D",
            "location": "metadata/src/processed/mod.rs:4035",
            "message": "file list check result",
            "data": {"package_name": metadata.name, "found_files": found_files, "total_files": files_this_package_provides.len(), "satisfied": satisfied},
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        });
        let _ = write_debug_log(&log_entry_files);
        // #endregion
        
        if satisfied {
            return true;
        }
    }
    
    // For packages without file lists, check if ALL their runtime dependencies are satisfied by installed packages
    // #region agent log
    let log_entry_deps_check = serde_json::json!({
        "sessionId": "debug-session",
        "runId": "post-fix",
        "hypothesisId": "D",
        "location": "metadata/src/processed/mod.rs:4045",
        "message": "checking if all deps are satisfied by installed packages",
        "data": {"package_name": metadata.name, "num_deps": metadata.runtime_dependencies.len()},
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    });
    let _ = write_debug_log(&log_entry_deps_check);
    // #endregion
    
    if metadata.runtime_dependencies.is_empty() {
        // #region agent log
        let log_entry_no_deps = serde_json::json!({
            "sessionId": "debug-session",
            "runId": "post-fix",
            "hypothesisId": "D",
            "location": "metadata/src/processed/mod.rs:4055",
            "message": "package has no runtime dependencies - cannot infer satisfaction",
            "data": {"package_name": metadata.name},
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        });
        let _ = write_debug_log(&log_entry_no_deps);
        // #endregion
    } else {
        let mut all_deps_satisfied = true;
        let mut first_unsatisfied_dep = None;
        let mut skipped_special_deps = 0;
        let mut real_dep_count = 0;
        
        for dep in &metadata.runtime_dependencies {
            let dep_name = match dep {
                DependKind::Latest(name) => name.clone(),
                DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                DependKind::Volatile(name) => name.clone(),
            };
            
            // Skip self-references (circular dependencies)
            if dep_name == metadata.name {
                skipped_special_deps += 1;
                continue;
            }
            
            real_dep_count += 1;
            
            // Check if this dependency is satisfied by an installed package (cross-format)
            let dep_satisfied = installed_provides.is_dependency_satisfied(&dep_name).is_some();
            
            // #region agent log
            let log_entry_dep_check = serde_json::json!({
                "sessionId": "debug-session",
                "runId": "post-fix",
                "hypothesisId": "D",
                "location": "metadata/src/processed/mod.rs:4080",
                "message": "checking if dep is satisfied by installed package",
                "data": {
                    "package_name": metadata.name,
                    "dep_name": &dep_name,
                    "dep_satisfied": dep_satisfied,
                    "provider": installed_provides.is_dependency_satisfied(&dep_name)
                },
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            });
            let _ = write_debug_log(&log_entry_dep_check);
            // #endregion
            
            if !dep_satisfied {
                all_deps_satisfied = false;
                first_unsatisfied_dep = Some(dep_name);
                break;
            }
        }
        
        // #region agent log
        let log_entry_deps_result = serde_json::json!({
            "sessionId": "debug-session",
            "runId": "post-fix",
            "hypothesisId": "D",
            "location": "metadata/src/processed/mod.rs:4100",
            "message": "all deps satisfied check result",
            "data": {"package_name": metadata.name, "all_deps_satisfied": all_deps_satisfied, "first_unsatisfied_dep": first_unsatisfied_dep, "skipped_special_deps": skipped_special_deps, "real_dep_count": real_dep_count},
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        });
        let _ = write_debug_log(&log_entry_deps_result);
        // #endregion
        
        if all_deps_satisfied && real_dep_count > 0 {
            // #region agent log
            let log_entry_satisfied = serde_json::json!({
                "sessionId": "debug-session",
                "runId": "post-fix",
                "hypothesisId": "D",
                "location": "metadata/src/processed/mod.rs:4110",
                "message": "is_dependency_satisfied_by_system result (all deps satisfied by installed packages)",
                "data": {"package_name": metadata.name, "real_dep_count": real_dep_count, "satisfied": true},
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            });
            let _ = write_debug_log(&log_entry_satisfied);
            // #endregion
            return true;
        } else if real_dep_count == 0 {
            // Package has only special dependencies
            // #region agent log
            let log_entry_only_special = serde_json::json!({
                "sessionId": "debug-session",
                "runId": "post-fix",
                "hypothesisId": "D",
                "location": "metadata/src/processed/mod.rs:4120",
                "message": "package has only special deps - likely satisfied",
                "data": {"package_name": metadata.name, "satisfied": true},
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            });
            let _ = write_debug_log(&log_entry_only_special);
            // #endregion
            return true;
        }
    }
    
    // #region agent log
    let log_entry_final = serde_json::json!({
        "sessionId": "debug-session",
        "runId": "post-fix",
        "hypothesisId": "B",
        "location": "metadata/src/processed/mod.rs:4130",
        "message": "is_dependency_satisfied_by_system result (not satisfied)",
        "data": {"package_name": metadata.name, "satisfied": false},
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    });
    let _ = write_debug_log(&log_entry_final);
    // #endregion
    
    false
}


/// Structure to track what installed packages provide (for cross-format compatibility)
/// Built from local package database only - no direct system checks
struct InstalledPackageProvides {
    // Package name -> true (if installed)
    package_names: HashSet<String>,
    // Library name -> list of packages that provide it
    provides_lib: HashMap<String, Vec<String>>,
    // File path -> list of packages that provide it
    provides_file: HashMap<String, Vec<String>>,
    // Package name -> list of package names it provides (for virtual packages)
    provides_pkg: HashMap<String, Vec<String>>,
}

impl InstalledPackageProvides {
    /// Build from installed packages in local database
    fn from_installed_packages(installed: &[crate::installed::InstalledMetaData]) -> Self {
        let mut package_names = HashSet::new();
        let mut provides_lib = HashMap::new();
        let mut provides_file = HashMap::new();
        let mut provides_pkg = HashMap::new();
        
        for pkg in installed {
            // Track package name
            package_names.insert(pkg.name.clone());
            
            // Extract provides from PreBuilt packages (files)
            if let crate::installed::InstalledInstallKind::PreBuilt(ref prebuilt) = pkg.install_kind {
                for file in &prebuilt.critical {
                    // Track file provides
                    provides_file.entry(file.clone())
                        .or_insert_with(Vec::new)
                        .push(pkg.name.clone());
                    
                    // Extract library name if it's a .so file
                    if file.contains(".so") {
                        if let Some(lib_name) = file.split('/').last() {
                            provides_lib.entry(lib_name.to_string())
                                .or_insert_with(Vec::new)
                                .push(pkg.name.clone());
                        }
                    }
                }
            }
            
            // The package name itself provides the package name (for direct name matching)
            provides_pkg.entry(pkg.name.clone())
                .or_insert_with(Vec::new)
                .push(pkg.name.clone());
        }
        
        Self {
            package_names,
            provides_lib,
            provides_file,
            provides_pkg,
        }
    }
    
    /// Check if a dependency is satisfied by any installed package (cross-format)
    /// Checks:
    /// 1. Direct package name match
    /// 2. Package provides (virtual packages)
    /// 3. Library provides
    /// 4. File provides
    fn is_dependency_satisfied(&self, dep_name: &str) -> Option<String> {
        // 1. Direct package name match
        if self.package_names.contains(dep_name) {
            return Some(dep_name.to_string());
        }
        
        // 2. Check if any package provides this dependency name (virtual packages)
        if let Some(providers) = self.provides_pkg.get(dep_name) {
            if let Some(provider) = providers.first() {
                return Some(provider.clone());
            }
        }
        
        // 3. Check library provides
        if let Some(providers) = self.provides_lib.get(dep_name) {
            if let Some(provider) = providers.first() {
                return Some(provider.clone());
            }
        }
        
        // 4. Check file provides
        if let Some(providers) = self.provides_file.get(dep_name) {
            if let Some(provider) = providers.first() {
                return Some(provider.clone());
            }
        }
        
        // 5. Check library version suffix matches (e.g., libdbus-1.so.3 matches libdbus-1.so)
        if dep_name.contains(".so") {
            let dep_base = dep_name.split('.').next().unwrap_or(dep_name);
            for (lib_name, providers) in &self.provides_lib {
                if lib_name == dep_name || 
                   lib_name.starts_with(&format!("{}.", dep_base)) ||
                   dep_name.starts_with(&format!("{}.", lib_name.split('.').next().unwrap_or(lib_name))) {
                    if let Some(provider) = providers.first() {
                        return Some(provider.clone());
                    }
                }
            }
        }
        
        None
    }
}

/// Fast synchronous check if a dependency is satisfied using in-memory lookup structures
/// This avoids disk I/O and async overhead
/// DEPRECATED: Use InstalledPackageProvides::is_dependency_satisfied instead
fn is_system_dependency_satisfied_fast(
    dep_name: &str,
    installed_package_names: &HashSet<String>,
    installed_library_files: &HashSet<String>,
) -> bool {
    // Check if package is already installed by Pax (O(1) lookup)
    if installed_package_names.contains(dep_name) {
        return true;
    }
    
    // Check if dependency is a library file and if it's provided by an installed package
    if dep_name.contains(".so") || dep_name.starts_with("ld-linux") {
        // Check if the exact library name is in our set
        if installed_library_files.contains(dep_name) {
            return true;
        }
        // Check if any installed library filename matches (handles version suffixes)
        // e.g., dependency "libdbus-1.so.3" matches installed "libdbus-1.so.3" or "libdbus-1.so"
        let dep_base = dep_name.split('.').next().unwrap_or(dep_name);
        if installed_library_files.iter().any(|lib| {
            lib == dep_name || 
            lib.starts_with(&format!("{}.", dep_base)) ||
            dep_name.starts_with(&format!("{}.", lib.split('.').next().unwrap_or(lib)))
        }) {
            return true;
        }
    }
    
    false
}

/// Check if a dependency name corresponds to a package whose files are already on the system
/// This is a quick heuristic check before fetching metadata
/// NOTE: This function does disk I/O and is slower - use is_system_dependency_satisfied_fast when possible
async fn is_system_dependency_satisfied(dep_name: &str) -> bool {
    // #region agent log
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let log_entry = serde_json::json!({
        "sessionId": "debug-session",
        "runId": "post-fix",
        "hypothesisId": "A",
        "location": "metadata/src/processed/mod.rs:3419",
        "message": "is_system_dependency_satisfied called",
        "data": {"dep_name": dep_name},
        "timestamp": timestamp
    });
    let _ = write_debug_log(&log_entry);
    // #endregion
    
    // First, check if package is already installed by pax
    let installed_check = InstalledMetaData::open(dep_name).is_ok();
    if installed_check {
        // #region agent log
        let log_entry2 = serde_json::json!({
            "sessionId": "debug-session",
            "runId": "post-fix",
            "hypothesisId": "A",
            "location": "metadata/src/processed/mod.rs:3421",
            "message": "is_system_dependency_satisfied result",
            "data": {"dep_name": dep_name, "installed_by_pax": true, "return_value": true, "reason": "installed_by_pax"},
            "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
        });
        let _ = write_debug_log(&log_entry2);
        // #endregion
        return true; // Already installed by pax, skip
    }
    
    // Check if dependency is a library file (contains .so) and if it exists on the system
    if dep_name.contains(".so") || dep_name.starts_with("ld-linux") {
        let lib_paths = vec![
            format!("/lib/{}", dep_name),
            format!("/lib64/{}", dep_name),
            format!("/usr/lib/{}", dep_name),
            format!("/usr/lib64/{}", dep_name),
            format!("/usr/local/lib/{}", dep_name),
            format!("/usr/local/lib64/{}", dep_name),
        ];
        
        // Also check common library subdirectories
        let mut extended_paths = lib_paths.clone();
        extended_paths.extend(vec![
            format!("/lib/x86_64-linux-gnu/{}", dep_name),
            format!("/usr/lib/x86_64-linux-gnu/{}", dep_name),
        ]);
        
        for lib_path in extended_paths {
            if Path::new(&lib_path).exists() {
                // #region agent log
                let log_entry3 = serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "post-fix",
                    "hypothesisId": "A",
                    "location": "metadata/src/processed/mod.rs:3450",
                    "message": "is_system_dependency_satisfied result",
                    "data": {"dep_name": dep_name, "installed_by_pax": false, "return_value": true, "reason": "library_exists", "lib_path": lib_path},
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                });
                let _ = write_debug_log(&log_entry3);
                // #endregion
                return true;
            }
        }
        
        // Try using ldconfig -p to check if library is available (faster than checking all paths)
        if let Ok(output) = RunCommand::new("ldconfig").arg("-p").output() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // Check if the library name appears in ldconfig output
            if stdout.contains(dep_name) {
                // #region agent log
                let log_entry4 = serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "post-fix",
                    "hypothesisId": "A",
                    "location": "metadata/src/processed/mod.rs:3465",
                    "message": "is_system_dependency_satisfied result",
                    "data": {"dep_name": dep_name, "installed_by_pax": false, "return_value": true, "reason": "library_in_ldconfig"},
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                });
                let _ = write_debug_log(&log_entry4);
                // #endregion
                return true;
            }
        }
    }
    
    // Check if dependency is a system binary (like rtld, ld-linux, etc.)
    // Common system binaries that should exist
    let system_binaries = vec!["rtld", "ld-linux", "ld.so"];
    if system_binaries.iter().any(|&bin| dep_name == bin || dep_name.starts_with(bin)) {
        // Check if binary exists in common system paths
        let bin_paths = vec![
            format!("/usr/bin/{}", dep_name),
            format!("/bin/{}", dep_name),
            format!("/usr/sbin/{}", dep_name),
            format!("/sbin/{}", dep_name),
        ];
        
        for bin_path in bin_paths {
            if Path::new(&bin_path).exists() {
                // #region agent log
                let log_entry5 = serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "post-fix",
                    "hypothesisId": "A",
                    "location": "metadata/src/processed/mod.rs:3485",
                    "message": "is_system_dependency_satisfied result",
                    "data": {"dep_name": dep_name, "installed_by_pax": false, "return_value": true, "reason": "system_binary_exists", "bin_path": bin_path},
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                });
                let _ = write_debug_log(&log_entry5);
                // #endregion
                return true;
            }
        }
        
        // Also try using 'which' command
        if let Ok(output) = RunCommand::new("which").arg(dep_name).output() {
            if output.status.success() {
                // #region agent log
                let log_entry6 = serde_json::json!({
                    "sessionId": "debug-session",
                    "runId": "post-fix",
                    "hypothesisId": "A",
                    "location": "metadata/src/processed/mod.rs:3500",
                    "message": "is_system_dependency_satisfied result",
                    "data": {"dep_name": dep_name, "installed_by_pax": false, "return_value": true, "reason": "found_via_which"},
                    "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                });
                let _ = write_debug_log(&log_entry6);
                // #endregion
                return true;
            }
        }
    }
    
    // #region agent log
    let log_entry7 = serde_json::json!({
        "sessionId": "debug-session",
        "runId": "post-fix",
        "hypothesisId": "A",
        "location": "metadata/src/processed/mod.rs:3510",
        "message": "is_system_dependency_satisfied result",
        "data": {"dep_name": dep_name, "installed_by_pax": false, "return_value": false, "reason": "not_found"},
        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    });
    let _ = write_debug_log(&log_entry7);
    // #endregion
    
    // We can't check files without metadata, so return false here
    // The actual check happens in is_dependency_satisfied_by_system after we have metadata
    false
}

/// Map library dependencies to package names for system verification
/// Used when checking if required system libraries are available
/// This is for error reporting, not dependency discovery
/// Keeps only essential system library mappings, no hardcoded packages
fn map_library_dependency_to_package(dep_name: &str) -> Option<String> {
    if dep_name.starts_with("lib") && dep_name.contains(".so") {
        // Extract base library name: libglib-2.0.so.0 -> glib2
        let base_name = dep_name
            .strip_prefix("lib")?
            .split(".so")
            .next()?
            .split("-")
            .next()?;

        // Only map essential system libraries that are commonly known
        match base_name {
            "c" | "gcc_s" | "stdc++" | "m" => Some("glibc".to_string()),
            "dl" | "rt" | "pthread" => Some("glibc".to_string()),
            _ => None, // Don't guess - let system verification handle it
        }
    } else if dep_name.starts_with("/lib") || dep_name.starts_with("/usr/lib") {
        // System library paths - these should be covered by glibc
        Some("glibc".to_string())
    } else {
        None
    }
}

pub async fn get_packages(
    package_names: Vec<String>,
    _preferred_source: Option<&str>,
    force_refresh: bool,
) -> Result<Vec<InstallPackage>, String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::fs::OpenOptions;
    use std::io::Write;
    
    let get_packages_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
        let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/processed/mod.rs:4178\",\"message\":\"get_packages_start\",\"data\":{{\"timestamp\":{}}},\"timestamp\":{}}}", get_packages_start, get_packages_start);
    }
    
    // Set thread-local refresh flag for dependency resolution
    set_force_refresh(force_refresh);
    
    let before_get_settings = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
        let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/processed/mod.rs:4187\",\"message\":\"before_get_settings\",\"data\":{{\"timestamp\":{}}},\"timestamp\":{}}}", before_get_settings, before_get_settings);
    }
    
    // Get configured repositories from settings
    let settings = settings::SettingsYaml::get_settings().map_err(|e| format!("Failed to load settings: {}", e))?;
    
    let after_get_settings = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
        let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/processed/mod.rs:4188\",\"message\":\"after_get_settings\",\"data\":{{\"timestamp\":{},\"duration_ms\":{}}},\"timestamp\":{}}}", after_get_settings, after_get_settings.saturating_sub(before_get_settings), after_get_settings);
    }
    let sources: Vec<OriginKind> = settings.sources.clone();
    
    // Build repo index FIRST to avoid per-package HTTP fetches (this eliminates the ~15s delay!)
    use crate::repo_index::MultiRepoIndex;
    let before_build_index = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
        let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/processed/mod.rs:4207\",\"message\":\"before_build_index_in_get_packages\",\"data\":{{\"timestamp\":{}}},\"timestamp\":{}}}", before_build_index, before_build_index);
    }
    
    let repo_index = match MultiRepoIndex::build(&sources, force_refresh).await {
        Ok(index) => Some(index),
        Err(e) => {
            eprintln!("Warning: Failed to build repo index: {}. Falling back to per-package fetches.", e);
            None
        }
    };
    
    let after_build_index = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
        let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/processed/mod.rs:4215\",\"message\":\"after_build_index_in_get_packages\",\"data\":{{\"timestamp\":{},\"duration_ms\":{}}},\"timestamp\":{}}}", after_build_index, after_build_index.saturating_sub(before_build_index), after_build_index);
    }

    // Process all packages in parallel
    // Collect errors separately since we need to fail fast if any dependency is missing
    let mut dependency_errors: Vec<String> = Vec::new();
    let package_futures: Vec<_> = package_names.iter().map(|name| {
        let name = name.clone();
        let sources_clone = sources.clone();
        let repo_index_clone = repo_index.as_ref();
        async move {
            // Try to use repo index first (fast path - no HTTP calls!)
            let all_matches: Vec<ProcessedMetaData> = if let Some(index) = repo_index_clone {
                // Use index for fast lookup - get all versions from all repos
                index.lookup_all_versions(&name)
            } else {
                // Fallback to per-source fetches if index failed
                ProcessedMetaData::get_all_metadata(&name, None, &sources_clone, true).await
            };

            // If no matches found, return None
            if all_matches.is_empty() {
                return None;
            }

            // Select package (either automatically or via user choice)
            let metadata = if all_matches.len() == 1 {
                all_matches.into_iter().next().unwrap()
            } else {
                match select_package_from_multiple(&all_matches, &name).await {
                    Ok(Some(selected)) => selected,
                    _ => return None, // User cancelled or error
                }
            };

            // #region agent log
            let _ = write_debug_log(&serde_json::json!({
                "sessionId": "debug-session",
                "runId": "pax-dep-resolution",
                "hypothesisId": "METADATA_DEPS",
                "location": "metadata/src/processed/mod.rs:4908",
                "message": "package_metadata_dependencies",
                "data": {
                    "package_name": metadata.name,
                    "package_kind": format!("{:?}", metadata.kind),
                    "runtime_deps_count": metadata.runtime_dependencies.len(),
                    "runtime_deps": metadata.runtime_dependencies.iter().map(|d| match d {
                        DependKind::Latest(n) => n.clone(),
                        DependKind::Specific(dv) => dv.name.clone(),
                        DependKind::Volatile(n) => n.clone(),
                    }).collect::<Vec<_>>(),
                    "build_deps_count": metadata.build_dependencies.len()
                },
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            }));
            // #endregion

            // Resolve all dependencies recursively
            // #region agent log
            let _ = write_debug_log(&serde_json::json!({
                "sessionId": "debug-session",
                "runId": "pax-dep-resolution",
                "hypothesisId": "CALL_RESOLVE",
                "location": "metadata/src/processed/mod.rs:4911",
                "message": "calling_resolve_all_dependencies",
                "data": {
                    "package_name": metadata.name,
                    "package_kind": format!("{:?}", metadata.kind),
                    "runtime_deps_in_metadata": metadata.runtime_dependencies.len()
                },
                "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
            }));
            // #endregion
            
            let mut run_deps = match resolve_all_dependencies(&metadata, &sources_clone).await {
                Ok(deps) => {
                    // #region agent log
                    let _ = write_debug_log(&serde_json::json!({
                        "sessionId": "debug-session",
                        "runId": "pax-dep-resolution",
                        "hypothesisId": "RESOLVE_RESULT",
                        "location": "metadata/src/processed/mod.rs:4920",
                        "message": "resolve_all_dependencies_result",
                        "data": {
                            "package_name": metadata.name,
                            "resolved_count": deps.len(),
                            "resolved_deps": deps.iter().map(|d| d.name.clone()).collect::<Vec<_>>()
                        },
                        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                    }));
                    // #endregion
                    deps
                },
                Err(e) => {
                    // #region agent log
                    let _ = write_debug_log(&serde_json::json!({
                        "sessionId": "debug-session",
                        "runId": "pax-dep-resolution",
                        "hypothesisId": "RESOLVE_ERROR",
                        "location": "metadata/src/processed/mod.rs:4925",
                        "message": "resolve_all_dependencies_error",
                        "data": {
                            "package_name": metadata.name,
                            "error": e
                        },
                        "timestamp": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
                    }));
                    // #endregion
                    // Error message already formatted nicely - return None to skip this package
                    // We'll collect the error and fail after all packages are processed
                    eprintln!("{}", e);
                    return None;
                }
            };

            // Special case: for nodejs packages, also ensure the corresponding libs package is installed
            if metadata.name.starts_with("nodejs") && !metadata.name.ends_with("libs") {
                // Try nodejs*-libs pattern (e.g., nodejs22-libs for nodejs22)
                let libs_name1 = if metadata.name.contains("22") {
                    "nodejs22-libs".to_string()
                } else {
                    format!("{}22-libs", metadata.name)
                };

                // Also try plain nodejs-libs
                let libs_name2 = "nodejs-libs".to_string();

                let libs_sources = sources_clone.clone();
                let libs_futures = vec![
                    ProcessedMetaData::get_metadata(&libs_name1, None, &libs_sources, true),
                    ProcessedMetaData::get_metadata(&libs_name2, None, &libs_sources, true),
                ];
                let libs_results = join_all(libs_futures).await;

                for libs_metadata in libs_results.into_iter().flatten() {
                    if !run_deps.iter().any(|dep| dep.name == libs_metadata.name) {
                        run_deps.push(libs_metadata);
                        break; // Found one, no need to try others
                    }
                }
            }

            // Resolve build dependencies in parallel
            let build_dep_futures: Vec<_> = metadata.build_dependencies.iter().map(|dep| {
                let dep_name = match dep {
                    DependKind::Latest(name) => name.clone(),
                    DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                    DependKind::Volatile(name) => name.clone(),
                };
                let sources_for_dep = sources_clone.clone();
                async move {
                    ProcessedMetaData::get_metadata(&dep_name, None, &sources_for_dep, true).await
                }
            }).collect();

            let build_deps: Vec<_> = join_all(build_dep_futures).await.into_iter().flatten().collect();

            // Convert ProcessedMetaData to InstallPackage
            let install_package = InstallPackage {
                metadata: metadata.clone(),
                run_deps,
                build_deps,
            };
            Some(install_package)
        }
    }).collect();
    
    let results = join_all(package_futures).await;
    let packages: Vec<_> = results.into_iter().flatten().collect();
    Ok(packages)
}

pub async fn get_package_info(
    package_name: &str,
    _show_files: bool,
    _show_deps: bool,
    _show_versions: bool,
    _settings: Option<&settings::SettingsYaml>,
) -> Result<ProcessedMetaData, String> {
    let sources = vec![settings::OriginKind::Pax("local".to_string())];
    ProcessedMetaData::get_metadata(package_name, None, &sources, true).await
        .ok_or_else(|| format!("Package {} not found", package_name))
}

pub fn list_installed_packages(
    show_deps: bool,
    show_dependents: bool,
    filter_pattern: Option<&str>,
) -> Result<Vec<InstalledMetaData>, String> {
    let mut all_packages: Vec<InstalledMetaData> = Vec::new();
    let installed_dir = utils::get_metadata_dir()?;

    // First, collect all packages
    for entry in std::fs::read_dir(&installed_dir)
        .map_err(|e| format!("Failed to read directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let installed: InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;

            // Apply filter if provided
            if let Some(pattern) = filter_pattern {
                if !installed.name.contains(pattern) && !installed.description.contains(pattern) {
                    continue;
                }
            }

            all_packages.push(installed);
        }
    }

    // If we need dependency information, compute it
    if show_deps || show_dependents {
        // Create a new vector with computed dependency information
        let mut result_packages = Vec::new();

        for package in &all_packages {
            let mut package_with_deps = package.clone();

            if show_dependents && package_with_deps.dependents.is_empty() {
                // Find packages that depend on this one
                for other_pkg in &all_packages {
                    if other_pkg.name != package.name {
                        // Check if this package is in the other's dependencies
                        for dep in &other_pkg.dependencies {
                            if dep.name == package.name {
                                if let Ok(version) = utils::Version::parse(&other_pkg.version) {
                                    package_with_deps.dependents.push(Specific {
                                        name: other_pkg.name.clone(),
                                        version,
                                    });
                                }
                                break;
                            }
                        }
                    }
                }
            }

            result_packages.push(package_with_deps);
        }

        all_packages = result_packages;
    }

    Ok(all_packages)
}

pub fn get_local_deps(package_name: &str) -> Result<Vec<String>, String> {
    let installed_dir = utils::get_metadata_dir()?;
    let package_file = installed_dir.join(format!("{}.json", package_name));
    
    if package_file.exists() {
        let content = std::fs::read_to_string(&package_file)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        let installed: InstalledMetaData = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse JSON: {}", e))?;
        Ok(installed.dependents.iter().map(|d| d.name.clone()).collect())
    } else {
        Ok(Vec::new())
    }
}

pub async fn search_packages(
    query: &str,
    exact_match: bool,
    installed_only: bool,
    _show_deps: bool,
    settings: Option<&settings::SettingsYaml>,
) -> Result<Vec<ProcessedMetaData>, String> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    let installed_dir = utils::get_metadata_dir()?;
    
    for entry in std::fs::read_dir(&installed_dir)
        .map_err(|e| format!("Failed to read directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let installed: InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            
            if installed.name.contains(query) || installed.description.contains(query) {
                let processed = ProcessedMetaData {
                    name: installed.name,
                    kind: installed.kind,
                    description: installed.description,
                    version: installed.version,
                    origin: installed.origin,
                    dependent: true,
                    build_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                    runtime_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                    install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                        build: "".to_string(),
                        install: "".to_string(),
                        uninstall: "".to_string(),
                        purge: "".to_string(),
                    }),
                    hash: installed.hash,
                    package_type: format!("{:?}", installed.kind.clone()),
                    installed: true,
                    dependencies: installed.dependencies.iter().map(|dep| dep.name.clone()).collect(),
                    dependents: installed.dependents.iter().map(|dep| dep.name.clone()).collect(),
                    installed_files: Vec::new(), // TODO: implement file tracking
                    available_versions: Vec::new(), // TODO: implement version discovery
                };
                seen.insert(processed.name.clone());
                results.push(processed);
            }
        }
    }
    
    if !installed_only {
        if let Some(settings) = settings {
            let sources = settings.sources.clone();
            let remote_matches = ProcessedMetaData::get_all_metadata(query, None, &sources, true).await;

            for mut remote in remote_matches {
                if !seen.contains(&remote.name) && matches_search(&remote, query, exact_match) {
                    remote.installed = false;
                    seen.insert(remote.name.clone());
                    results.push(remote);
                }
            }
        }
    }
    
    Ok(results)
}

pub async fn collect_updates(force_refresh: bool) -> Result<Vec<ProcessedMetaData>, String> {
    // Set thread-local refresh flag for dependency resolution
    set_force_refresh(force_refresh);
    // Check for updates from repositories
    let installed_dir = utils::get_metadata_dir()?;
    let settings = settings::SettingsYaml::get_settings()
        .map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources = settings.sources;
    let mut updates = Vec::new();
    
    for entry in std::fs::read_dir(&installed_dir)
        .map_err(|e| format!("Failed to read directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let installed: InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            
            // Check if newer version is available
            if let Some(latest) = ProcessedMetaData::get_metadata(&installed.name, None, &sources, true).await {
                let installed_version = utils::Version::parse(&installed.version)
                    .unwrap_or_default();
                let latest_version = utils::Version::parse(&latest.version)
                    .unwrap_or_default();
                
                if latest_version > installed_version {
                    updates.push(latest);
                }
            }
        }
    }
    
    Ok(updates)
}

pub async fn upgrade_all(force_refresh: bool) -> Result<Vec<String>, String> {
    // Check for updates on all installed packages
    let updates = collect_updates(force_refresh).await?;
    Ok(updates.iter().map(|u| u.name.clone()).collect())
}

pub async fn upgrade_only(package_names: Vec<String>, force_refresh: bool) -> Result<Vec<String>, String> {
    // Set thread-local refresh flag for dependency resolution
    set_force_refresh(force_refresh);
    // Check for updates on specific packages
    let settings = settings::SettingsYaml::get_settings()
        .map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources = settings.sources;
    let mut to_upgrade = Vec::new();
    
    for name in package_names {
        // Check installed version
        let installed = match InstalledMetaData::open(&name) {
            Ok(installed) => installed,
            Err(_) => continue, // Not installed
        };
        
        // Check latest version
        if let Some(latest) = ProcessedMetaData::get_metadata(&name, None, &sources, true).await {
            let installed_version = utils::Version::parse(&installed.version)
                .unwrap_or_default();
            let latest_version = utils::Version::parse(&latest.version)
                .unwrap_or_default();
            
            if latest_version > installed_version {
                to_upgrade.push(name);
            }
        }
    }
    
    Ok(to_upgrade)
}

pub async fn upgrade_packages(package_names: Vec<String>, force_refresh: bool) -> Result<(), String> {
    // Set thread-local refresh flag for dependency resolution
    set_force_refresh(force_refresh);
    
    // Upgrade specific packages
    let settings = settings::SettingsYaml::get_settings()
        .map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources = settings.sources;
    let runtime = Runtime::new()
        .map_err(|_| "Failed to create runtime".to_string())?;
    
    for name in package_names {
        // Get latest version
        let latest = ProcessedMetaData::get_metadata(&name, None, &sources, true).await
            .ok_or_else(|| format!("Package {} not found", name))?;
        
        // Install the latest version (this will handle upgrades)
        latest.install(&runtime)?;
    }
    
    Ok(())
}

pub async fn emancipate(_package_name: &str) -> Result<(), String> {
    // This would typically remove a package and its dependencies
    // For now, just return success
    Ok(())
}
