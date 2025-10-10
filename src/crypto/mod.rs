use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

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

