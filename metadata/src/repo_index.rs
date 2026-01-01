use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH, Duration};
use std::sync::{Mutex, OnceLock};
use serde::{Deserialize, Serialize};
use settings::OriginKind;
use crate::processed::ProcessedMetaData;
use crate::depend_kind::DependKind;
use utils::get_update_dir;

// Cache for mirror URL to avoid repeated blocking network calls
static MIRROR_CACHE: OnceLock<Mutex<(Option<String>, u64)>> = OnceLock::new();
const MIRROR_CACHE_TTL_MS: u64 = 3600 * 1000; // 1 hour

fn get_cached_mirror_url() -> Result<String, String> {
    let cache = MIRROR_CACHE.get_or_init(|| Mutex::new((None, 0)));
    let mut guard = cache.lock().unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    
    // Check if cache is valid
    if let (Some(cached_url), cached_time) = &*guard {
        if now.saturating_sub(*cached_time) < MIRROR_CACHE_TTL_MS {
            return Ok(cached_url.clone());
        }
    }
    
    // Cache miss or expired - fetch new mirror
    let mirror_url = settings::get_best_mirror_url()?;
    *guard = (Some(mirror_url.clone()), now);
    Ok(mirror_url)
}

/// Repository index - contains all package metadata for a repo
/// Built once per repo, used for O(1) lookups during resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoIndex {
    // package name -> list of versions (sorted, latest first)
    pub packages: HashMap<String, Vec<ProcessedMetaData>>,
    
    // provides(lib/file) -> list of packages that provide it
    pub provides_lib: HashMap<String, Vec<String>>,
    pub provides_file: HashMap<String, Vec<String>>,
    // provides(virtual package name) -> list of packages that provide it
    pub provides_pkg: HashMap<String, Vec<String>>,
    
    // package name -> all its dependencies (for fast graph traversal)
    pub dependencies: HashMap<String, Vec<DependKind>>,
    
    // Repo origin for tracking
    pub origin: OriginKind,
    
    // Cache key (repo URL + revision hash if available)
    pub cache_key: String,
}

impl RepoIndex {
    /// Resolve the display URL for a PAX origin that uses mirror lists
    /// Returns the resolved mirror URL if applicable, otherwise returns the original origin
    fn resolve_display_origin(origin: &OriginKind) -> OriginKind {
        if let OriginKind::Pax(url) = origin {
            // Check if this is a mirror-based PAX repo (contains "oreon" and might use mirror list)
            if url.contains("oreon") {
                // Try to get the current resolved mirror URL
                if let Ok(mirror_base) = get_cached_mirror_url() {
                    // Extract the path part from the original URL (e.g., "oreon-11/unstable/x86_64v3")
                    if let Some(path_start) = url.find("oreon-11") {
                        let path_part = &url[path_start..];
                        // Construct the resolved URL
                        let resolved_url = if mirror_base.contains("oreon-11") {
                            // Mirror already has full path
                            mirror_base.trim_end_matches('/').to_string()
                        } else {
                            // Mirror is base URL, append path
                            format!("{}/{}", mirror_base.trim_end_matches('/'), path_part)
                        };
                        return OriginKind::Pax(resolved_url);
                    }
                }
            }
        }
        origin.clone()
    }
    
