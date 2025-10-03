use crate::crypto;
use std::path::Path;

// Package verification result
#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub hash_valid: bool,
    pub signature_valid: bool,
    pub errors: Vec<String>,
}

impl VerificationResult {
    // Check if verification passed
    pub fn is_valid(&self) -> bool {
        self.hash_valid && self.signature_valid && self.errors.is_empty()
    }

    // Get detailed error message
    pub fn error_message(&self) -> String {
        if self.is_valid() {
            return "Package verification passed".to_string();
        }

        let mut msg = String::from("Package verification failed:\n");
        
        if !self.hash_valid {
            msg.push_str("  - Hash mismatch\n");
        }
        
        if !self.signature_valid {
            msg.push_str("  - Invalid signature\n");
        }
        
        for error in &self.errors {
            msg.push_str(&format!("  - {}\n", error));
        }

        msg
    }
}

// Verify a package with all checks
pub fn verify_package<P: AsRef<Path>>(
    package_path: P,
    signature_path: P,
    expected_hash: &str,
) -> Result<VerificationResult, String> {
    let mut result = VerificationResult {
        hash_valid: false,
        signature_valid: false,
        errors: Vec::new(),
    };

    // verify hash
    println!("Verifying package integrity...");
    match crypto::verify_sha256(&package_path, expected_hash) {
        Ok(true) => {
            result.hash_valid = true;
            println!("  Hash: OK");
        }
        Ok(false) => {
            result.errors.push("Hash does not match expected value".to_string());
            println!("  Hash: FAILED");
        }
        Err(e) => {
            result.errors.push(format!("Failed to verify hash: {}", e));
            println!("  Hash: ERROR");
        }
    }

    // verify signature
    println!("Verifying package signature...");
    match crypto::verify_with_trusted_keys(&package_path, &signature_path) {
        Ok(true) => {
            result.signature_valid = true;
            println!("  Signature: OK");
        }
        Ok(false) => {
            result.errors.push("Signature verification failed".to_string());
            println!("  Signature: FAILED");
        }
        Err(e) => {
            result.errors.push(format!("Failed to verify signature: {}", e));
            println!("  Signature: ERROR");
        }
    }

    Ok(result)
}

// Quick hash-only verification (for when signature is not critical)
pub fn verify_hash_only<P: AsRef<Path>>(
    package_path: P,
    expected_hash: &str,
) -> Result<bool, String> {
    crypto::verify_sha256(package_path, expected_hash)
}

// Verify package structure (check if it's a valid archive)
pub fn verify_package_structure<P: AsRef<Path>>(
    package_path: P,
    package_type: &str,
) -> Result<bool, String> {
    match package_type {
        "pax" => verify_pax_structure(&package_path),
        "rpm" => verify_rpm_structure(&package_path),
        "deb" => verify_deb_structure(&package_path),
        _ => Err(format!("Unknown package type: {}", package_type)),
    }
}

// Verify .pax package structure
fn verify_pax_structure<P: AsRef<Path>>(package_path: P) -> Result<bool, String> {
    use std::fs::File;
    use zstd::stream::read::Decoder as ZstdDecoder;
    use tar::Archive;

    let file = File::open(package_path.as_ref())
        .map_err(|e| format!("Failed to open package: {}", e))?;
    
    let decoder = ZstdDecoder::new(file)
        .map_err(|e| format!("Invalid zstd compression: {}", e))?;
    
    let mut archive = Archive::new(decoder);
    let mut has_metadata = false;

    for entry in archive.entries()
        .map_err(|e| format!("Invalid tar archive: {}", e))? {
        let entry = entry
            .map_err(|e| format!("Failed to read entry: {}", e))?;
        
        let path = entry.path()
            .map_err(|e| format!("Failed to get entry path: {}", e))?;
        
        if path == std::path::Path::new("metadata.json") {
            has_metadata = true;
            break;
        }
    }

    if !has_metadata {
        return Err("Package missing metadata.json".to_string());
    }

    Ok(true)
}

