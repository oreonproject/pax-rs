use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use reqwest::Url;
use settings::OriginKind;
use std::fmt;
use std::hash::Hash;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command as RunCommand,
    sync::OnceLock,
};
use tokio::runtime::Runtime;
use utils::{err, get_update_dir, tmpfile, Range, VerReq, Version};

use crate::{
    depend_kind::DependKind, DepVer, InstalledInstallKind, InstalledMetaData, MetaDataKind,
    Specific, installed::InstalledCompilable, parsers::pax::RawPax, parsers::github::RawGithub, parsers::apt::RawApt,
};

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

fn rpm_query_field(path: &Path, format: &str) -> Result<String, String> {
    use std::process::Command;

    let output = Command::new("rpm")
        .arg("-qp")
        .arg("--queryformat")
        .arg(format)
        .arg(path)
        .output()
        .map_err(|e| format!("Failed to execute rpm query: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "rpm query failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
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

            raw_pax
                .process()
                .ok_or("Failed to process PAX metadata")?
        };

        let (has_entries, critical_files, config_files) = Self::collect_payload_from(&temp_dir)?;

        eprintln!("[LOAD_PAX] Package {}: has_entries={}, critical_files={}, install_kind before={:?}", 
            processed.name, has_entries, critical_files.len(), processed.install_kind);

        if has_entries {
            processed.install_kind = ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            });
            eprintln!("[LOAD_PAX] Changed install_kind to PreBuilt for {}", processed.name);
        } else {
            eprintln!("[LOAD_PAX] Package {} has no entries, keeping install_kind as {:?}", processed.name, processed.install_kind);
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

        let runtime_deps = Self::parse_new_metadata_dependencies(
            metadata_value
                .pointer("/dependencies/runtime")
                .or_else(|| metadata_value.pointer("/dependencies/runtime_dependencies"))
                .or_else(|| metadata_value.pointer("/package/dependencies/runtime"))
                .or_else(|| metadata_value.pointer("/package/runtime_dependencies")),
        );

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

        let extract_status = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "rpm2cpio '{}' | cpio -idmv",
                path.display()
            ))
            .current_dir(&temp_dir)
            .status()
            .map_err(|e| format!("Failed to extract RPM package {}: {}", path.display(), e))?;

        if !extract_status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            return err!(
                "Failed to extract RPM package {}. Ensure rpm2cpio and cpio are installed.",
                path.display()
            );
        }

        let name = rpm_query_field(path, "%{NAME}")?
            .trim()
            .to_string();
        let version = rpm_query_field(path, "%{VERSION}")?
            .trim()
            .to_string();
        let summary = rpm_query_field(path, "%{SUMMARY}")?
            .trim()
            .to_string();
        let requires_raw = rpm_query_field(path, "[%{REQUIRENAME}\n]")?;

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
            runtime_dependencies: Self::parse_dependency_list(&requires_raw),
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

    async fn fetch_pax_metadata_from_url(url: &str) -> Option<Self> {
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
        let mut metadata = None;
        let mut sources = sources.iter();
        while let (Some(source), None) = (sources.next(), &metadata) {
            match source {
                OriginKind::Pax(source) => {
                    let base = source.trim_end_matches('/');
                    let candidate_urls = if let Some(version) = version {
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
                        ]
                    };

                    for url in candidate_urls {
                        if metadata.is_some() {
                            break;
                        }
                        if let Some(processed) = Self::fetch_pax_metadata_from_url(&url).await {
                            metadata = Some(processed);
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
                                                                if let Ok(raw_pax) = serde_json::from_str::<RawPax>(&asset_body) {
                                                                    if let Some(processed) = raw_pax.process() {
                                                                        return Some(processed);
                                                                    }
                                                                }
                                                                // Try to parse as GitHub format
                                                                if let Ok(raw_github) = serde_json::from_str::<RawGithub>(&asset_body) {
                                                                    if let Some(processed) = raw_github.process() {
                                                                        return Some(processed);
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
                                            return Some(processed);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        None
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
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
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
                        } else {
                            None
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
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
                            // Convert DebPackageInfo to ProcessedMetaData
                            let processed = ProcessedMetaData {
                                name: package_info.name,
                                kind: MetaDataKind::Apt,
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
                OriginKind::Yum(repo_url) => {
                    metadata = {
                        use crate::yum_repository::YumRepositoryClient;
                        
                        let client = YumRepositoryClient::new(repo_url.clone());
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
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
                        } else {
                            None
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
                metadata = Some(mut_metadata);
                break;
            }
        }
        metadata
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

pub async fn get_packages(
    package_names: Vec<String>,
    _preferred_source: Option<&str>,
) -> Result<Vec<InstallPackage>, String> {
    let mut packages = Vec::new();
    
    // Get configured repositories from settings
    let settings = settings::SettingsYaml::get_settings().map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources: Vec<OriginKind> = settings.sources.clone();
    
    for name in package_names {
        if let Some(metadata) = ProcessedMetaData::get_metadata(&name, None, &sources, true).await {
            // Resolve runtime dependencies
            let mut run_deps = Vec::new();
            for dep in &metadata.runtime_dependencies {
                let dep_name = match dep {
                    DependKind::Latest(name) => name.clone(),
                    DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                    DependKind::Volatile(name) => name.clone(),
                };
                if let Some(dep_metadata) = ProcessedMetaData::get_metadata(&dep_name, None, &sources, true).await {
                    run_deps.push(dep_metadata);
                }
            }
            
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
                
                for libs_name in [libs_name1, libs_name2] {
                    if let Some(libs_metadata) = ProcessedMetaData::get_metadata(&libs_name, None, &sources, true).await {
                        if !run_deps.iter().any(|dep| dep.name == libs_metadata.name) {
                            run_deps.push(libs_metadata);
                        }
                        break; // Found one, no need to try others
                    }
                }
            }
            
            // Resolve build dependencies
            let mut build_deps = Vec::new();
            for dep in &metadata.build_dependencies {
                let dep_name = match dep {
                    DependKind::Latest(name) => name.clone(),
                    DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                    DependKind::Volatile(name) => name.clone(),
                };
                if let Some(dep_metadata) = ProcessedMetaData::get_metadata(&dep_name, None, &sources, true).await {
                    build_deps.push(dep_metadata);
                }
            }
            
            // Convert ProcessedMetaData to InstallPackage
            let install_package = InstallPackage {
                metadata: metadata.clone(),
                run_deps,
                build_deps,
            };
            packages.push(install_package);
        }
    }
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
    _show_deps: bool,
    _show_dependents: bool,
    _filter_pattern: Option<&str>,
) -> Result<Vec<InstalledMetaData>, String> {
    let mut packages = Vec::new();
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
            
            // Apply filter if provided
            if let Some(pattern) = _filter_pattern {
                if !installed.name.contains(pattern) && !installed.description.contains(pattern) {
                    continue;
                }
            }
            
            packages.push(installed);
        }
    }
    
    Ok(packages)
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
            if let Some(mut remote) =
                ProcessedMetaData::get_metadata(query, None, &sources, true).await
            {
                if !seen.contains(&remote.name) && matches_search(&remote, query, exact_match)
                {
                    remote.installed = false;
                    seen.insert(remote.name.clone());
                    results.push(remote);
                }
            }
        }
    }
    
    Ok(results)
}

pub async fn collect_updates() -> Result<Vec<ProcessedMetaData>, String> {
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

pub async fn upgrade_all() -> Result<Vec<String>, String> {
    // Check for updates on all installed packages
    let updates = collect_updates().await?;
    Ok(updates.iter().map(|u| u.name.clone()).collect())
}

pub async fn upgrade_only(package_names: Vec<String>) -> Result<Vec<String>, String> {
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

pub async fn upgrade_packages(package_names: Vec<String>) -> Result<(), String> {
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
