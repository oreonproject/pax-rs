use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fs::{File, read_dir, create_dir_all};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const TRUSTED_KEYS_DIR: &str = "/etc/pax/trusted-keys";

// Calculate SHA256 hash of a file
pub fn calculate_sha256<P: AsRef<Path>>(path: P) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hex::encode(hasher.finalize()))
}

// Verify file matches expected hash
pub fn verify_sha256<P: AsRef<Path>>(path: P, expected_hash: &str) -> Result<bool, String> {
    let actual_hash = calculate_sha256(&path)
        .map_err(|e| format!("Failed to calculate hash: {}", e))?;
    
    Ok(actual_hash.to_lowercase() == expected_hash.to_lowercase())
}

// Verify Ed25519 signature for a file
pub fn verify_signature<P: AsRef<Path>>(
    file_path: P,
    signature_path: P,
    public_key_bytes: &[u8; 32],
) -> Result<bool, String> {
    // Read the file contents
    let mut file = File::open(file_path.as_ref())
        .map_err(|e| format!("Failed to open file: {}", e))?;
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    // Read signature
    let mut sig_file = File::open(signature_path.as_ref())
        .map_err(|e| format!("Failed to open signature file: {}", e))?;
    let mut sig_bytes = Vec::new();
    sig_file.read_to_end(&mut sig_bytes)
        .map_err(|e| format!("Failed to read signature: {}", e))?;

    // Parse signature
    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|e| format!("Invalid signature format: {}", e))?;

    // Parse public key
    let public_key = VerifyingKey::from_bytes(public_key_bytes)
        .map_err(|e| format!("Invalid public key: {}", e))?;

    // Verify signature
    match public_key.verify(&contents, &signature) {
        Ok(()) => Ok(true),
        Err(_) => Ok(false),
    }
}

// Try to verify signature against any trusted key
pub fn verify_with_trusted_keys<P: AsRef<Path>>(
    file_path: P,
    signature_path: P,
) -> Result<bool, String> {
    let keys = load_trusted_keys()?;
    
    if keys.is_empty() {
        return Err("No trusted keys found. Add repository keys with 'pax trust add'".to_string());
    }

    // try each key until one works
    for (key_name, key_bytes) in keys {
        match verify_signature(&file_path, &signature_path, &key_bytes) {
            Ok(true) => {
                println!("Verified with key: {}", key_name);
                return Ok(true);
            }
            Ok(false) => continue,
            Err(_) => continue,
        }
    }

    Ok(false)
}

// Load all trusted public keys from the trust store
pub fn load_trusted_keys() -> Result<Vec<(String, [u8; 32])>, String> {
    let keys_dir = Path::new(TRUSTED_KEYS_DIR);
    
    if !keys_dir.exists() {
        create_dir_all(keys_dir)
            .map_err(|e| format!("Failed to create keys directory: {}", e))?;
        return Ok(Vec::new());
    }

    let mut keys = Vec::new();
    let entries = read_dir(keys_dir)
        .map_err(|e| format!("Failed to read keys directory: {}", e))?;

    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read dir entry: {}", e))?;
        let path = entry.path();
        
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("pub") {
            let key_name = path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            match load_public_key(&path) {
                Ok(key_bytes) => keys.push((key_name, key_bytes)),
                Err(e) => eprintln!("Warning: Failed to load key {}: {}", path.display(), e),
            }
        }
    }

    Ok(keys)
}

// Load a single public key from file
pub fn load_public_key<P: AsRef<Path>>(path: P) -> Result<[u8; 32], String> {
    let mut file = File::open(path.as_ref())
        .map_err(|e| format!("Failed to open key file: {}", e))?;
    
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|e| format!("Failed to read key file: {}", e))?;

    // Remove whitespace and newlines
    let contents = contents.trim();
    
    // Try to decode as hex
    let key_bytes = hex::decode(contents)
        .map_err(|e| format!("Invalid hex encoding: {}", e))?;

    if key_bytes.len() != 32 {
        return Err(format!("Invalid key length: expected 32 bytes, got {}", key_bytes.len()));
    }

    let mut key_array = [0u8; 32];
    key_array.copy_from_slice(&key_bytes);
    Ok(key_array)
}

// Add a trusted public key
pub fn add_trusted_key(key_name: &str, key_bytes: &[u8; 32]) -> Result<(), String> {
    let keys_dir = Path::new(TRUSTED_KEYS_DIR);
    create_dir_all(keys_dir)
        .map_err(|e| format!("Failed to create keys directory: {}", e))?;

    let key_path = keys_dir.join(format!("{}.pub", key_name));
    
    if key_path.exists() {
        return Err(format!("Key '{}' already exists", key_name));
    }

    std::fs::write(&key_path, hex::encode(key_bytes))
        .map_err(|e| format!("Failed to write key file: {}", e))?;

    Ok(())
}

// Remove a trusted key
pub fn remove_trusted_key(key_name: &str) -> Result<(), String> {
    let key_path = PathBuf::from(TRUSTED_KEYS_DIR).join(format!("{}.pub", key_name));
    
    if !key_path.exists() {
        return Err(format!("Key '{}' not found", key_name));
    }

    std::fs::remove_file(&key_path)
        .map_err(|e| format!("Failed to remove key: {}", e))?;

    Ok(())
}

// List all trusted keys
pub fn list_trusted_keys() -> Result<Vec<String>, String> {
    let keys = load_trusted_keys()?;
    Ok(keys.into_iter().map(|(name, _)| name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_sha256_calculation() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test content").unwrap();
        
        let hash = calculate_sha256(temp_file.path()).unwrap();
        // SHA256 of "test content"
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn test_sha256_verification() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test content").unwrap();
        
        let hash = calculate_sha256(temp_file.path()).unwrap();
        assert!(verify_sha256(temp_file.path(), &hash).unwrap());
        assert!(!verify_sha256(temp_file.path(), "wrong_hash").unwrap());
    }
}

