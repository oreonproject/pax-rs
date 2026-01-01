use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    thread::sleep,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use utils::{PostAction, err, get_dir, is_root};

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MirrorEntry {
    url: String,
    location: Option<String>,
    priority: Option<i32>,
}


#[derive(PartialEq, Serialize, Deserialize, Debug, Clone)]
pub struct SettingsYaml {
    pub locked: bool,
    pub version: String,
    pub arch: Arch,
    pub exec: Option<String>,
    #[serde(default)]
    pub mirror_list: Option<String>,
    pub sources: Vec<OriginKind>,
    #[serde(default)]
    pub disabled_sources: Vec<String>, // URLs of sources that failed health checks
}

impl SettingsYaml {
    pub fn new() -> Self {
        let mut command = std::process::Command::new("/usr/bin/uname");
        let arch = if let Ok(output) = command.arg("-m").output() {
            match String::from_utf8_lossy(&output.stdout)
                .to_string()
                .as_str()
                .trim()
            {
                "x86_64" => {
                    let mut command = std::process::Command::new("/usr/bin/bash");
                    command.arg("-c").arg("(lscpu|grep -q avx512f&&echo 4&&exit||lscpu|grep -q avx2&&echo 3&&exit||lscpu|grep -q sse4_2&&echo 2&&exit||echo 1)");
                    if let Ok(output) = command.output() {
                        match String::from_utf8_lossy(&output.stdout)
                            .to_string()
                            .as_str()
                            .trim()
                        {
                            "4" | "3" => Arch::X86_64v3,
                            "2" | "1" => Arch::X86_64v1,
                            _ => Arch::NoArch,
                        }
                    } else {
                        Arch::NoArch
                    }
                }
                "aarch64" => Arch::Aarch64,
                "armv7l" => Arch::Armv7l,
                "armv8l" => Arch::Armv8l,
                _ => Arch::NoArch,
            }
        } else {
            Arch::NoArch
        };
        Self {
            locked: false,
            version: env!("SETTINGS_YAML_VERSION").to_string(),
            arch,
            exec: None,
            mirror_list: None,
            sources: Vec::new(),
            disabled_sources: Vec::new(),
        }
    }
    pub fn set_settings(mut self) -> Result<(), String> {
        // Remove duplicate sources before saving
        let mut unique_sources = Vec::new();
        for source in self.sources {
            let is_duplicate = unique_sources.iter().any(|existing| {
                match (existing, &source) {
                    (OriginKind::Pax(existing_url), OriginKind::Pax(new_url)) => existing_url == new_url,
                    (OriginKind::Apt(existing_url), OriginKind::Apt(new_url)) => existing_url == new_url,
                    (OriginKind::Rpm(existing_url), OriginKind::Rpm(new_url)) => existing_url == new_url,
                    (OriginKind::Github { user: eu, repo: er }, OriginKind::Github { user: nu, repo: nr }) => eu == nu && er == nr,
                    _ => false,
                }
            });
            if !is_duplicate {
                unique_sources.push(source);
            }
        }
        self.sources = unique_sources;

        let mut file = match File::create(affirm_path()?) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open SettingsYaml as WO!"),
        };
        let settings = match serde_norway::to_string(&self) {
            Ok(settings) => settings,
            Err(_) => return err!("Failed to parse SettingsYaml to string!"),
        };
        match file.write_all(settings.as_bytes()) {
            Ok(_) => Ok(()),
            Err(_) => err!("Failed to write to file!"),
        }
    }
    pub fn get_settings() -> Result<Self, String> {
        let path = {
            let mut p = get_dir()?;
            p.push("settings.yaml");
            p
        };

        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                // If settings file doesn't exist, return default settings
                return Ok(Self::new());
            }
        };
        let mut data = String::new();
        if file.read_to_string(&mut data).is_err() {
            return err!("Failed to read file!");
        };
        let mut settings: SettingsYaml = match serde_norway::from_str::<SettingsYaml>(&data) {
            Ok(mut settings_yaml) => {
                // Clean URL prefixes from stored repository URLs
                for source in &mut settings_yaml.sources {
                    match source {
                        OriginKind::Rpm(url) | OriginKind::Yum(url) | OriginKind::Apt(url) | OriginKind::Deb(url) => {
                            let cleaned = url
                                .strip_prefix("rpm://")
                                .or_else(|| url.strip_prefix("yum://"))
                                .or_else(|| url.strip_prefix("dnf://"))
                                .or_else(|| url.strip_prefix("apt://"))
                                .or_else(|| url.strip_prefix("deb://"))
                                .map(|s| s.trim_end_matches('/').to_string())
                                .unwrap_or_else(|| url.trim_end_matches('/').to_string());
                            *url = cleaned;
                        }
                        _ => {}
                    }
                }
                settings_yaml
            }
            Err(e) => {
                // If parsing fails, log the error and create fresh settings
                println!("\x1B[93m[WARN] Settings file corrupted ({}). Creating fresh settings...\x1B[0m", e);
                let new_settings = Self::new();
                if let Err(e) = new_settings.clone().set_settings() {
                    return err!("Failed to create new settings file: {}", e);
                }
                new_settings
            }
        };
        let dir = get_dir()?;
        match load_sources_conf(&dir) {
            Ok((mirror, file_sources)) => {
                if mirror.is_some() {
                    settings.mirror_list = mirror;
                }
                if !file_sources.is_empty() {
                    // Validate and clean up sources
                    let mut valid_sources = Vec::new();
                    for source in file_sources {
                        // Skip obviously invalid sources
                        let is_valid = match &source {
                            OriginKind::Pax(url) => {
                                // Skip local URLs that are likely non-existent
                                let should_skip = url.contains("pax.local") ||
                                    url.contains("localhost") ||
                                    url.contains("127.0.0.1") ||
                                    url.is_empty() ||
                                    (!url.starts_with("http://") && !url.starts_with("https://"));
                                !should_skip
                            },
                        OriginKind::Apt(url) | OriginKind::Rpm(url) | OriginKind::Deb(url) | OriginKind::Yum(url) | OriginKind::LocalDir(url) => {
                            // Allow URLs with prefixes (they'll be cleaned when used)
                            let clean_url = url
                                .strip_prefix("rpm://")
                                .or_else(|| url.strip_prefix("yum://"))
                                .or_else(|| url.strip_prefix("dnf://"))
                                .or_else(|| url.strip_prefix("apt://"))
                                .or_else(|| url.strip_prefix("deb://"))
                                .or_else(|| url.strip_prefix("pax://"))
                                .unwrap_or(url);
                            !url.is_empty() && (clean_url.starts_with("http://") || clean_url.starts_with("https://") || clean_url.starts_with("/"))
                        },
                        OriginKind::Github { user, repo } => {
                            !user.is_empty() && !repo.is_empty()
                        },
                        OriginKind::CloudflareR2 { .. } => false, // Skip R2 repos for validation
                    };

                        // Remove duplicates
                        let is_duplicate = valid_sources.iter().any(|existing| {
                            match (existing, &source) {
                                (OriginKind::Pax(existing_url), OriginKind::Pax(new_url)) => existing_url == new_url,
                                (OriginKind::Apt(existing_url), OriginKind::Apt(new_url)) => existing_url == new_url,
                                (OriginKind::Rpm(existing_url), OriginKind::Rpm(new_url)) => existing_url == new_url,
                                (OriginKind::Github { user: eu, repo: er }, OriginKind::Github { user: nu, repo: nr }) => eu == nu && er == nr,
                                _ => false,
                            }
                        });

                        if is_valid && !is_duplicate {
                            valid_sources.push(source);
                        }
                    }
                    settings.sources = valid_sources;
                } else {
                    // No sources configured - use correct Oreon mirror
                    let arch = match settings.arch {
                        Arch::X86_64v1 => "x86_64v1",
                        Arch::X86_64v3 => "x86_64v3",
                        Arch::Aarch64 => "aarch64",
                        _ => "x86_64v3", // default fallback
                    };
                    let oreon_url = format!("https://repo.oreonproject.org/oreon-11/unstable/{}", arch);
                    settings.sources.push(OriginKind::Pax(oreon_url));
                }
                // Deduplicate sources before ensuring required repositories
                let mut deduplicated_sources = Vec::new();
                let mut seen_urls = std::collections::HashSet::new();
                let mut fedora_repos = Vec::new();

                // Separate Fedora repos for special handling
                for source in &settings.sources {
                    match source {
                        OriginKind::Rpm(url) if url.contains("dl.fedoraproject.org") => {
                            fedora_repos.push(source.clone());
                        }
                        _ => {
                            let url = match source {
                                OriginKind::Pax(url) => url.clone(),
                                OriginKind::Apt(url) => url.clone(),
                                OriginKind::Rpm(url) => url.clone(),
                                OriginKind::Deb(url) => url.clone(),
                                OriginKind::Yum(url) => url.clone(),
                                OriginKind::LocalDir(url) => url.clone(),
                                _ => continue, // Skip other types for deduplication
                            };

                            if !seen_urls.contains(&url) {
                                seen_urls.insert(url);
                                deduplicated_sources.push(source.clone());
                            }
                        }
                    }
                }

                // For Fedora repos, ensure both base and updates are available
                if !fedora_repos.is_empty() {
                    let has_updates = fedora_repos.iter().any(|repo| {
                        matches!(repo, OriginKind::Rpm(url) if url.contains("updates"))
                    });
                    let has_valid_base = fedora_repos.iter().any(|repo| {
                        if let OriginKind::Rpm(url) = repo {
                            url.contains("/releases/") && url.contains("/os") && !url.contains("Everything/Everything")
                        } else {
                            false
                        }
                    });

                    // Add all configured Fedora repos (but skip invalid base repos)
                    for repo in &fedora_repos {
                        if let OriginKind::Rpm(url) = repo {
                            // Skip base repos with invalid URLs (Everything/Everything)
                            if url.contains("/releases/") && url.contains("Everything/Everything") {
                                continue;
                            }
                            if !seen_urls.contains(url) {
                                seen_urls.insert(url.clone());
                                deduplicated_sources.push(repo.clone());
                            }
                        }
                    }

                    // If updates is configured but valid base is not, automatically add base
                    if has_updates && !has_valid_base {
                        // Extract version and arch from updates URL
                        // Example: https://dl.fedoraproject.org/pub/fedora/linux/updates/43/Everything/x86_64
                        if let Some(updates_repo) = fedora_repos.iter().find(|repo| {
                            matches!(repo, OriginKind::Rpm(url) if url.contains("updates"))
                        }) {
                            if let OriginKind::Rpm(updates_url) = updates_repo {
                                // Strip rpm:// prefix if present
                                let clean_url = updates_url.strip_prefix("rpm://").unwrap_or(updates_url);
                                // Extract path structure from updates URL and construct base URL dynamically
                                if let Some(updates_start) = clean_url.find("/updates/") {
                                    // Get the base part before /updates/
                                    let base_part = &clean_url[..updates_start];
                                    let after_updates = &clean_url[updates_start + 9..];
                                    // Split the path after /updates/ to get version and rest
                                    let path_parts: Vec<&str> = after_updates.split('/').filter(|s| !s.is_empty()).collect();
                                    if path_parts.len() >= 2 {
                                        let version = path_parts[0];
                                        // Get everything between version and arch (e.g., "Everything")
                                        let middle_path = if path_parts.len() > 2 {
                                            path_parts[1..path_parts.len()-1].join("/") + "/"
                                        } else {
                                            String::new()
                                        };
                                        // Get the last component (arch)
                                        let arch = path_parts[path_parts.len() - 1];
                                        
                                        // Construct base URL by replacing /updates/ with /releases/ and appending /os
                                        let base_url = format!(
                                            "{}/releases/{}/{}{}/os",
                                            base_part, version, middle_path, arch
                                        );
                                        
                                        if !seen_urls.contains(&base_url) {
                                            seen_urls.insert(base_url.clone());
                                            deduplicated_sources.push(OriginKind::Rpm(base_url));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                settings.sources = deduplicated_sources;

                // Ensure only one Oreon repository (prefer the official one)
                let oreon_url_pattern = {
                    let arch = match settings.arch {
                        Arch::X86_64v1 => "x86_64v1",
                        Arch::X86_64v3 => "x86_64v3",
                        Arch::Aarch64 => "aarch64",
                        _ => "x86_64v3", // default fallback
                    };
                    format!("https://repo.oreonproject.org/oreon-11/unstable/{}", arch)
                };

                let has_oreon = settings.sources.iter().any(|source| {
                    matches!(source, OriginKind::Pax(url) if url == &oreon_url_pattern)
                });
                if !has_oreon {
                    settings.sources.push(OriginKind::Pax(oreon_url_pattern.clone()));
                }

                // Remove duplicate Oreon repositories, keeping only the official one
                settings.sources.retain(|source| {
                    !matches!(source, OriginKind::Pax(url) if url.contains("oreon") && url != &oreon_url_pattern)
                });

            }
            Err(fault) => {
                println!(
                    "\x1B[93m[WARN] Unable to load sources config: {}\x1B[0m",
                    fault
                );
                // Start with default repositories
                settings.sources.push(OriginKind::Pax("http://pax.local:8080".to_string()));
                let arch = match settings.arch {
                    Arch::X86_64v1 => "x86_64v1",
                    Arch::X86_64v3 => "x86_64v3",
                    Arch::Aarch64 => "aarch64",
                    _ => "x86_64v3", // default fallback
                };
                let oreon_url = format!("https://mirrors.oreonhq.com/oreon-11/unstable/{}", arch);
                settings.sources.push(OriginKind::Pax(oreon_url));
            }
        }
        Ok(settings)
    }
}

#[derive(PartialEq, Eq, Deserialize, Serialize, Debug, Hash, Clone)]
pub enum OriginKind {
    Apt(String),
    Pax(String),
    Github { user: String, repo: String },
    Rpm(String),
    CloudflareR2 { 
        bucket: String, 
        account_id: String,
        access_key_id: Option<String>,
        secret_access_key: Option<String>,
        region: Option<String>,
    },
    Deb(String),  // Enhanced dpkg/deb support
    Yum(String), // Enhanced dnf/yum support
    LocalDir(String), // Local directory repository
}

impl std::fmt::Display for OriginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OriginKind::Pax(url) => write!(f, "PAX: {}", url),
            OriginKind::Github { user, repo } => write!(f, "GitHub: {}/{}", user, repo),
            OriginKind::Apt(url) => write!(f, "APT: {}", url),
            OriginKind::Rpm(url) => write!(f, "RPM: {}", url),
            OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                write!(f, "Cloudflare R2: {}.{}", bucket, account_id)
            },
            OriginKind::Deb(url) => write!(f, "DEB: {}", url),
            OriginKind::Yum(url) => write!(f, "YUM: {}", url),
            OriginKind::LocalDir(path) => write!(f, "Local: {}", path),
        }
    }
}

#[derive(PartialEq, Serialize, Deserialize, Debug, Clone)]
pub enum Arch {
    NoArch,
    X86_64v1,
    X86_64v3,
    Aarch64,
    Armv7l,
    Armv8l,
}

impl Default for SettingsYaml {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetch mirrors from the Oreon mirror list URL
fn fetch_oreon_mirrors() -> Result<Vec<String>, String> {
    let mirror_list_url = "https://mirrors.oreonhq.com/oreon-11/sources";

    // Create a client with aggressive timeout to avoid hanging
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .connect_timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    match client.get(mirror_list_url).send() {
        Ok(response) => {
            if !response.status().is_success() {
                return err!("Failed to fetch mirror list: HTTP {}", response.status());
            }

            match response.text() {
                Ok(text) => {
                    // The mirror list is a plain text file with one URL per line
                    let mirrors: Vec<String> = text.lines()
                        .map(|line| line.trim())
                        .filter(|line| !line.is_empty() && !line.starts_with('#'))
                        .map(|line| line.replace("$arch", "x86_64v3")) // Replace $arch with detected arch
                        .collect();

                    if mirrors.is_empty() {
                        return err!("No mirrors found in mirror list");
                    }

                    Ok(mirrors)
                }
                Err(e) => err!("Failed to read mirror list response: {}", e),
            }
        }
        Err(e) => err!("Failed to fetch mirror list from {}: {}", mirror_list_url, e),
    }
}

/// Select the best/fastest mirror from the list using parallel testing
fn select_best_mirror(mirrors: &[String]) -> Result<String, String> {
    if mirrors.is_empty() {
        return err!("No mirrors available");
    }

    // #region agent log
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/home/blester/pax-rs/.cursor/debug.log")
        .and_then(|mut file| {
            use std::io::Write;
            writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"mirror-selection\",\"hypothesisId\":\"MULTI_MIRROR\",\"location\":\"settings/src/lib.rs:464\",\"message\":\"selecting_best_mirror\",\"data\":{{\"mirror_count\":{},\"mirrors\":{:?}}},\"timestamp\":{}}}", 
                mirrors.len(), mirrors, 
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
            Ok(())
        });
    // #endregion

    if mirrors.len() == 1 {
        return Ok(mirrors[0].clone());
    }

    // Create a client with aggressive timeout for mirror testing
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .connect_timeout(std::time::Duration::from_millis(500))
        .build() {
        Ok(client) => client,
        Err(_) => return Ok(mirrors[0].clone()), // Fall back to first mirror if client creation fails
    };

    let mut results = Vec::new();

    // Test mirrors in parallel with a limit to avoid overwhelming the network
    let max_concurrent = std::cmp::min(mirrors.len(), 3); // Test up to 3 mirrors concurrently

    for chunk in mirrors.chunks(max_concurrent) {
        let mut handles = Vec::new();

        for mirror in chunk {
            let mirror = mirror.clone();
            let client = client.clone();
            let handle = std::thread::spawn(move || {
                let start = Instant::now();
                // Use HEAD request for faster testing (no body download)
                let test_url = format!("{}/checksums.json", mirror.trim_end_matches('/'));

                match client.head(&test_url).send() {
                    Ok(response) => {
                        if response.status().is_success() {
                            let elapsed = start.elapsed().as_millis();
                            Some((mirror, elapsed))
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            });
            handles.push(handle);
        }

        // Collect results from this batch
        for handle in handles {
            if let Ok(Some((mirror, time))) = handle.join() {
                results.push((mirror, time));
            }
        }

        // If we found a mirror under 500ms, use it immediately
        if let Some((fast_mirror, _)) = results.iter().find(|(_, time)| *time < 500) {
            // #region agent log
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/home/blester/pax-rs/.cursor/debug.log")
                .and_then(|mut file| {
                    use std::io::Write;
                    writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"mirror-selection\",\"hypothesisId\":\"MULTI_MIRROR\",\"location\":\"settings/src/lib.rs:521\",\"message\":\"fast_mirror_found\",\"data\":{{\"selected_mirror\":\"{}\",\"response_time_ms\":{}}},\"timestamp\":{}}}", 
                        fast_mirror, results.iter().find(|(m, _)| m == fast_mirror).map(|(_, t)| t).unwrap_or(&0),
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
                    Ok(())
                });
            // #endregion
            return Ok(fast_mirror.clone());
        }
    }

    // Return the fastest mirror from all results
    if let Some((best_mirror, best_time)) = results.into_iter().min_by_key(|(_, time)| *time) {
        // #region agent log
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/home/blester/pax-rs/.cursor/debug.log")
            .and_then(|mut file| {
                use std::io::Write;
                writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"mirror-selection\",\"hypothesisId\":\"MULTI_MIRROR\",\"location\":\"settings/src/lib.rs:527\",\"message\":\"best_mirror_selected\",\"data\":{{\"selected_mirror\":\"{}\",\"response_time_ms\":{}}},\"timestamp\":{}}}", 
                    best_mirror, best_time,
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
                Ok(())
            });
        // #endregion
        Ok(best_mirror)
    } else {
        // All mirrors failed, fall back to first one
        // #region agent log
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/home/blester/pax-rs/.cursor/debug.log")
            .and_then(|mut file| {
                use std::io::Write;
                writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"mirror-selection\",\"hypothesisId\":\"MULTI_MIRROR\",\"location\":\"settings/src/lib.rs:531\",\"message\":\"all_mirrors_failed_fallback\",\"data\":{{\"fallback_mirror\":\"{}\"}},\"timestamp\":{}}}", 
                    mirrors[0],
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
                Ok(())
            });
        // #endregion
        Ok(mirrors[0].clone())
    }
}

/// Get the best mirror URL, either from configured mirror list or fetch from Oreon
/// Computes fresh each time to handle changing network conditions
pub fn get_best_mirror_url() -> Result<String, String> {
    // First try to get from settings
    if let Ok(settings) = SettingsYaml::get_settings() {
        if let Some(mirror_list_url) = &settings.mirror_list {
            // If we have a configured mirror list URL, fetch mirrors from it
            match reqwest::blocking::get(mirror_list_url) {
                Ok(response) => {
                    if response.status().is_success() {
                        if let Ok(text) = response.text() {
                            // The mirror list is a plain text file with one URL per line
                            let mirrors: Vec<String> = text.lines()
                                .map(|line| line.trim())
                                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                                .map(|line| line.replace("$arch", "x86_64v3")) // Replace $arch with detected arch
                                .collect();

                            if mirrors.is_empty() {
                                return err!("No mirrors found in configured mirror list");
                            }

                            return select_best_mirror(&mirrors);
                        }
                    }
                }
                Err(_) => {} // Fall back to default
            }
        }
    }

    // Fall back to fetching from default Oreon mirror list
    let mirrors = fetch_oreon_mirrors()?;
    select_best_mirror(&mirrors)
}

fn load_sources_conf(dir: &Path) -> Result<(Option<String>, Vec<OriginKind>), String> {
    let path = dir.join("sources.conf");
    if !path.exists() {
        return Ok((None, Vec::new()));
    }
    let contents =
        fs::read_to_string(&path).map_err(|_| format!("Failed to read {}.", path.display()))?;
    let mut mirror = None;
    let mut sources = Vec::new();
    for (idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut entries = Vec::new();
        for part in trimmed.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value
                    .trim_matches(|c| matches!(c, '"' | '\''))
                    .to_string();
                entries.push((key, value));
            }
        }

        let find = |needle: &str| -> Option<&str> {
            entries
                .iter()
                .find(|(key, _)| key == needle)
                .map(|(_, value)| value.as_str())
        };

        let source_type = find("sourcetype")
            .or_else(|| find("type"))
            .map(|s| s.to_lowercase());
        let source_url = find("url").map(|s| s.to_string());
        let provider = find("provider").map(|s| s.to_lowercase());

        match source_type.as_deref() {
            Some("mirror") => {
                if let Some(url) = source_url {
                    if mirror.is_none() {
                        mirror = Some(url);
                    }
                } else {
                    println!(
                        "\x1B[93m[WARN] Mirror entry missing url= on line {} of {}.\x1B[0m",
                        idx + 1,
                        path.display()
                    );
                }
            }
            Some("repo") | Some("repository") => {
                if let Some(url) = source_url {
                    // Strip URL scheme prefixes if present
                    let clean_url: String = if url.starts_with("rpm://") {
                        url[6..].to_string()
                    } else if url.starts_with("yum://") {
                        url[6..].to_string()
                    } else if url.starts_with("dnf://") {
                        url[6..].to_string()
                    } else if url.starts_with("apt://") {
                        url[6..].to_string()
                    } else if url.starts_with("deb://") {
                        url[6..].to_string()
                    } else if url.starts_with("pax://") {
                        url[6..].to_string()
                    } else {
                        url.clone()
                    };
                    
                    if clean_url.starts_with("http://") || clean_url.starts_with("https://") {
                        let origin = match provider.as_deref() {
                            Some("apt") | Some("deb") => OriginKind::Apt(clean_url.clone()),
                            Some("rpm") | Some("yum") | Some("dnf") => OriginKind::Rpm(clean_url.clone()),
                            Some("dpkg") => OriginKind::Deb(clean_url.clone()),
                            Some("cloudflare") | Some("r2") => {
                                // Parse Cloudflare R2 configuration
                                let bucket = find("bucket").unwrap_or("").to_string();
                                let account_id = find("account_id").unwrap_or("").to_string();
                                let access_key_id = find("access_key_id").map(|s| s.to_string());
                                let secret_access_key = find("secret_access_key").map(|s| s.to_string());
                                let region = find("region").map(|s| s.to_string());
                                
                                if bucket.is_empty() || account_id.is_empty() {
                                    println!(
                                        "\x1B[93m[WARN] Cloudflare R2 repository missing required bucket or account_id on line {} of {}.\x1B[0m",
                                        idx + 1,
                                        path.display()
                                    );
                                    continue;
                                }
                                
                                OriginKind::CloudflareR2 {
                                    bucket,
                                    account_id,
                                    access_key_id,
                                    secret_access_key,
                                    region,
                                }
                            },
                            Some("local") | Some("dir") | Some("directory") => {
                                // Check if it's a valid directory
                                let dir_path = Path::new(&clean_url);
                                if dir_path.exists() && dir_path.is_dir() {
                                    OriginKind::LocalDir(url.clone())
                                } else {
                                    println!(
                                        "\x1B[93m[WARN] Local directory repository does not exist: `{}` on line {} of {}.\x1B[0m",
                                        clean_url,
                                        idx + 1,
                                        path.display()
                                    );
                                    continue;
                                }
                            },
                            _ => OriginKind::Pax(clean_url.clone()),
                        };
                        sources.push(origin);
                    } else if url.starts_with("apt://") {
                        sources.push(OriginKind::Apt(
                            url.strip_prefix("apt://").unwrap().to_string(),
                        ));
                    } else if url.starts_with("deb://") {
                        sources.push(OriginKind::Deb(
                            url.strip_prefix("deb://").unwrap().to_string(),
                        ));
                    } else if url.starts_with("yum://") || url.starts_with("dnf://") {
                        sources.push(OriginKind::Yum(
                            url.strip_prefix("yum://").or_else(|| url.strip_prefix("dnf://")).unwrap().to_string(),
                        ));
                    } else if url.starts_with("r2://") {
                        // Parse R2 URL format: r2://bucket.account_id.region
                        let parts: Vec<&str> = url.trim_start_matches("r2://").split('.').collect();
                        if parts.len() >= 2 {
                            let bucket = parts[0].to_string();
                            let account_id = parts[1].to_string();
                            let region = if parts.len() > 2 { Some(parts[2].to_string()) } else { None };
                            
                            sources.push(OriginKind::CloudflareR2 {
                                bucket,
                                account_id,
                                access_key_id: None,
                                secret_access_key: None,
                                region,
                            });
                        } else {
                            println!(
                                "\x1B[93m[WARN] Invalid R2 URL format on line {} of {}: {}\x1B[0m",
                                idx + 1,
                                path.display(),
                                url
                            );
                        }
                    } else if url.starts_with("github://") {
                        if let Some((user, repo)) =
                            url.trim_start_matches("github://").split_once('/')
                        {
                            sources.push(OriginKind::Github {
                                user: user.to_string(),
                                repo: repo.to_string(),
                            });
                        } else {
                            println!(
                                "\x1B[93m[WARN] Invalid GitHub URL `{}` on line {} of {}.\x1B[0m",
                                url,
                                idx + 1,
                                path.display()
                            );
                        }
                    } else if url.starts_with("file://") || url.starts_with("/") || url.starts_with("./") || url.starts_with("../") {
                        // Local directory repository
                        let dir_path = if url.starts_with("file://") {
                            url.strip_prefix("file://").unwrap().to_string()
                        } else {
                            url.to_string()
                        };
                        let dir_path = Path::new(&dir_path);
                        if dir_path.exists() && dir_path.is_dir() {
                            sources.push(OriginKind::LocalDir(dir_path.to_string_lossy().to_string()));
                        } else {
                            println!(
                                "\x1B[93m[WARN] Local directory repository does not exist or is not a directory: `{}` on line {} of {}.\x1B[0m",
                                url,
                                idx + 1,
                                path.display()
                            );
                        }
                    } else {
                        println!(
                            "\x1B[93m[WARN] Unsupported repo URL `{}` on line {} of {}.\x1B[0m",
                            url,
                            idx + 1,
                            path.display()
                        );
                    }
                } else {
                    let github_pair = find("github")
                        .and_then(|value| value.split_once('/'))
                        .map(|(user, repo)| (user.to_string(), repo.to_string()))
                        .or_else(|| {
                            if provider.as_deref() == Some("github") {
                                if let (Some(user), Some(repo)) = (find("user"), find("repo")) {
                                    return Some((user.to_string(), repo.to_string()));
                                }
                            }
                            None
                        });

                    if let Some((user, repo)) = github_pair {
                        sources.push(OriginKind::Github { user, repo });
                    } else {
                        println!(
                            "\x1B[93m[WARN] Repository entry missing url= on line {} of {}.\x1B[0m",
                            idx + 1,
                            path.display()
                        );
                    }
                }
            }
            Some(other) => {
                println!(
                    "\x1B[93m[WARN] Unknown source type `{}` on line {} of {}.\x1B[0m",
                    other,
                    idx + 1,
                    path.display()
                );
            }
            None => {
                println!(
                    "\x1B[93m[WARN] Missing sourcetype= on line {} of {}.\x1B[0m",
                    idx + 1,
                    path.display()
                );
            }
        };
    }
    Ok((mirror, sources))
}

fn affirm_path() -> Result<PathBuf, String> {
    let mut path = get_dir()?;
    path.push("settings.yaml");
    if !path.exists() {
        match File::create(&path) {
            Ok(mut file) => {
                if let Ok(new_settings) = serde_norway::to_string(&SettingsYaml::new()) {
                    if file.write_all(new_settings.as_bytes()).is_ok() {
                        Ok(path)
                    } else {
                        err!("Failed to write to file!")
                    }
                } else {
                    err!("Failed to serialize settings!")
                }
            }
            Err(_) => err!("Failed to create settings file!"),
        }
    } else if path.is_file() {
        if File::open(&path).is_ok() {
            Ok(path)
        } else {
            err!("Failed to read settings file!")
        }
    } else {
        err!("Settings file is of unexpected type!")
    }
}

pub fn acquire_lock() -> Result<Option<PostAction>, String> {
    acquire_lock_with_auto_force(false)
}

pub fn check_root_required(required: bool) -> Option<PostAction> {
    if required && !is_root() {
        Some(PostAction::Elevate)
    } else {
        None
    }
}

pub fn disable_unhealthy_sources() -> Result<(), String> {
    let mut settings = SettingsYaml::get_settings().map_err(|e| format!("Failed to load settings: {}", e))?;

    // Create a test client with very aggressive timeouts
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_millis(500))
        .build() {
        Ok(client) => client,
        Err(_) => return Ok(()), // Can't test, skip
    };

    let mut unhealthy_urls = Vec::new();

    // Test each source
    for source in &settings.sources {
        let test_url = match source {
            OriginKind::Pax(url) => {
                // For PAX repos, test the checksums.json endpoint
                if url.contains("oreon") {
                    format!("{}/checksums.json", url.trim_end_matches('/'))
                } else {
                    // For other repos, just test basic connectivity to base URL
                    url.clone()
                }
            },
            OriginKind::Apt(url) | OriginKind::Rpm(url) | OriginKind::Deb(url) | OriginKind::Yum(url) | OriginKind::LocalDir(url) => url.clone(),
            OriginKind::Github { .. } => continue, // Skip GitHub repos for now
            OriginKind::CloudflareR2 { .. } => continue, // Skip R2 repos for now
        };

        // Skip if already disabled
        if settings.disabled_sources.contains(&test_url) {
            continue;
        }

        // Test the URL
        let is_healthy = match client.head(&test_url).send() {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        };

        if !is_healthy {
            unhealthy_urls.push(test_url);
        }
    }

        // Disable unhealthy sources
        if !unhealthy_urls.is_empty() {
            settings.disabled_sources.extend(unhealthy_urls);
            let disabled_count = settings.disabled_sources.len();
            settings.set_settings()?;
            println!("\x1B[93m[INFO] Disabled {} unresponsive repositories\x1B[0m", disabled_count);
        }

    Ok(())
}

pub fn acquire_lock_with_auto_force(auto_force_unlock: bool) -> Result<Option<PostAction>, String> {
    if !is_root() {
        return Ok(Some(PostAction::Elevate));
    }
    let mut settings = SettingsYaml::get_settings()?;
    let mut attempts = 0;
    const MAX_ATTEMPTS: i32 = 10; // Give up after 10 attempts (50 seconds total)
    let mut user_chose_kill = false;
    
    loop {
        if settings.locked {
            attempts += 1;
            
            // On first attempt, ask if user wants to force unlock immediately (unless auto_force_unlock is true)
            if attempts == 1 && !user_chose_kill {
                if auto_force_unlock {
                    // Auto-force unlock when --yes flag is used
                    println!("\x1B[93m[WARN] Program lock detected. Auto-forcing unlock (--yes flag active).\x1B[0m");
                    let mut tmp_settings = SettingsYaml::get_settings()?;
                    tmp_settings.locked = false;
                    tmp_settings.set_settings()?;
                    settings = SettingsYaml::get_settings()?;
                    user_chose_kill = true;
                    break;
                } else {
                    use utils::choice;
                    match choice("\x1B[93m[WARN] Program lock detected. Force unlock immediately? (y/n)\x1B[0m", false) {
                        Ok(true) => {
                            println!("\x1B[93m[WARN] Forcing unlock (previous instance likely crashed).\x1B[0m");
                            let mut tmp_settings = SettingsYaml::get_settings()?;
                            tmp_settings.locked = false;
                            tmp_settings.set_settings()?;
                            settings = SettingsYaml::get_settings()?;
                            user_chose_kill = true;
                            break;
                        }
                        Ok(false) => {
                            // User chose to wait, continue with normal retry cycle
                            println!("\x1B[93mWaiting for lock to be released...\x1B[0m");
                        }
                        Err(_) => {
                            // Error reading input, continue with normal retry
                            println!("\x1B[93mWaiting for lock to be released...\x1B[0m");
                        }
                    }
                }
            }
            
            if attempts >= MAX_ATTEMPTS {
                // Force unlock and continue - better than hanging forever
                eprintln!("\x1B[93m[WARN] Forcing unlock after timeout (previous instance likely crashed).\x1B[0m");
                let mut tmp_settings = SettingsYaml::get_settings()?;
                tmp_settings.locked = false;
                tmp_settings.set_settings()?;
                break;
            }
            
            // Show retry messages (unless user already chose to kill)
            if !user_chose_kill {
            
                for i in 0..20 {
                    print!(
                        "\x1B[2K\r\x1B[91mAwaiting program lock. Retrying in {:.2}s...\x1B[0m",
                        (100 - i) as f32 / 20f32
                    );
                    let _ = std::io::stdout().flush();
                    sleep(Duration::from_millis(50));
                }
                for i in 0..20 {
                    print!(
                        "\x1B[2K\r\x1B[93mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                        (80 - i) as f32 / 20f32
                    );
                    let _ = std::io::stdout().flush();
                    sleep(Duration::from_millis(50));
                }
                for i in 0..20 {
                    print!(
                        "\x1B[2K\r\x1B[95mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                        (60 - i) as f32 / 20f32
                    );
                    let _ = std::io::stdout().flush();
                    sleep(Duration::from_millis(50));
                }
                for i in 0..20 {
                    print!(
                        "\x1B[2K\r\x1B[94mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                        (40 - i) as f32 / 20f32
                    );
                    let _ = std::io::stdout().flush();
                    sleep(Duration::from_millis(50));
                }
                for i in 0..20 {
                    print!(
                        "\x1B[2K\r\x1B[92mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                        (20 - i) as f32 / 20f32
                    );
                    let _ = std::io::stdout().flush();
                    sleep(Duration::from_millis(50));
                }
                println!("\x1B[2K\r\x1B[92mAwaiting program lock. Retrying now\x1B[0m...");
            }
            settings = SettingsYaml::get_settings()?;
        } else {
            break;
        }
    }
    if settings.sources.is_empty() && settings.mirror_list.is_none() {
        return Ok(Some(PostAction::PullSources));
    }
    settings.locked = true;
    settings.set_settings()?;
    Ok(None)
}

pub fn remove_lock() -> Result<(), String> {
    let mut settings = SettingsYaml::get_settings()?;
    settings.locked = false;
    settings.set_settings()
}

