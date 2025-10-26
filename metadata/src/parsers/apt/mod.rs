use serde::Deserialize;
use settings::OriginKind;
use utils::{Range, VerReq, Version};

use crate::{
    DepVer, depend_kind::DependKind,
    parsers::MetaDataKind,
    processed::{PreBuilt, ProcessedInstallKind, ProcessedMetaData},
};

#[derive(Debug, Deserialize)]
pub struct RawApt {
    pub name: String,
    pub description: String,
    pub version: String,
    pub origin: String,
    pub build_dependencies: Vec<String>,
    pub runtime_dependencies: Vec<String>,
    pub critical_files: Vec<String>,
    pub config_files: Vec<String>,
    pub hash: String,
}

impl RawApt {
    pub fn process(self) -> Option<ProcessedMetaData> {
        let origin = OriginKind::Apt(self.origin.clone());
        
        let build_dependencies = Self::as_dep_kind(&self.build_dependencies)?;
        let runtime_dependencies = Self::as_dep_kind(&self.runtime_dependencies)?;
        
        Some(ProcessedMetaData {
            name: self.name,
            kind: MetaDataKind::Apt,
            description: self.description,
            version: self.version,
            origin,
            dependent: true,
            build_dependencies,
            runtime_dependencies,
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: self.critical_files,
                configs: self.config_files,
            }),
            hash: self.hash,
            package_type: "APT".to_string(),
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
        
        if let Some(ver) = ver.strip_prefix(">>") {
            lower = VerReq::Gt(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix(">=") {
            lower = VerReq::Ge(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix("==") {
            lower = VerReq::Eq(Version::parse(ver).ok()?);
            upper = VerReq::Eq(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<=") {
            upper = VerReq::Le(Version::parse(ver).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<<") {
            upper = VerReq::Lt(Version::parse(ver).ok()?);
        } else {
            lower = VerReq::Eq(Version::parse(ver).ok()?);
            upper = VerReq::Eq(Version::parse(ver).ok()?);
        };
        
        Some(Range { lower, upper })
    }
    
    fn as_dep_kind(deps: &[String]) -> Option<Vec<DependKind>> {
        let mut result = Vec::new();
        
        for dep in deps {
            let val = if let Some(dep) = dep.strip_prefix('!') {
                DependKind::Volatile(dep.to_string())
            } else if let Some(index) = dep.find(['=', '>', '<']) {
                let (name, ver) = dep.split_at(index);
                DependKind::Specific(DepVer {
                    name: name.to_string(),
                    range: RawApt::parse_ver(ver)?,
                })
            } else {
                DependKind::Latest(dep.to_string())
            };
            result.push(val);
        }
        
        Some(result)
    }
}