// Verify .rpm package structure
fn verify_rpm_structure<P: AsRef<Path>>(package_path: P) -> Result<bool, String> {
    use std::fs::File;
    use std::io::Read;

    let mut file = File::open(package_path.as_ref())
        .map_err(|e| format!("Failed to open package: {}", e))?;
    
    // RPM files start with specific magic bytes: 0xED 0xAB 0xEE 0xDB
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .map_err(|e| format!("Failed to read RPM magic: {}", e))?;
    
    if magic != [0xED, 0xAB, 0xEE, 0xDB] {
        return Err("Invalid RPM file - magic bytes don't match".to_string());
    }

    Ok(true)
}

// Verify .deb package structure
fn verify_deb_structure<P: AsRef<Path>>(package_path: P) -> Result<bool, String> {
    use std::fs::File;
    use ar::Archive as ArArchive;

    let file = File::open(package_path.as_ref())
        .map_err(|e| format!("Failed to open package: {}", e))?;
    
    let mut archive = ArArchive::new(file);
    let mut has_control = false;
    let mut has_data = false;

    while let Some(entry) = archive.next_entry() {
        let entry = entry
            .map_err(|e| format!("Invalid ar archive: {}", e))?;
        
        let name = std::str::from_utf8(entry.header().identifier())
            .map_err(|e| format!("Invalid entry name: {}", e))?;

        if name.starts_with("control.tar") {
            has_control = true;
        }
        if name.starts_with("data.tar") {
            has_data = true;
        }
    }

    if !has_control || !has_data {
        return Err("Invalid deb package structure".to_string());
    }

    Ok(true)
}

// Verification options
pub struct VerifyOptions {
    pub verify_hash: bool,
    pub verify_signature: bool,
    pub verify_structure: bool,
    pub force_insecure: bool,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        VerifyOptions {
            verify_hash: true,
            verify_signature: true,
            verify_structure: true,
            force_insecure: false,
        }
    }
}

// Verify with custom options
pub fn verify_with_options<P: AsRef<Path>>(
    package_path: P,
    signature_path: Option<P>,
    expected_hash: Option<&str>,
    package_type: &str,
    options: &VerifyOptions,
) -> Result<VerificationResult, String> {
    let mut result = VerificationResult {
        hash_valid: !options.verify_hash,  // if not verifying, consider it valid
        signature_valid: !options.verify_signature,
        errors: Vec::new(),
    };

    if options.force_insecure {
        println!("\x1B[33mWARNING: Verification checks disabled with --force-insecure\x1B[0m");
        return Ok(result);
    }

    // Verify structure
    if options.verify_structure {
        match verify_package_structure(&package_path, package_type) {
            Ok(true) => println!("  Structure: OK"),
            Ok(false) => {
                result.errors.push("Invalid package structure".to_string());
                println!("  Structure: FAILED");
            }
            Err(e) => {
                result.errors.push(format!("Structure check failed: {}", e));
                println!("  Structure: ERROR");
            }
        }
    }

    // Verify hash
    if options.verify_hash {
        if let Some(hash) = expected_hash {
            match crypto::verify_sha256(&package_path, hash) {
                Ok(true) => {
                    result.hash_valid = true;
                    println!("  Hash: OK");
                }
                Ok(false) => {
                    result.errors.push("Hash mismatch".to_string());
                    println!("  Hash: FAILED");
                }
                Err(e) => {
                    result.errors.push(format!("Hash verification error: {}", e));
                    println!("  Hash: ERROR");
                }
            }
        }
    }

    // Verify signature
    if options.verify_signature {
        if let Some(sig_path) = signature_path {
            match crypto::verify_with_trusted_keys(&package_path, &sig_path) {
                Ok(true) => {
                    result.signature_valid = true;
                    println!("  Signature: OK");
                }
                Ok(false) => {
                    result.errors.push("Invalid signature".to_string());
                    println!("  Signature: FAILED");
                }
                Err(e) => {
                    result.errors.push(format!("Signature verification error: {}", e));
                    println!("  Signature: ERROR");
                }
            }
        }
    }

    Ok(result)
}

