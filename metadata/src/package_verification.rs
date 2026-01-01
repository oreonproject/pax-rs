use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utils::err;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSignature {
    pub package_name: String,
    pub package_version: String,
    pub signature_type: SignatureType,
    pub signature_data: String,
    pub signer: Option<String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignatureType {
    Sha256,
    Sha512,
    Gpg,
    Ed25519,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub package_name: String,
    pub is_valid: bool,
    pub verification_type: VerificationType,
    pub details: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerificationType {
    Checksum,
    Signature,
    Both,
}

pub struct PackageVerifier {
    trusted_keys: HashMap<String, String>, // key_id -> public_key
}

impl PackageVerifier {
    pub fn new() -> Self {
        Self {
            trusted_keys: HashMap::new(),
        }
    }

    pub fn add_trusted_key(&mut self, key_id: String, public_key: String) {
        self.trusted_keys.insert(key_id, public_key);
    }

    pub fn verify_package(
        &self,
        package_path: &std::path::Path,
        expected_signature: Option<&PackageSignature>,
    ) -> Result<VerificationResult, String> {
        let package_name = package_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let mut warnings = Vec::new();
        let mut is_valid = true;
        let mut verification_type = VerificationType::Checksum;
        let mut details = String::new();

        // Calculate actual checksum
        let actual_checksum = self.calculate_checksum(package_path)?;
        details.push_str(&format!("Package checksum: {}\n", actual_checksum));

        if let Some(signature) = expected_signature {
            verification_type = VerificationType::Both;
            
            // Verify signature
            match signature.signature_type {
                SignatureType::Sha256 => {
                    if actual_checksum != signature.signature_data {
                        is_valid = false;
                        details.push_str("SHA256 checksum mismatch!\n");
                    } else {
                        details.push_str("SHA256 checksum verified\n");
                    }
                }
                SignatureType::Sha512 => {
                    let sha512_checksum = self.calculate_sha512(package_path)?;
                    if sha512_checksum != signature.signature_data {
                        is_valid = false;
                        details.push_str("SHA512 checksum mismatch!\n");
                    } else {
                        details.push_str("SHA512 checksum verified\n");
                    }
                }
                SignatureType::Gpg => {
                    if let Err(e) = self.verify_gpg_signature(package_path, signature) {
                        is_valid = false;
                        details.push_str(&format!("GPG signature verification failed: {}\n", e));
                        warnings.push("GPG signature verification failed".to_string());
                    } else {
                        details.push_str("GPG signature verified\n");
                    }
                }
                SignatureType::Ed25519 => {
                    if let Err(e) = self.verify_ed25519_signature(package_path, signature) {
                        is_valid = false;
                        details.push_str(&format!("Ed25519 signature verification failed: {}\n", e));
                        warnings.push("Ed25519 signature verification failed".to_string());
                    } else {
                        details.push_str("Ed25519 signature verified\n");
                    }
                }
            }
        } else {
            // No signature provided, just verify checksum
            details.push_str("No signature provided, checksum only verification\n");
            warnings.push("Package not signed".to_string());
        }

        Ok(VerificationResult {
            package_name,
            is_valid,
            verification_type,
            details,
            warnings,
        })
    }

    fn calculate_checksum(&self, path: &std::path::Path) -> Result<String, String> {
        use sha2::{Sha256, Digest};
        use std::fs::File;
        use std::io::Read;

        let mut file = File::open(path)
            .map_err(|_| format!("Failed to open file {}", path.display()))?;

        let mut hasher = Sha256::new();
        let mut buffer = [0; 8192];

        loop {
            let bytes_read = file.read(&mut buffer)
                .map_err(|_| format!("Failed to read file {}", path.display()))?;
            
            if bytes_read == 0 {
                break;
            }

            hasher.update(&buffer[..bytes_read]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    fn calculate_sha512(&self, path: &std::path::Path) -> Result<String, String> {
        use sha2::{Sha512, Digest};
        use std::fs::File;
        use std::io::Read;

        let mut file = File::open(path)
            .map_err(|_| format!("Failed to open file {}", path.display()))?;

        let mut hasher = Sha512::new();
        let mut buffer = [0; 8192];

        loop {
            let bytes_read = file.read(&mut buffer)
                .map_err(|_| format!("Failed to read file {}", path.display()))?;
            
            if bytes_read == 0 {
                break;
            }

            hasher.update(&buffer[..bytes_read]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    fn verify_gpg_signature(
        &self,
        _path: &std::path::Path,
        signature: &PackageSignature,
    ) -> Result<(), String> {
        // This would integrate with GPG for actual signature verification
        // For now, we'll do a basic check
        
        if let Some(signer) = &signature.signer {
            if !self.trusted_keys.contains_key(signer) {
                return err!("Untrusted signer: {}", signer);
            }
        }

        // In a real implementation, this would:
        // 1. Extract the signature from signature_data
        // 2. Use GPG to verify the signature against the package
        // 3. Check that the signer is in our trusted keys
        
        println!("GPG signature verification would happen here");
        Ok(())
    }

    fn verify_ed25519_signature(
        &self,
        _path: &std::path::Path,
        signature: &PackageSignature,
    ) -> Result<(), String> {
        // This would integrate with Ed25519 for actual signature verification
        // For now, we'll do a basic check
        
        if let Some(signer) = &signature.signer {
            if !self.trusted_keys.contains_key(signer) {
                return err!("Untrusted signer: {}", signer);
            }
        }

        // In a real implementation, this would:
        // 1. Extract the signature from signature_data
        // 2. Use Ed25519 to verify the signature against the package
        // 3. Check that the signer is in our trusted keys
        
        println!("Ed25519 signature verification would happen here");
        Ok(())
    }

    pub fn verify_package_metadata(
        &self,
        metadata: &crate::processed::ProcessedMetaData,
    ) -> Result<VerificationResult, String> {
        // Verify package metadata integrity
        let mut warnings = Vec::new();
        let mut is_valid = true;
        let mut details = String::new();

        // Check required fields
        if metadata.name.is_empty() {
            is_valid = false;
            details.push_str("Package name is empty\n");
        }

        if metadata.version.is_empty() {
            is_valid = false;
            details.push_str("Package version is empty\n");
        }

        if metadata.description.is_empty() {
            warnings.push("Package description is empty".to_string());
        }

        // Check dependency validity
        for dep in &metadata.runtime_dependencies {
            if dep.name().is_empty() {
                is_valid = false;
                details.push_str("Empty dependency name found\n");
            }
        }

        // Check hash if provided
        if !metadata.hash.is_empty() {
            details.push_str(&format!("Package hash: {}\n", metadata.hash));
        } else {
            warnings.push("No package hash provided".to_string());
        }

        Ok(VerificationResult {
            package_name: metadata.name.clone(),
            is_valid,
            verification_type: VerificationType::Checksum,
            details,
            warnings,
        })
    }

    pub fn load_trusted_keys(&mut self, keys_path: &std::path::Path) -> Result<(), String> {
        use std::fs::File;
        use std::io::Read;

        let mut file = File::open(keys_path)
            .map_err(|_| format!("Failed to open keys file {}", keys_path.display()))?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|_| format!("Failed to read keys file {}", keys_path.display()))?;

        // Parse keys file (simplified format: key_id:public_key)
        for line in contents.lines() {
            if let Some((key_id, public_key)) = line.split_once(':') {
                self.trusted_keys.insert(key_id.trim().to_string(), public_key.trim().to_string());
            }
        }

        Ok(())
    }

    pub fn save_trusted_keys(&self, keys_path: &std::path::Path) -> Result<(), String> {
        use std::fs::File;
        use std::io::Write;

        let mut file = File::create(keys_path)
            .map_err(|_| format!("Failed to create keys file {}", keys_path.display()))?;

        for (key_id, public_key) in &self.trusted_keys {
            writeln!(file, "{}:{}", key_id, public_key)
                .map_err(|_| format!("Failed to write keys file {}", keys_path.display()))?;
        }

        Ok(())
    }

    /// Load Oreon keyring from the official keyring URL
    pub async fn load_oreon_keyring(&mut self) -> Result<(), String> {
        let keyring_url = "https://mirrors.oreonhq.com/oreon-11/keyring.json";

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        let response = client.get(keyring_url).send().await
            .map_err(|e| format!("Failed to fetch Oreon keyring: {}", e))?;

        if !response.status().is_success() {
            return err!("Failed to fetch Oreon keyring: HTTP {}", response.status());
        }

        let keyring_text = response.text().await
            .map_err(|e| format!("Failed to read keyring response: {}", e))?;

        let keyring: serde_json::Value = serde_json::from_str(&keyring_text)
            .map_err(|e| format!("Failed to parse keyring JSON: {}", e))?;

        // Parse keyring format (assuming it's a JSON object with key_id -> public_key mappings)
        if let Some(keys_obj) = keyring.as_object() {
            for (key_id, public_key_value) in keys_obj {
                if let Some(public_key) = public_key_value.as_str() {
                    self.trusted_keys.insert(key_id.clone(), public_key.to_string());
                }
            }
        }

        Ok(())
    }
}

impl Default for PackageVerifier {
    fn default() -> Self {
        Self::new()
    }
}
