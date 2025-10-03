pub mod rpm;
pub mod deb;
pub mod pax;

use std::path::Path;

// Common metadata structure for all package types
#[derive(Debug, Clone)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub origin: String,
    pub dependencies: Vec<Dependency>,
    pub runtime_dependencies: Vec<Dependency>,
    pub provides: Vec<Provides>,
    pub conflicts: Vec<String>,
}

// Dependency information
#[derive(Debug, Clone)]
pub struct Dependency {
    pub name: String,
    pub version_constraint: Option<String>,
    pub dep_type: DependencyType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DependencyType {
    Build,       // needed to build/install
    Runtime,     // needed to run
    Optional,    // nice to have but not required
}

// What a package provides
#[derive(Debug, Clone)]
pub struct Provides {
    pub name: String,
    pub version: Option<String>,
    pub provide_type: ProvideType,
}

#[derive(Debug, Clone)]
pub enum ProvideType {
    Binary,      // executable binary
    Library,     // shared library
    Virtual,     // virtual package/capability
    File,        // generic file
}

// Script execution stages
#[derive(Debug, Clone, Copy)]
pub enum ScriptStage {
    PreInstall,
    PostInstall,
    PreRemove,
    PostRemove,
}

// File entry in a package
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub file_type: FileType,
}

#[derive(Debug, Clone)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    Other,
}

// Trait that all package adapters must implement
pub trait PackageAdapter {
    // Extract metadata from package file
    fn extract_metadata(&self) -> Result<PackageMetadata, String>;
    
    // Extract all files from package to a directory
    fn extract_files(&self, dest_dir: &Path) -> Result<Vec<FileEntry>, String>;
    
    // Get dependencies
    fn get_dependencies(&self) -> Result<Vec<Dependency>, String>;
    
    // Get what this package provides
    fn get_provides(&self) -> Result<Vec<Provides>, String>;
    
    // Run package scripts (if any)
    fn run_script(&self, stage: ScriptStage) -> Result<(), String>;
    
    // Get package hash/checksum
    fn get_hash(&self) -> Result<String, String>;
}

// Detect package type from file extension
pub fn detect_package_type(path: &Path) -> Option<PackageType> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(|ext| match ext {
            "rpm" => Some(PackageType::Rpm),
            "deb" => Some(PackageType::Deb),
            "pax" => Some(PackageType::Pax),
            _ => None,
        })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PackageType {
    Rpm,
    Deb,
    Pax,
}

impl PackageType {
    pub fn as_str(&self) -> &str {
        match self {
            PackageType::Rpm => "rpm",
            PackageType::Deb => "deb",
            PackageType::Pax => "pax",
        }
    }
}

