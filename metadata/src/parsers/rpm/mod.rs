use serde::Deserialize;
use settings::OriginKind;
use utils::{Range, VerReq, Version};

use crate::{
    DepVer, depend_kind::DependKind,
    parsers::MetaDataKind,
    processed::{ProcessedCompilable, ProcessedInstallKind, ProcessedMetaData},
};

#[derive(Debug, Deserialize)]
pub struct RawRpm {
    pub name: String,
    pub description: String,
    pub version: String,
    pub release: String,
    pub arch: String,
    pub origin: String,
    pub build_dependencies: Vec<String>,
    pub runtime_dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub purge: String,
    pub hash: String,
}

impl RawRpm {
    pub fn process(self) -> Option<ProcessedMetaData> {
        let origin = OriginKind::Rpm(self.origin.clone());
        
        let build_dependencies = Self::as_dep_kind(&self.build_dependencies)?;
        let runtime_dependencies = Self::as_dep_kind(&self.runtime_dependencies)?;
        
        // Combine version and release for RPM packages
        let full_version = format!("{}-{}", self.version, self.release);
        
        Some(ProcessedMetaData {
            name: self.name,
            kind: MetaDataKind::Rpm,
            description: self.description,
            version: full_version,
            origin,
            dependent: true,
            build_dependencies,
            runtime_dependencies,
            install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                build: self.build,
                install: self.install,
                uninstall: self.uninstall,
                purge: self.purge,
            }),
            hash: self.hash,
            package_type: "RPM".to_string(),
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: Vec::new(),
        })
    }
    
    fn parse_ver(ver: &str) -> Option<Range> {
        let mut lower = VerReq::NoBound;
        let mut upper = VerReq::NoBound;
        
        // Handle RPM version constraints
        if let Some(ver) = ver.strip_prefix(">=") {
            lower = VerReq::Ge(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix(">") {
            lower = VerReq::Gt(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<=") {
            upper = VerReq::Le(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<") {
            upper = VerReq::Lt(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix("=") {
            lower = VerReq::Eq(Version::parse(ver).ok()?);
            upper = VerReq::Eq(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix("==") {
            lower = VerReq::Eq(Version::parse(ver).ok()?);
            upper = VerReq::Eq(Version::parse(ver).ok()?);
        } else {
            // Default to exact version match
            lower = VerReq::Eq(Version::parse(ver).ok()?);
            upper = VerReq::Eq(Version::parse(ver).ok()?);
        }
        
        Some(Range { lower, upper })
    }
    
    fn as_dep_kind(deps: &[String]) -> Option<Vec<DependKind>> {
        let mut result = Vec::new();
        
        for dep in deps {
            let dep_kind = if dep.contains(">>") || dep.contains(">=") || 
                           dep.contains("<<") || dep.contains("<=") || 
                           dep.contains("==") || dep.contains("=") {
                // Specific version requirement
                DependKind::Specific(DepVer {
                    name: dep.split_whitespace().next()?.to_string(),
                    range: Self::parse_ver(dep)?,
                })
            } else if dep.starts_with("volatile:") {
                // Volatile dependency (system binary)
                DependKind::Volatile(dep.strip_prefix("volatile:")?.to_string())
            } else {
                // Latest version
                DependKind::Latest(dep.to_string())
            };
            
            result.push(dep_kind);
        }
        
        Some(result)
    }
}
