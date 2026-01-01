use reqwest::Client;
use serde::{Deserialize, Serialize};
use settings::OriginKind;
use utils::err;
use futures::StreamExt;
use async_compression::tokio::bufread::GzipDecoder;
use tokio_util::io::StreamReader;
use tokio::io::AsyncBufReadExt;

#[derive(Debug, Clone)]
pub struct YumRepositoryClient {
    base_url: String,
    client: Client,
}

impl YumRepositoryClient {
    pub fn new(base_url: String) -> Self {
        // Clean URL prefixes if present
        let mut clean_url = base_url
            .strip_prefix("rpm://")
            .or_else(|| base_url.strip_prefix("yum://"))
            .or_else(|| base_url.strip_prefix("dnf://"))
            .map(|s| s.to_string())
            .unwrap_or(base_url);
        
        // Ensure URL doesn't end with a trailing slash (we add paths with leading slashes)
        clean_url = clean_url.trim_end_matches('/').to_string();
        
        Self {
            base_url: clean_url,
            client: Client::new(),
        }
    }

    pub fn from_origin(origin: &OriginKind) -> Option<Self> {
        match origin {
            OriginKind::Yum(url) | OriginKind::Rpm(url) => Some(Self::new(url.clone())),
            _ => None,
        }
    }

    pub async fn list_packages(&self) -> Result<Vec<YumPackageInfo>, String> {
        // First, get the repomd.xml to find the correct primary.xml filename
        let repomd_url = format!("{}/repodata/repomd.xml", self.base_url);
        let repomd_response = self.client.get(&repomd_url).send().await
            .map_err(|e| format!("Failed to fetch repomd.xml: {}", e))?;

        if !repomd_response.status().is_success() {
            return err!("Failed to fetch repomd.xml: {}", repomd_response.status());
        }

        let repomd_content = repomd_response.text().await
            .map_err(|e| format!("Failed to read repomd.xml: {}", e))?;

        // Parse repomd.xml to find the primary.xml.gz filename
        let primary_filename = self.parse_repomd_for_primary(&repomd_content)?;
        let primary_url = format!("{}/{}", self.base_url, primary_filename);
        
        let response = self.client.get(&primary_url).send().await
            .map_err(|e| format!("Failed to fetch package list: {}", e))?;

        if !response.status().is_success() {
            return err!("Failed to fetch package list: {}", response.status());
        }

        let bytes = response.bytes().await
            .map_err(|e| format!("Failed to read package list: {}", e))?;

        // Check compression type and decompress if needed
        let packages_content = if primary_url.ends_with(".gz") {
            self.decompress_gzip_bytes(&bytes)?
        } else if primary_url.ends_with(".zst") {
            self.decompress_zstd_bytes(&bytes)?
        } else {
            String::from_utf8(bytes.to_vec())
                .map_err(|e| format!("Failed to convert bytes to string: {}", e))?
        };

        // Show parsing message
        print!("\rParsing packages... [                    ] 0 packages");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        
        let result = self.parse_primary_xml(&packages_content);
        
        // Clear progress line
        print!("\r                                           \r");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        
        result
    }

    pub async fn get_package(&self, package_name: &str, version: Option<&str>) -> Result<YumPackageInfo, String> {
        self.get_package_inner(package_name, version).await
    }

