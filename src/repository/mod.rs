use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Get the system's architecture
pub fn get_system_arch() -> String {
    std::env::consts::ARCH.to_string()
}

// Repository metadata structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryIndex {
    pub packages: Vec<PackageEntry>,
    pub version: String,
    pub last_updated: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub runtime_dependencies: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    pub hash: String,
    pub size: u64,
    pub download_url: String,
    pub signature_url: String,
}

// Repository client for fetching metadata
pub struct RepositoryClient {
    client: Client,
    sources: Vec<String>,
}

impl RepositoryClient {
    // Create new repository client
    pub fn new(sources: Vec<String>) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to create client: {}", e))?;

        // Expand mirrorlists to actual endpoints
        let expanded_sources = Self::expand_mirrorlists(&client, sources);

        Ok(RepositoryClient { client, sources: expanded_sources })
    }
    
    // Expand mirrorlist URLs to actual mirror endpoints
    fn expand_mirrorlists(client: &Client, sources: Vec<String>) -> Vec<String> {
        let mut expanded = Vec::new();
        
        for source in sources {
            if source.contains("mirrorlist") || source.contains("/mirrors") {
                // This is a mirrorlist, fetch the actual mirrors
                if let Ok(response) = client.get(&source).send() {
                    if let Ok(text) = response.text() {
                        let mirrors: Vec<String> = text
                            .lines()
                            .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
                            .map(|line| line.trim().to_string())
                            .collect();
                        
                        if !mirrors.is_empty() {
                            if mirrors.is_empty() {
                                println!("Warning: No mirrors found in mirrorlist");
                            }
                            expanded.extend(mirrors);
                            continue;
                        }
                    }
                }
                // If mirrorlist fetch failed, try using it as direct endpoint anyway
                println!("Warning: Failed to fetch mirrorlist from {}, trying as direct endpoint", source);
            }
            expanded.push(source);
        }
        
        expanded
    }

    // Fetch repository index from a source
    pub fn fetch_index(&self, source_url: &str) -> Result<RepositoryIndex, String> {
        let url = format!("{}/repository/metadata", source_url);
        
        println!("Fetching repository index from {}...", source_url);
        
        let response = self.client.get(&url)
            .send()
            .map_err(|e| format!("Failed to fetch index: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Failed to fetch index: {}", response.status()));
        }

        let text = response.text()
            .map_err(|e| format!("Failed to read response: {}", e))?;
        let index: RepositoryIndex = serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse index: {}", e))?;

        Ok(index)
    }

    // Fetch all repository indexes
    pub fn fetch_all_indexes(&self) -> HashMap<String, RepositoryIndex> {
        let mut indexes = HashMap::new();

        for source in &self.sources {
            match self.fetch_index(source) {
                Ok(index) => {
                    println!("  Found {} packages from {}", index.packages.len(), source);
                    indexes.insert(source.clone(), index);
                }
                Err(e) => {
                    eprintln!("Warning: Failed to fetch from {}: {}", source, e);
                }
            }
        }

        indexes
    }

    // Search for a package across all repositories (tries all repos for fallback)
    pub fn search_package(&self, name: &str) -> Result<Option<(String, PackageEntry)>, String> {
        let indexes = self.fetch_all_indexes();
        let system_arch = get_system_arch();

        // Try each repository in order, fallback to next if not found
        for (source, index) in indexes {
            for mut package in index.packages {
                if package.name == name {
                    // Update download_url to include architecture in format: name-version-arch.pax
                    package.download_url = format!("{}/packages/{}-{}-{}.pax", 
                        source, package.name, package.version, system_arch);
                    package.signature_url = format!("{}/packages/{}-{}-{}.pax.sig", 
                        source, package.name, package.version, system_arch);
                    return Ok(Some((source, package)));
                }
            }
        }

        Ok(None)
    }
    
    // Search for a package with specific version, tries all repos
    pub fn search_package_version(&self, name: &str, version: &str) -> Result<Option<(String, PackageEntry)>, String> {
        let indexes = self.fetch_all_indexes();
        let system_arch = get_system_arch();

        for (source, index) in indexes {
            for mut package in index.packages {
                if package.name == name && package.version == version {
                    // Update download_url to include architecture in format: name-version-arch.pax
                    package.download_url = format!("{}/packages/{}-{}-{}.pax", 
                        source, package.name, package.version, system_arch);
                    package.signature_url = format!("{}/packages/{}-{}-{}.pax.sig", 
                        source, package.name, package.version, system_arch);
                    return Ok(Some((source, package)));
                }
            }
        }
        
        // Fallback: if specific version not found, try any version
        self.search_package(name)
    }

    // Search for packages by pattern
    pub fn search_pattern(&self, pattern: &str) -> HashMap<String, Vec<PackageEntry>> {
        let mut results = HashMap::new();
        let indexes = self.fetch_all_indexes();
        let pattern_lower = pattern.to_lowercase();

        for (source, index) in indexes {
            let mut matches = Vec::new();
            
            for package in index.packages {
                if package.name.to_lowercase().contains(&pattern_lower) ||
                   package.description.to_lowercase().contains(&pattern_lower) {
                    matches.push(package);
                }
            }

            if !matches.is_empty() {
                results.insert(source, matches);
            }
        }

        results
    }

    // Get package metadata
    pub fn get_package_metadata(
        &self,
        source_url: &str,
        package_name: &str,
    ) -> Result<PackageMetadata, String> {
        let url = format!("{}/packages/metadata/{}", source_url, package_name);
        
        let response = self.client.get(&url)
            .send()
            .map_err(|e| format!("Failed to fetch metadata: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Package not found: {}", package_name));
        }

        let text = response.text()
            .map_err(|e| format!("Failed to read response: {}", e))?;
        let metadata: PackageMetadata = serde_json::from_str(&text)
            .map_err(|e| format!("Failed to parse metadata: {}", e))?;

        Ok(metadata)
    }

    // Fetch repository public key
    pub fn fetch_public_key(&self, source_url: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/repository/pubkey", source_url);
        
        let response = self.client.get(&url)
            .send()
            .map_err(|e| format!("Failed to fetch public key: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Failed to fetch public key: {}", response.status()));
        }

        let key_bytes = response.bytes()
            .map_err(|e| format!("Failed to read key: {}", e))?;

        Ok(key_bytes.to_vec())
    }

    // List all available packages
    pub fn list_all_packages(&self) -> Vec<(String, PackageEntry)> {
        let mut all_packages = Vec::new();
        let indexes = self.fetch_all_indexes();

        for (source, index) in indexes {
            for package in index.packages {
                all_packages.push((source.clone(), package));
            }
        }

        all_packages
    }

    // Check for package updates
    pub fn check_updates(
        &self,
        installed: &HashMap<String, String>, // name -> version
    ) -> Vec<(String, String, String)> {
        // returns (name, old_version, new_version)
        let mut updates = Vec::new();
        let indexes = self.fetch_all_indexes();

        for (_, index) in indexes {
            for package in index.packages {
                if let Some(installed_version) = installed.get(&package.name) {
                    // simple version comparison - in production would be more sophisticated
                    if installed_version != &package.version {
                        updates.push((
                            package.name.clone(),
                            installed_version.clone(),
                            package.version.clone(),
                        ));
                    }
                }
            }
        }

        updates
    }
}

// Detailed package metadata (from API)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub origin: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub runtime_dependencies: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    pub build: Option<String>,
    pub install: Option<String>,
    pub uninstall: Option<String>,
    pub hash: String,
    pub binary: Option<String>,
}

// Build repository client from settings
pub fn create_client_from_settings(
    settings: &crate::SettingsYaml,
) -> Result<RepositoryClient, String> {
    if settings.sources.is_empty() {
        return Err("No repository sources configured. Run 'pax pax-init --force' to initialize.".to_string());
    }

    RepositoryClient::new(settings.sources.clone())
}