    /// Load or build index for a repository
    /// Returns cached index if available and fresh, otherwise fetches and builds
    pub async fn load_or_build(origin: &OriginKind, force_refresh: bool) -> Result<Self, String> {
        use std::time::{SystemTime, UNIX_EPOCH};
        use std::fs::OpenOptions;
        use std::io::Write;
        
        let load_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
            let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/repo_index.rs:35\",\"message\":\"load_or_build_start\",\"data\":{{\"origin\":\"{:?}\",\"force_refresh\":{},\"timestamp\":{}}},\"timestamp\":{}}}", origin, force_refresh, load_start, load_start);
        }
        
        let cache_key = Self::cache_key_for_origin(origin);
        
        let before_cache_check = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
            let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/repo_index.rs:40\",\"message\":\"before_cache_check\",\"data\":{{\"timestamp\":{}}},\"timestamp\":{}}}", before_cache_check, before_cache_check);
        }
        
        // Try to load from disk cache first (24 hour TTL) unless force_refresh is true
        if !force_refresh {
            if let Ok(cached) = Self::load_from_cache(&cache_key) {
                let after_cache_check = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                    let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/repo_index.rs:42\",\"message\":\"cache_hit\",\"data\":{{\"timestamp\":{},\"duration_ms\":{}}},\"timestamp\":{}}}", after_cache_check, after_cache_check.saturating_sub(before_cache_check), after_cache_check);
                }
                let display_origin = Self::resolve_display_origin(origin);
                eprintln!("Using cached index for {:?}", display_origin);
                return Ok(cached);
            }
        } else {
            let display_origin = Self::resolve_display_origin(origin);
            eprintln!("Force refreshing index for {:?}", display_origin);
        }
        
        let after_cache_check = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
            let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/repo_index.rs:49\",\"message\":\"cache_miss_or_force\",\"data\":{{\"timestamp\":{},\"duration_ms\":{}}},\"timestamp\":{}}}", after_cache_check, after_cache_check.saturating_sub(before_cache_check), after_cache_check);
        }
        
        // Build index by fetching all repo metadata
        let index = Self::build_index(origin).await?;
        
        // Save to cache
        if let Err(e) = index.save_to_cache() {
            eprintln!("Warning: Failed to save cache: {}", e);
        }
        
        Ok(index)
    }
    
    /// Build index by fetching all metadata from repo
    async fn build_index(origin: &OriginKind) -> Result<Self, String> {
        match origin {
            OriginKind::Rpm(url) | OriginKind::Yum(url) => {
                Self::build_rpm_index(url).await
            }
            OriginKind::Pax(url) => {
                Self::build_pax_index(url).await
            }
            OriginKind::Deb(url) => {
                Self::build_deb_index(url).await
            }
            OriginKind::Github { .. } | OriginKind::Apt(_) | OriginKind::CloudflareR2 { .. } | OriginKind::LocalDir(_) => {
                // These repos don't have a single metadata file
                // For now, return empty index (will fall back to per-package fetches)
                Ok(Self {
                    packages: HashMap::new(),
                    provides_lib: HashMap::new(),
                    provides_file: HashMap::new(),
                    provides_pkg: HashMap::new(),
                    dependencies: HashMap::new(),
                    origin: origin.clone(),
                    cache_key: Self::cache_key_for_origin(origin),
                })
            }
        }
    }
    
    /// Build index from RPM repository (uses repodata/primary.xml)
    async fn build_rpm_index(base_url: &str) -> Result<Self, String> {
        use crate::yum_repository::YumRepositoryClient;
        use std::time::SystemTime;
        
        let build_start = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        eprintln!("Building RPM index from {}...", base_url);
        
        let client = YumRepositoryClient::new(base_url.to_string());
        let packages_info = client.list_packages().await?;
        
        eprintln!("Parsed {} packages, building index...", packages_info.len());
        
        let mut packages: HashMap<String, Vec<ProcessedMetaData>> = HashMap::new();
        let mut provides_lib: HashMap<String, Vec<String>> = HashMap::new();
        let mut provides_file: HashMap<String, Vec<String>> = HashMap::new();
        let mut provides_pkg: HashMap<String, Vec<String>> = HashMap::new();
        let mut dependencies: HashMap<String, Vec<DependKind>> = HashMap::new();
        
        let total = packages_info.len();
        for (idx, pkg_info) in packages_info.into_iter().enumerate() {
            if idx % 10000 == 0 && idx > 0 {
                eprintln!("Indexed {}/{} packages...", idx, total);
            }
            // Convert YumPackageInfo to ProcessedMetaData
            use crate::parsers::MetaDataKind;
            use crate::processed::{ProcessedInstallKind, PreBuilt};
            
            let metadata = ProcessedMetaData {
                name: pkg_info.name,
                kind: MetaDataKind::Rpm,
                description: pkg_info.description,
                version: format!("{}-{}", pkg_info.version, pkg_info.release),
                origin: OriginKind::Rpm(base_url.to_string()),
                dependent: false,
                build_dependencies: Vec::new(),
                runtime_dependencies: pkg_info.dependencies.into_iter()
                    .map(|dep| DependKind::Latest(dep))
                    .collect(),
                install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                    critical: Vec::new(), // File lists not available in primary.xml
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
            
            // Index by package name (normalized to lowercase for case-insensitive lookup)
            let normalized_name = metadata.name.to_lowercase();
            packages.entry(normalized_name.clone())
                .or_insert_with(Vec::new)
                .push(metadata.clone());
            
            // Index package provides (virtual package names)
            for provide in &pkg_info.provides {
                let normalized_provide = provide.to_lowercase();
                provides_pkg.entry(normalized_provide)
                    .or_insert_with(Vec::new)
                    .push(normalized_name.clone());
            }
            
            // Index provides (libraries and files)
            if let crate::processed::ProcessedInstallKind::PreBuilt(ref prebuilt) = metadata.install_kind {
                for file in &prebuilt.critical {
                    provides_file.entry(file.clone())
                        .or_insert_with(Vec::new)
                        .push(normalized_name.clone());
                    
                    // Extract library name
                    if file.contains(".so") {
                        if let Some(lib_name) = file.split('/').last() {
                            provides_lib.entry(lib_name.to_string())
                                .or_insert_with(Vec::new)
                                .push(normalized_name.clone());
                        }
                    }
                }
            }
            
            // Index dependencies (use normalized name as key)
            dependencies.insert(normalized_name, metadata.runtime_dependencies.clone());
        }
        
        // Sort versions for each package (latest first)
        for versions in packages.values_mut() {
            versions.sort_by(|a, b| {
                utils::Version::parse(&b.version)
                    .cmp(&utils::Version::parse(&a.version))
            });
        }
        
        let build_end = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        eprintln!("RPM index built in {}ms ({} packages)", build_end.saturating_sub(build_start), packages.len());
        
        Ok(Self {
            packages,
            provides_lib,
            provides_file,
            provides_pkg,
            dependencies,
            origin: OriginKind::Rpm(base_url.to_string()),
            cache_key: Self::cache_key_for_origin(&OriginKind::Rpm(base_url.to_string())),
        })
    }
    
    /// Build index from PAX repository (uses metadata/packages.json)
    async fn build_pax_index(base_url: &str) -> Result<Self, String> {
        use std::time::{SystemTime, UNIX_EPOCH};
        use std::fs::OpenOptions;
        use std::io::Write;
        
        let build_pax_start = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
            let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/repo_index.rs:186\",\"message\":\"build_pax_index_start\",\"data\":{{\"base_url\":\"{}\",\"timestamp\":{}}},\"timestamp\":{}}}", base_url, build_pax_start, build_pax_start);
        }
        
        // For Oreon repos, use mirror list and append /metadata/packages.json
        // Also determine the actual base URL to use for fetching individual packages
        let (index_url, actual_base_url) = if base_url.contains("oreon") {
            let before_mirror = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/repo_index.rs:221\",\"message\":\"before_get_best_mirror\",\"data\":{{\"timestamp\":{}}},\"timestamp\":{}}}", before_mirror, before_mirror);
            }
            
            // Extract the path part from base_url (e.g., "oreon-11/unstable/x86_64v3")
            let path_part = if let Some(path_start) = base_url.find("oreon-11") {
                &base_url[path_start..]
            } else {
                return Err(format!("Invalid Oreon repo URL: {}", base_url));
            };
            
            // Get best mirror from mirror list (returns base mirror URL, cached)
            let mirror_base = get_cached_mirror_url()
                .map_err(|e| format!("Failed to get mirror: {}", e))?;
            
            let after_mirror = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"metadata/src/repo_index.rs:234\",\"message\":\"after_get_best_mirror\",\"data\":{{\"mirror\":\"{}\",\"timestamp\":{},\"duration_ms\":{}}},\"timestamp\":{}}}", mirror_base, after_mirror, after_mirror.saturating_sub(before_mirror), after_mirror);
            }
            
            // Construct index URL and actual base URL for fetching packages
            let (idx_url, actual_base) = if mirror_base.contains("oreon-11") {
                // Mirror already has full path
                let base = mirror_base.trim_end_matches('/');
                let idx = format!("{}/metadata/packages.json", base);
                let actual = base.to_string();
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                    let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"url_debug\",\"hypothesisId\":\"URL_DUP\",\"location\":\"metadata/src/repo_index.rs:267\",\"message\":\"mirror_has_path\",\"data\":{{\"base_url\":\"{}\",\"mirror_base\":\"{}\",\"path_part\":\"{}\",\"actual_base\":\"{}\",\"index_url\":\"{}\"}},\"timestamp\":{}}}", base_url, mirror_base, path_part, actual, idx, SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64);
                }
                (idx, actual)
            } else {
                // Mirror is just the base domain, need to append path
                let base = format!("{}/{}", mirror_base.trim_end_matches('/'), path_part);
                let idx = format!("{}/metadata/packages.json", base);
                if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                    let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"url_debug\",\"hypothesisId\":\"URL_DUP\",\"location\":\"metadata/src/repo_index.rs:273\",\"message\":\"mirror_needs_path\",\"data\":{{\"base_url\":\"{}\",\"mirror_base\":\"{}\",\"path_part\":\"{}\",\"actual_base\":\"{}\",\"index_url\":\"{}\"}},\"timestamp\":{}}}", base_url, mirror_base, path_part, base, idx, SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64);
                }
                (idx, base)
            };
            (idx_url, actual_base)
        } else {
            // For other PAX repos, use the base URL directly
            let base = base_url.trim_end_matches('/');
            (format!("{}/metadata/packages.json", base), base.to_string())
        };
        
        // Check if repo is reachable first
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;
        
        let response = client.get(&index_url).send().await
            .map_err(|e| format!("Failed to fetch packages.json: {}", e))?;
        
        if !response.status().is_success() {
            return Err(format!("packages.json not found ({}): {}", response.status(), index_url));
        }
        
        let text = response.text().await
            .map_err(|e| format!("Failed to read packages.json: {}", e))?;
        let index_data: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse packages.json: {}", e))?;
        
        let packages_array = index_data.get("packages")
            .and_then(|p| p.as_array())
            .ok_or_else(|| "packages.json missing packages array".to_string())?;
        
        let mut packages: HashMap<String, Vec<ProcessedMetaData>> = HashMap::new();
        let mut provides_lib: HashMap<String, Vec<String>> = HashMap::new();
        let mut provides_file: HashMap<String, Vec<String>> = HashMap::new();
        let mut provides_pkg: HashMap<String, Vec<String>> = HashMap::new();
        let mut dependencies: HashMap<String, Vec<DependKind>> = HashMap::new();
        
        // Fetch all package metadata in parallel
        let mut fetch_futures: Vec<_> = Vec::new();
        for pkg in packages_array {
            if let (Some(name_val), Some(path_val)) = (pkg.get("name"), pkg.get("path")) {
                if let (Some(name), Some(path)) = (name_val.as_str(), path_val.as_str()) {
                    // Use actual_base_url (which may be the mirror URL for Oreon repos)
                    let url = format!("{}/{}", actual_base_url.trim_end_matches('/'), path);
                    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                        let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"url_debug\",\"hypothesisId\":\"URL_DUP\",\"location\":\"metadata/src/repo_index.rs:315\",\"message\":\"fetching_package\",\"data\":{{\"name\":\"{}\",\"path\":\"{}\",\"actual_base_url\":\"{}\",\"full_url\":\"{}\"}},\"timestamp\":{}}}", name, path, actual_base_url, url, SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64);
                    }
                    let name_str = name.to_string();
                    fetch_futures.push(async move {
                        // Fetch PAX metadata from URL (now public)
                        if let Some(metadata) = crate::processed::ProcessedMetaData::fetch_pax_metadata_from_url(&url).await {
                            Ok::<_, String>((name_str, metadata))
                        } else {
                            Err(format!("Failed to fetch PAX metadata from {}", url))
                        }
                    });
                }
            }
        }
        
        let results = futures::future::join_all(fetch_futures).await;
        
        for result in results {
            if let Ok((_name, metadata)) = result {
                // Index by package name from metadata (normalized to lowercase for case-insensitive lookup)
                // Use metadata.name instead of name from packages.json, as packages.json may have versioned names
                let normalized_name = metadata.name.to_lowercase();
                packages.entry(normalized_name.clone())
                    .or_insert_with(Vec::new)
                    .push(metadata.clone());
                
                // Index provides
                if let crate::processed::ProcessedInstallKind::PreBuilt(ref prebuilt) = metadata.install_kind {
                    for file in &prebuilt.critical {
                        provides_file.entry(file.clone())
                            .or_insert_with(Vec::new)
                            .push(normalized_name.clone());
                        
                        if file.contains(".so") {
                            if let Some(lib_name) = file.split('/').last() {
                                provides_lib.entry(lib_name.to_string())
                                    .or_insert_with(Vec::new)
                                    .push(normalized_name.clone());
                            }
                        }
                    }
                }
                
                // Index dependencies (use normalized name as key)
                dependencies.insert(normalized_name, metadata.runtime_dependencies.clone());
            }
        }
        
        // Sort versions
        for versions in packages.values_mut() {
            versions.sort_by(|a, b| {
                utils::Version::parse(&b.version)
                    .cmp(&utils::Version::parse(&a.version))
            });
        }
        
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
            let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"url_debug\",\"hypothesisId\":\"URL_DUP\",\"location\":\"metadata/src/repo_index.rs:374\",\"message\":\"storing_origin\",\"data\":{{\"base_url\":\"{}\",\"actual_base_url\":\"{}\"}},\"timestamp\":{}}}", base_url, actual_base_url, SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64);
        }
        
        Ok(Self {
            packages,
            provides_lib,
            provides_file,
            provides_pkg: HashMap::new(), // PAX packages don't have provides
            dependencies,
            // Use actual_base_url (which may be the mirror URL for Oreon repos) for origin
            origin: OriginKind::Pax(actual_base_url.clone()),
            cache_key: Self::cache_key_for_origin(&OriginKind::Pax(actual_base_url)),
        })
    }
    
    /// Build index from Debian repository
    async fn build_deb_index(base_url: &str) -> Result<Self, String> {
        use crate::deb_repository::DebRepositoryClient;
        
        let client = DebRepositoryClient::new(base_url.to_string());
        let packages_info = client.list_packages().await?;
        
        let mut packages: HashMap<String, Vec<ProcessedMetaData>> = HashMap::new();
        let mut provides_lib: HashMap<String, Vec<String>> = HashMap::new();
        let mut provides_file: HashMap<String, Vec<String>> = HashMap::new();
        let mut provides_pkg: HashMap<String, Vec<String>> = HashMap::new();
        let mut dependencies: HashMap<String, Vec<DependKind>> = HashMap::new();
        
        for pkg_info in packages_info {
            // Convert DebPackageInfo to ProcessedMetaData
            use crate::parsers::MetaDataKind;
            use crate::processed::{ProcessedInstallKind, PreBuilt};
            
            let metadata = ProcessedMetaData {
                name: pkg_info.name,
                kind: MetaDataKind::Deb,
                description: pkg_info.description,
                version: pkg_info.version,
                origin: OriginKind::Deb(base_url.to_string()),
                dependent: false,
                build_dependencies: Vec::new(),
                runtime_dependencies: pkg_info.dependencies.into_iter()
                    .map(|dep| DependKind::Latest(dep))
                    .collect(),
                install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                    critical: Vec::new(), // File lists not available in Packages file
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
            
            // Index by package name (normalized to lowercase for case-insensitive lookup)
            let normalized_name = metadata.name.to_lowercase();
            packages.entry(normalized_name.clone())
                .or_insert_with(Vec::new)
                .push(metadata.clone());
            
            if let crate::processed::ProcessedInstallKind::PreBuilt(ref prebuilt) = metadata.install_kind {
                for file in &prebuilt.critical {
                    provides_file.entry(file.clone())
                        .or_insert_with(Vec::new)
                        .push(normalized_name.clone());
                    
                    if file.contains(".so") {
                        if let Some(lib_name) = file.split('/').last() {
                            provides_lib.entry(lib_name.to_string())
                                .or_insert_with(Vec::new)
                                .push(normalized_name.clone());
                        }
                    }
                }
            }
            
            // Index dependencies (use normalized name as key)
            dependencies.insert(normalized_name, metadata.runtime_dependencies.clone());
        }
        
        for versions in packages.values_mut() {
            versions.sort_by(|a, b| {
                utils::Version::parse(&b.version)
                    .cmp(&utils::Version::parse(&a.version))
            });
        }
        
        Ok(Self {
            packages,
            provides_lib,
            provides_file,
            provides_pkg: HashMap::new(), // DEB packages don't have provides indexed yet
            dependencies,
            origin: OriginKind::Deb(base_url.to_string()),
            cache_key: Self::cache_key_for_origin(&OriginKind::Deb(base_url.to_string())),
        })
    }
    
    /// Lookup package by name (returns latest version)
    pub fn lookup_package(&self, name: &str) -> Option<&ProcessedMetaData> {
        // Normalize to lowercase for case-insensitive lookup
        self.packages.get(&name.to_lowercase())?.first()
    }
    
    /// Lookup packages that provide a library
    pub fn lookup_provides_lib(&self, lib: &str) -> Vec<&String> {
        self.provides_lib.get(lib)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
    
    /// Lookup packages that provide a file
    pub fn lookup_provides_file(&self, file: &str) -> Vec<&String> {
        self.provides_file.get(file)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
    
    /// Lookup packages that provide a virtual package name
    pub fn lookup_provides_pkg(&self, pkg: &str) -> Vec<&String> {
        // Normalize to lowercase for case-insensitive lookup
        self.provides_pkg.get(&pkg.to_lowercase())
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
    
    /// Get dependencies for a package
    pub fn get_dependencies(&self, name: &str) -> Option<&Vec<DependKind>> {
        // Normalize to lowercase for case-insensitive lookup
        self.dependencies.get(&name.to_lowercase())
    }
    
    fn cache_key_for_origin(origin: &OriginKind) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        format!("{:?}", origin).hash(&mut hasher);
        format!("repo_{:x}", hasher.finish())
    }
    
    fn cache_path() -> Result<PathBuf, String> {
        let mut dir = get_update_dir()?;
        dir.push("repo_indexes");
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create cache dir: {}", e))?;
        Ok(dir)
    }
    
    fn load_from_cache(cache_key: &str) -> Result<Self, String> {
        let cache_dir = Self::cache_path()?;
        let cache_file = cache_dir.join(format!("{}.json", cache_key));
        
        if !cache_file.exists() {
            return Err("Cache file not found".to_string());
        }
        
        // Check if cache is expired (24 hours TTL)
        let metadata = fs::metadata(&cache_file)
            .map_err(|e| format!("Failed to read cache metadata: {}", e))?;
        let modified = metadata.modified()
            .map_err(|e| format!("Failed to get cache mtime: {}", e))?;
        let age = SystemTime::now().duration_since(modified)
            .unwrap_or(Duration::from_secs(0));
        if age > Duration::from_secs(24 * 3600) {
            return Err("Cache expired".to_string());
        }
        
        let content = fs::read_to_string(&cache_file)
            .map_err(|e| format!("Failed to read cache: {}", e))?;
        
        serde_json::from_str(&content)
            .map_err(|e| format!("Failed to deserialize cache: {}", e))
    }
    
    fn save_to_cache(&self) -> Result<(), String> {
        let cache_dir = Self::cache_path()?;
        let cache_file = cache_dir.join(format!("{}.json", self.cache_key));
        
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize index: {}", e))?;
        
        fs::write(&cache_file, json)
            .map_err(|e| format!("Failed to write cache: {}", e))?;
        
        Ok(())
    }
}

/// Multi-repo index - combines indexes from all configured repos
#[derive(Debug, Clone)]
pub struct MultiRepoIndex {
    indexes: Vec<RepoIndex>,
}

impl MultiRepoIndex {
    pub async fn build(sources: &[OriginKind], force_refresh: bool) -> Result<Self, String> {
        use std::time::SystemTime;
        use futures::future::join_all;
        
        let build_start = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        if force_refresh {
            eprintln!("Force refreshing indexes for {} repositories...", sources.len());
        } else {
            eprintln!("Building indexes for {} repositories...", sources.len());
        }
        
        // Build all indexes in parallel
        let build_futures: Vec<_> = sources.iter().map(|source| {
            let source = source.clone();
            async move {
                RepoIndex::load_or_build(&source, force_refresh).await
            }
        }).collect();
        
        let results = join_all(build_futures).await;
        
        let mut indexes = Vec::new();
        let mut successful = 0;
        let mut failed = 0;
        
        for (source, result) in sources.iter().zip(results) {
            match result {
                Ok(index) => {
                    indexes.push(index);
                    successful += 1;
                }
                Err(e) => {
                    eprintln!("Warning: Failed to build index for {:?}: {}", source, e);
                    failed += 1;
                }
            }
        }
        
        let build_end = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        eprintln!("Index building complete: {} successful, {} failed, {}ms total", successful, failed, build_end.saturating_sub(build_start));
        
        if indexes.is_empty() {
            return Err("No repositories could be indexed".to_string());
        }
        
        Ok(Self { indexes })
    }
    
    /// Get only PAX indexes (for PAX package dependency resolution)
    fn pax_indexes(&self) -> Vec<&RepoIndex> {
        self.indexes.iter()
            .filter(|idx| matches!(idx.origin, OriginKind::Pax(_)))
            .collect()
    }
    
    /// Lookup package across all repos (returns first match)
    pub fn lookup_package(&self, name: &str) -> Option<&ProcessedMetaData> {
        for index in &self.indexes {
            if let Some(pkg) = index.lookup_package(name) {
                return Some(pkg);
            }
        }
        None
    }
    
    /// Lookup package in PAX repos only (for PAX package dependency resolution)
    pub fn lookup_package_pax_only(&self, name: &str) -> Option<&ProcessedMetaData> {
        for index in self.pax_indexes() {
            if let Some(pkg) = index.lookup_package(name) {
                return Some(pkg);
            }
        }
        None
    }
    
    /// Lookup all versions of a package across all repos
    pub fn lookup_all_versions(&self, name: &str) -> Vec<ProcessedMetaData> {
        // Normalize to lowercase for case-insensitive lookup
        let normalized_name = name.to_lowercase();
        let mut matches = Vec::new();
        for index in &self.indexes {
            if let Some(versions) = index.packages.get(&normalized_name) {
                matches.extend(versions.iter().cloned());
            }
        }
        matches
    }
    
    /// Lookup all versions of a package in PAX repos only (for PAX package dependency resolution)
    pub fn lookup_all_versions_pax_only(&self, name: &str) -> Vec<ProcessedMetaData> {
        // Normalize to lowercase for case-insensitive lookup
        let normalized_name = name.to_lowercase();
        let mut matches = Vec::new();
        for index in self.pax_indexes() {
            if let Some(versions) = index.packages.get(&normalized_name) {
                matches.extend(versions.iter().cloned());
            }
        }
        matches
    }
    
    /// Lookup packages that provide a library across all repos
    pub fn lookup_provides_lib(&self, lib: &str) -> Vec<&String> {
        let mut result = Vec::new();
        for index in &self.indexes {
            result.extend(index.lookup_provides_lib(lib));
        }
        result
    }
    
    /// Lookup packages that provide a library in PAX repos only
    pub fn lookup_provides_lib_pax_only(&self, lib: &str) -> Vec<&String> {
        let mut result = Vec::new();
        for index in self.pax_indexes() {
            result.extend(index.lookup_provides_lib(lib));
        }
        result
    }
    
    pub fn lookup_provides_file(&self, file: &str) -> Vec<&String> {
        let mut result = Vec::new();
        for index in &self.indexes {
            result.extend(index.lookup_provides_file(file));
        }
        result
    }
    
    pub fn lookup_provides_pkg(&self, pkg: &str) -> Vec<&String> {
        let mut result = Vec::new();
        for index in &self.indexes {
            result.extend(index.lookup_provides_pkg(pkg));
        }
        result
    }
    
    /// Lookup packages that provide a virtual package in PAX repos only
    pub fn lookup_provides_pkg_pax_only(&self, pkg: &str) -> Vec<&String> {
        let mut result = Vec::new();
        for index in self.pax_indexes() {
            result.extend(index.lookup_provides_pkg(pkg));
        }
        result
    }
    
    /// Get dependencies for a package (checks all indexes and merges ALL dependencies)
    pub fn get_dependencies(&self, name: &str) -> Option<Vec<DependKind>> {
        // Normalize to lowercase for case-insensitive lookup
        let normalized_name = name.to_lowercase();
        let mut all_deps = Vec::new();
        let mut seen = std::collections::HashSet::new();
        
        // Check ALL indexes, not just where package exists
        for index in &self.indexes {
            if let Some(deps) = index.get_dependencies(&normalized_name) {
                for dep in deps {
                    let dep_name = match dep {
                        DependKind::Latest(n) => n,
                        DependKind::Specific(dv) => &dv.name,
                        DependKind::Volatile(n) => n,
                    };
                    
                    // Filter out virtual packages using pattern-based heuristic (no hardcoding)
                    let name_lower = dep_name.to_lowercase();
                    let has_separators = name_lower.contains('-') || name_lower.contains('_') || name_lower.contains('.');
                    let has_numbers = name_lower.chars().any(|c| c.is_ascii_digit());
                    let is_single_word = !name_lower.contains(' ') && !has_separators;
                    let is_short = name_lower.len() <= 6;
                    let is_likely_virtual = is_single_word && is_short && !has_numbers;
                    
                    // Skip virtual packages
                    if is_likely_virtual {
                        continue;
                    }
                    
                    // Create a unique key for the dependency
                    let dep_key = match dep {
                        DependKind::Latest(n) => n.clone(),
                        DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                        DependKind::Volatile(n) => format!("volatile:{}", n),
                    };
                    
                    if !seen.contains(&dep_key) {
                        seen.insert(dep_key);
                        all_deps.push(dep.clone());
                    }
                }
            }
        }
        
        // Also try to get dependencies from package metadata if available
        if let Some(pkg) = self.lookup_package(&normalized_name) {
            for dep in &pkg.runtime_dependencies {
                let dep_name = match dep {
                    DependKind::Latest(n) => n,
                    DependKind::Specific(dv) => &dv.name,
                    DependKind::Volatile(n) => n,
                };
                
                // Filter out virtual packages using pattern-based heuristic (no hardcoding)
                let name_lower = dep_name.to_lowercase();
                let has_separators = name_lower.contains('-') || name_lower.contains('_') || name_lower.contains('.');
                let has_numbers = name_lower.chars().any(|c| c.is_ascii_digit());
                let is_single_word = !name_lower.contains(' ') && !has_separators;
                let is_short = name_lower.len() <= 6;
                let is_likely_virtual = is_single_word && is_short && !has_numbers;
                
                // Skip virtual packages
                if is_likely_virtual {
                    continue;
                }
                
                let dep_key = match dep {
                    DependKind::Latest(n) => n.clone(),
                    DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                    DependKind::Volatile(n) => format!("volatile:{}", n),
                };
                
                if !seen.contains(&dep_key) {
                    seen.insert(dep_key);
                    all_deps.push(dep.clone());
                }
            }
        }
        
        if all_deps.is_empty() {
            None
        } else {
            Some(all_deps)
        }
    }
    
    /// Get dependencies for a package from PAX repos only (for PAX package dependency resolution)
    pub fn get_dependencies_pax_only(&self, name: &str) -> Option<Vec<DependKind>> {
        // Normalize to lowercase for case-insensitive lookup
        let normalized_name = name.to_lowercase();
        let mut all_deps = Vec::new();
        let mut seen = std::collections::HashSet::new();
        
        // Check only PAX indexes
        for index in self.pax_indexes() {
            if let Some(deps) = index.get_dependencies(&normalized_name) {
                for dep in deps {
                    let dep_key = match dep {
                        DependKind::Latest(n) => n.clone(),
                        DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                        DependKind::Volatile(n) => format!("volatile:{}", n),
                    };
                    
                    if !seen.contains(&dep_key) {
                        seen.insert(dep_key);
                        all_deps.push(dep.clone());
                    }
                }
            }
        }
        
        // Also try to get dependencies from PAX package metadata if available
        if let Some(pkg) = self.lookup_package_pax_only(&normalized_name) {
            for dep in &pkg.runtime_dependencies {
                let dep_key = match dep {
                    DependKind::Latest(n) => n.clone(),
                    DependKind::Specific(dv) => format!("{}:{:?}", dv.name, dv.range),
                    DependKind::Volatile(n) => format!("volatile:{}", n),
                };
                
                if !seen.contains(&dep_key) {
                    seen.insert(dep_key);
                    all_deps.push(dep.clone());
                }
            }
        }
        
        if all_deps.is_empty() {
            None
        } else {
            Some(all_deps)
        }
    }
}
