use serde::Deserialize;
use settings::OriginKind;
use utils::{Range, VerReq, Version};

use crate::{
    DepVer, depend_kind::DependKind,
    parsers::MetaDataKind,
    processed::{ProcessedCompilable, ProcessedInstallKind, ProcessedMetaData},
};

#[derive(Debug, Deserialize)]
pub struct RawPax {
    pub name: String,
    pub description: String,
    pub version: String,
    pub origin: String,
    pub build_dependencies: Vec<String>,
    pub runtime_dependencies: Vec<String>,
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub purge: String,
    pub hash: String,
}

impl RawPax {
    pub fn process(self) -> Option<ProcessedMetaData> {
        // Parse the origin string - could be github or pax format
        // TODO: maybe we should support more formats later? idk
        let origin = if self.origin.starts_with("gh/") {
            let split = self
                .origin
                .split('/')
                .skip(1)
                .map(|x| x.to_string())
                .collect::<Vec<String>>();
            if split.len() == 2 {
                OriginKind::Github {
                    user: split[0].clone(),
                    repo: split[1].clone(),
                }
            } else {
                return None;
            }
        // } else if self.origin.starts_with("https://") {
        //     OriginKind::Url(self.origin.clone())
        // } else {
        //     return None;
        // };
        // ^^^ commented out for now, might add URL support later
        } else {
            OriginKind::Pax(self.origin.clone())
        };
        let build_dependencies = Self::as_dep_kind(&self.build_dependencies)?;
        let runtime_dependencies = Self::as_dep_kind(&self.runtime_dependencies)?;
        Some(ProcessedMetaData {
            name: self.name,
            kind: MetaDataKind::Pax,
            description: self.description,
            version: self.version,
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
            package_type: "PAX".to_string(),
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
        
        // Clean up the version string first
        let ver = ver.trim();
        
        if ver.is_empty() {
            return Some(Range { lower: VerReq::NoBound, upper: VerReq::NoBound });
        }
        
        // Handle different version constraint formats
        if let Some(ver) = ver.strip_prefix(">>") {
            lower = VerReq::Gt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix(">=") {
            lower = VerReq::Ge(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix(">") {
            lower = VerReq::Gt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("==") {
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Eq(parsed_ver.clone());
            upper = VerReq::Eq(parsed_ver);
        } else if let Some(ver) = ver.strip_prefix("=") {
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Eq(parsed_ver.clone());
            upper = VerReq::Eq(parsed_ver);
        } else if let Some(ver) = ver.strip_prefix("<=") {
            upper = VerReq::Le(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<<") {
            upper = VerReq::Lt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("<") {
            upper = VerReq::Lt(Version::parse(ver.trim()).ok()?);
        } else if let Some(ver) = ver.strip_prefix("~") {
            // Tilde constraint: >= version, < next major version
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Ge(parsed_ver.clone());
            // Calculate next major version
            let mut next_major = parsed_ver.clone();
            next_major.major += 1;
            next_major.minor = 0;
            next_major.patch = 0;
            upper = VerReq::Lt(next_major);
        } else if let Some(ver) = ver.strip_prefix("^") {
            // Caret constraint: >= version, < next minor version
            let parsed_ver = Version::parse(ver.trim()).ok()?;
            lower = VerReq::Ge(parsed_ver.clone());
            // Calculate next minor version
            let mut next_minor = parsed_ver.clone();
            next_minor.minor += 1;
            next_minor.patch = 0;
            upper = VerReq::Lt(next_minor);
        } else {
            // Default to exact version match
            let parsed_ver = Version::parse(ver).ok()?;
            lower = VerReq::Eq(parsed_ver.clone());
            upper = VerReq::Eq(parsed_ver);
        }
        
        Some(Range { lower, upper })
    }
    fn as_dep_kind(deps: &[String]) -> Option<Vec<DependKind>> {
        let mut result = Vec::new();
        for dep in deps {
            let val = if let Some(dep) = dep.strip_prefix('!') {
                DependKind::Volatile(dep.to_string())
            // } else if let Some((name, ver)) = dep.split_once(':') {
            //     DependKind::Specific(DepVer {
            //         name: name.to_string(),
            //         range: RawPax::parse_ver(ver)?,
            //     })
            // ^^^ this was commented out, not sure why tbh
            } else if let Some(index) = dep.find(['=', '>', '<']) {
                let (name, ver) = dep.split_at(index);
                DependKind::Specific(DepVer {
                    name: name.to_string(),
                    range: RawPax::parse_ver(ver)?,
                })
            } else {
                DependKind::Latest(dep.to_string())
            };
            result.push(val);
        }
        Some(result)
    }
}