    async fn get_package_inner(&self, package_name: &str, version: Option<&str>) -> Result<YumPackageInfo, String> {
        // Optimized: stream parse XML and stop when we find the package
        // This avoids downloading/parsing the entire metadata file

        // First, get the repomd.xml to find the correct primary.xml filename
        let repomd_url = format!("{}/repodata/repomd.xml", self.base_url);
        let repomd_response = self.client.get(&repomd_url).send().await
            .map_err(|e| format!("Failed to fetch repomd.xml: {}", e))?;

        if !repomd_response.status().is_success() {
            return err!("Failed to fetch repomd.xml: {}", repomd_response.status());
        }

        let repomd_content = repomd_response.text().await
            .map_err(|e| format!("Failed to read repomd.xml: {}", e))?;

        // Parse repomd.xml to find the primary.xml.gz filename
        let primary_filename = self.parse_repomd_for_primary(&repomd_content)?;
        let primary_url = format!("{}/{}", self.base_url, primary_filename);

        // Stream the response and parse incrementally - stop as soon as we find the package
        let response = self.client.get(&primary_url).send().await
            .map_err(|e| format!("Failed to fetch package list: {}", e))?;

        if !response.status().is_success() {
            return err!("Failed to fetch package list: {}", response.status());
        }

        // Convert response stream to async reader
        let stream = response.bytes_stream()
            .map(|result| result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
        let reader = StreamReader::new(stream);

        // Decompress if compressed
        let mut reader: Box<dyn tokio::io::AsyncBufRead + Unpin + Send> = if primary_url.ends_with(".gz") {
            Box::new(tokio::io::BufReader::new(GzipDecoder::new(reader)))
        } else if primary_url.ends_with(".zst") {
            Box::new(tokio::io::BufReader::new(async_compression::tokio::bufread::ZstdDecoder::new(reader)))
        } else {
            Box::new(tokio::io::BufReader::new(reader))
        };

        // Use a faster approach: scan for package name first, then parse the full package
        let mut package_xml = String::new();
        let mut in_target_package = false;

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let trimmed = line.trim();

                    // Look for package start
                    if trimmed.starts_with("<package") && (trimmed.contains("type=\"rpm\"") || trimmed.contains("type='rpm'")) {
                        in_target_package = false;
                        package_xml.clear();
                        package_xml.push_str(trimmed);
                        package_xml.push('\n');
                    } else if trimmed.starts_with("<name>") && trimmed.ends_with("</name>") {
                        // Check if this is our target package
                        if let Some(name) = trimmed.strip_prefix("<name>").and_then(|s| s.strip_suffix("</name>")) {
                            if name.eq_ignore_ascii_case(package_name) {
                                in_target_package = true;
                            }
                        }
                        if in_target_package {
                            package_xml.push_str(trimmed);
                            package_xml.push('\n');
                        }
                    } else if in_target_package {
                        package_xml.push_str(trimmed);
                        package_xml.push('\n');

                        // Check for package end
                        if trimmed == "</package>" || trimmed.ends_with("</package>") {
                            // Parse the complete package XML
                            match self.parse_single_package(&package_xml) {
                                Ok(Some(pkg_info)) => {
                                    // Double-check name match
                                    if pkg_info.name.eq_ignore_ascii_case(package_name) {
                                        // Check version if specified
                                        if let Some(ver) = version {
                                            if pkg_info.version == ver {
                                                print!("\r                                             \r");
                                                std::io::Write::flush(&mut std::io::stdout()).ok();
                                                return Ok(pkg_info);
                                            }
                                        } else {
                                            print!("\r                                             \r");
                                            std::io::Write::flush(&mut std::io::stdout()).ok();
                                            return Ok(pkg_info);
                                        }
                                    }
                                }
                                Ok(None) | Err(_) => {
                                    // Parse failed, continue searching
                                    in_target_package = false;
                                }
                            }
                            package_xml.clear();
                        }
                    }
                }
                Err(e) => return Err(format!("Failed to read package list: {}", e)),
            }
        }

        print!("\r                                             \r");
        std::io::Write::flush(&mut std::io::stdout()).ok();

        Err(format!("Package {} not found", package_name))
    }

    pub async fn download_package(&self, package_info: &YumPackageInfo) -> Result<Vec<u8>, String> {
        let response = self.client
            .get(&package_info.url)
            .send()
            .await
            .map_err(|e| format!("Failed to download package: {}", e))?;

        if !response.status().is_success() {
            return err!("Failed to download package: {}", response.status());
        }

        let bytes = response.bytes().await
            .map_err(|e| format!("Failed to read package data: {}", e))?;

        Ok(bytes.to_vec())
    }

    pub fn parse_repomd_for_primary(&self, repomd_xml: &str) -> Result<String, String> {
        // Simple XML parsing to find the primary.xml.gz filename
        let mut in_primary_data = false;
        for line in repomd_xml.lines() {
            let line = line.trim();
            if line.contains("type=\"primary\"") {
                in_primary_data = true;
            } else if in_primary_data && line.contains("<location href=") {
                // Look for href attribute
                if let Some(href_start) = line.find("href=\"") {
                    if let Some(href_end) = line[href_start + 6..].find("\"") {
                        let filename = &line[href_start + 6..href_start + 6 + href_end];
                        return Ok(filename.to_string());
                    }
                }
            } else if in_primary_data && line.contains("</data>") {
                break;
            }
        }
        err!("Could not find primary.xml.gz filename in repomd.xml")
    }

    fn parse_primary_xml(&self, xml: &str) -> Result<Vec<YumPackageInfo>, String> {
        let mut packages = Vec::new();
        
        // Start animated progress bar
        let bar_width = 20;
        let mut position = 0i32;
        let mut direction = 1i32;
        let mut frame_counter = 0;
        
        // Split by package blocks
        let package_blocks: Vec<&str> = xml.split("<package type=\"rpm\">").collect();
        
        for block in package_blocks.iter().skip(1) { // Skip first empty block
            if let Some(package_end) = block.find("</package>") {
                let package_xml = &block[..package_end];
                if let Some(package_info) = self.parse_single_package(package_xml)? {
                    packages.push(package_info);
                    
                    // Update animation every package
                    frame_counter += 1;
                    if frame_counter % 5 == 0 {
                        let bar = self.generate_bar(position, bar_width);
                        print!("\rParsing packages... [{}] {} packages", bar, packages.len());
                        std::io::Write::flush(&mut std::io::stdout()).ok();
                        
                        // Update position with ping-pong effect
                        position += direction;
                        if position >= bar_width as i32 - 1 {
                            direction = -1;
                        } else if position <= 0 {
                            direction = 1;
                        }
                    }
                }
            }
        }
        
        Ok(packages)
    }
    
    fn generate_bar(&self, position: i32, width: usize) -> String {
        let pos = position.max(0).min((width - 1) as i32) as usize;
        let mut bar = vec![' '; width];
        bar[pos] = '#';
        bar.iter().collect()
    }
    
    fn parse_single_package(&self, package_xml: &str) -> Result<Option<YumPackageInfo>, String> {
        // Simple regex-based parsing
        let mut name = None;
        let mut version = None;
        let mut release = None;
        let mut arch = None;
        let mut summary = None;
        let mut description = None;
        let mut location = None;
        let mut dependencies = Vec::new();
        let mut provides = Vec::new();
        let mut in_provides = false;
        
        for line in package_xml.lines() {
            let line = line.trim();

            // Track if we're inside a provides section
            if line.contains("<rpm:provides>") {
                in_provides = true;
                continue;
            } else if line.contains("</rpm:provides>") {
                in_provides = false;
                continue;
            }
            
            // Extract package info
            if line.starts_with("<name>") && line.ends_with("</name>") {
                name = Some(line[6..line.len()-7].trim().to_string());
            } else if line.starts_with("<arch>") && line.ends_with("</arch>") {
                arch = Some(line[6..line.len()-7].to_string());
            } else if line.starts_with("<summary>") && line.ends_with("</summary>") {
                summary = Some(line[9..line.len()-10].to_string());
            } else if line.starts_with("<description>") && line.ends_with("</description>") {
                description = Some(line[12..line.len()-13].to_string());
            } else if line.contains("href=\"") {
                if let Some(start) = line.find("href=\"") {
                    if let Some(end) = line[start+6..].find("\"") {
                        location = Some(line[start+6..start+6+end].to_string());
                    }
                }
            } else if line.starts_with("<version ") {
                if let Some(ver_start) = line.find("ver=\"") {
                    if let Some(ver_end) = line[ver_start+5..].find("\"") {
                        version = Some(line[ver_start+5..ver_start+5+ver_end].to_string());
                    }
                }
                if let Some(rel_start) = line.find("rel=\"") {
                    if let Some(rel_end) = line[rel_start+5..].find("\"") {
                        release = Some(line[rel_start+5..rel_start+5+rel_end].to_string());
                    }
                }
            } else if line.contains("<rpm:entry") && line.contains("name=\"") {
                if let Some(start) = line.find("name=\"") {
                    if let Some(end) = line[start+6..].find("\"") {
                        let entry_name = &line[start+6..start+6+end];
                        
                        if in_provides {
                            // This is a provides entry - extract the name
                            let clean_provide = if let Some(paren_start) = entry_name.find('(') {
                                entry_name[..paren_start].trim()
                            } else if let Some(op_start) = entry_name.find(|c: char| c == '>' || c == '<' || c == '=' || c == ' ') {
                                entry_name[..op_start].trim()
                            } else {
                                entry_name.trim()
                            };
                            
                            if !clean_provide.is_empty()
                                && !clean_provide.starts_with("rpmlib(")
                                && !clean_provide.ends_with(".so")
                                && !clean_provide.starts_with('/')
                                && !provides.iter().any(|p| p == clean_provide)
                            {
                                provides.push(clean_provide.to_string());
                            }
                        } else {
                            // This is a dependency entry
                            // Skip rpmlib dependencies and filesystem
                            if !entry_name.is_empty()
                                && !entry_name.starts_with("rpmlib(")
                                && !entry_name.contains("filesystem")
                                && !entry_name.starts_with("/bin/")
                                && !entry_name.starts_with("/usr/bin/")
                                && !entry_name.starts_with("/sbin/")
                            {
                                // Extract just the package name, handling version constraints and ABI specs
                                let clean_name = if let Some(paren_start) = entry_name.find('(') {
                                    // Handle cases like "python(abi) = 3.14" -> "python"
                                    entry_name[..paren_start].trim()
                                } else if let Some(op_start) = entry_name.find(|c: char| c == '>' || c == '<' || c == '=' || c == ' ') {
                                    // Handle version constraints like "package >= 1.0" -> "package"
                                    entry_name[..op_start].trim()
                                } else {
                                    entry_name.trim()
                                };

                                // Skip if it's already in dependencies and filter out non-package dependencies
                                // Use pattern-based filtering to skip virtual packages (no hardcoding)
                                let name_lower = clean_name.to_lowercase();
                                let has_separators = name_lower.contains('-') || name_lower.contains('_') || name_lower.contains('.');
                                let has_numbers = name_lower.chars().any(|c| c.is_ascii_digit());
                                let is_single_word = !name_lower.contains(' ') && !has_separators;
                                let is_short = name_lower.len() <= 6;
                                let is_likely_virtual = is_single_word && is_short && !has_numbers;
                                
                                if !clean_name.is_empty()
                                    && !dependencies.iter().any(|d| d == clean_name)
                                    && !clean_name.ends_with(".so")  // Skip library sonames
                                    && !clean_name.ends_with(".so.0")  // Skip versioned library sonames
                                    && !clean_name.starts_with('/')  // Skip file paths
                                    && !is_likely_virtual  // Skip virtual packages (pattern-based, no hardcoding)
                                {
                                    dependencies.push(clean_name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        
        if let (Some(name), Some(version), Some(release), Some(arch)) = (name, version, release, arch) {
            let full_version = format!("{}-{}", version, release);
            let url = location.map(|loc| format!("{}/{}", self.base_url, loc)).unwrap_or_default();
            
            // Silently parse dependencies without spamming output
            
            Ok(Some(YumPackageInfo {
                name,
                version: full_version,
                description: description.unwrap_or(summary.unwrap_or_default()),
                size: 0,
                url,
                dependencies,
                provides,
                architecture: arch,
                release,
                epoch: "0".to_string(),
            }))
        } else {
            Ok(None)
        }
    }

    fn decompress_gzip_bytes(&self, bytes: &[u8]) -> Result<String, String> {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let mut decoder = GzDecoder::new(bytes);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed)
            .map_err(|e| format!("Failed to decompress gzip: {}", e))?;

        Ok(decompressed)
    }

    fn decompress_zstd_bytes(&self, bytes: &[u8]) -> Result<String, String> {
        use std::io::Read;
        use zstd::Decoder;

        let mut decoder = Decoder::new(bytes)
            .map_err(|e| format!("Failed to create zstd decoder: {}", e))?;
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed)
            .map_err(|e| format!("Failed to decompress zstd: {}", e))?;

        Ok(decompressed)
    }

    #[allow(dead_code)]
    fn decompress_gzip(&self, data: &str) -> Result<String, String> {
        use flate2::read::GzDecoder;
        use std::io::Read;
        
        let bytes = data.as_bytes();
        let mut decoder = GzDecoder::new(bytes);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed)
            .map_err(|e| format!("Failed to decompress gzip: {}", e))?;
        
        Ok(decompressed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YumPackageInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub size: u64,
    pub url: String,
    pub dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub architecture: String,
    pub release: String,
    pub epoch: String,
}

pub async fn test_yum_connection(origin: &OriginKind) -> Result<bool, String> {
    let client = match YumRepositoryClient::from_origin(origin) {
        Some(client) => client,
        None => return Ok(false),
    };

    // Try to list packages to test connection
    match client.list_packages().await {
        Ok(_) => Ok(true),
        Err(e) => {
            println!("Yum repository connection test failed: {}", e);
            Ok(false)
        }
    }
}
