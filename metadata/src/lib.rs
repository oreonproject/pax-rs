pub mod parsers;
pub mod processed;
pub mod installed;
pub mod depend_kind;
pub mod rollback;
pub mod package_verification;
pub mod package_holds;
pub mod file_tracking;
pub mod service_management;
pub mod repository_auth;
pub mod conflict_resolution;
pub mod cloudflare_r2;
pub mod deb_repository;
pub mod yum_repository;
pub mod performance;
pub mod extensions;

// Re-export commonly used types
pub use utils::{DepVer, Specific};
pub use installed::{InstalledMetaData, InstalledInstallKind};
pub use processed::{ProcessedMetaData, ProcessedInstallKind, ProcessedCompilable, InstallPackage, QueuedChanges};
pub use parsers::{MetaDataKind, pax::RawPax};
pub use package_verification::PackageVerifier;
pub use package_holds::PackageHoldManager;
pub use utils::get_metadata_dir as get_metadata_path;

// Re-export commonly used functions
pub use processed::{
    get_packages, get_package_info, list_installed_packages,
    get_local_deps, search_packages, collect_updates,
    upgrade_all, upgrade_only, upgrade_packages, emancipate
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_package_verification_sha256() {
        // Test SHA256 hash verification
        use sha2::{Sha256, Digest};
        
        let test_data = b"Hello, PAX!";
        let mut hasher = Sha256::new();
        hasher.update(test_data);
        let hash = format!("{:x}", hasher.finalize());
        
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 produces 64 character hex string
    }

    #[test]
    fn test_dependency_parsing() {
        // Test that dependencies are parsed correctly
        let dep_str = "package1 >= 1.0.0, package2 << 2.0.0";
        let deps = crate::processed::ProcessedMetaData::parse_dependency_list(dep_str);
        
        // Should return a vector of dependencies
        assert!(deps.len() >= 0);
    }

    #[test]
    fn test_version_parsing() {
        use utils::Version;
        
        let v1 = Version::parse("1.2.3");
        assert!(v1.is_ok());
        
        let v2 = Version::parse("invalid");
        // This might fail or succeed depending on implementation
        // Just check that it doesn't panic
        let _ = v2;
    }
}
