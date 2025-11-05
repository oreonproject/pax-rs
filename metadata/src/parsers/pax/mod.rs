use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use std::fmt;
use settings::OriginKind;
use utils::{Range, VerReq, Version};

use crate::{
    DepVer, depend_kind::DependKind,
    parsers::MetaDataKind,
    processed::{ProcessedCompilable, ProcessedInstallKind, ProcessedMetaData},
};

// Helper function to normalize field names (handles both hyphen and underscore variants)
// This is case-insensitive and handles any whitespace variations
fn normalize_key(key: &str) -> String {
    let trimmed = key.trim();
    let lower = trimmed.to_lowercase();
    match lower.as_str() {
        "build-dependencies" | "build_dependencies" | "builddependencies" => "build_dependencies".to_string(),
        "runtime-dependencies" | "runtime_dependencies" | "runtimedependencies" => "runtime_dependencies".to_string(),
        _ => trimmed.to_string(),
    }
}

#[derive(Debug)]
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

impl<'de> Deserialize<'de> for RawPax {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawPaxVisitor;

        impl<'de> Visitor<'de> for RawPaxVisitor {
            type Value = RawPax;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct RawPax")
            }

            fn visit_map<V>(self, mut map: V) -> Result<RawPax, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut name = None;
                let mut description = None;
                let mut version = None;
                let mut origin = None;
                let mut build_dependencies = None;
                let mut runtime_dependencies = None;
                let mut build = None;
                let mut install = None;
                let mut uninstall = None;
                let mut purge = None;
                let mut hash = None;

                while let Some(key) = map.next_key::<String>()? {
                    // Normalize the key (trim whitespace and handle variations)
                    let normalized = normalize_key(&key);
                    
                    match normalized.as_str() {
                        "name" => {
                            if name.is_none() {
                                name = Some(map.next_value()?);
                            }
                        }
                        "description" => {
                            if description.is_none() {
                                description = Some(map.next_value()?);
                            }
                        }
                        "version" => {
                            if version.is_none() {
                                version = Some(map.next_value()?);
                            }
                        }
                        "origin" => {
                            if origin.is_none() {
                                origin = Some(map.next_value()?);
                            }
                        }
                        "build_dependencies" => {
                            // Accept the value regardless of whether we've seen it before
                            // This handles cases where both hyphen and underscore versions exist
                            let value: Vec<String> = map.next_value()?;
                            if build_dependencies.is_none() {
                                build_dependencies = Some(value);
                            }
                        }
                        "runtime_dependencies" => {
                            // Accept the value regardless of whether we've seen it before
                            let value: Vec<String> = map.next_value()?;
                            if runtime_dependencies.is_none() {
                                runtime_dependencies = Some(value);
                            }
                        }
                        "build" => {
                            if build.is_none() {
                                build = Some(map.next_value()?);
                            }
                        }
                        "install" => {
                            if install.is_none() {
                                install = Some(map.next_value()?);
                            }
                        }
                        "uninstall" => {
                            if uninstall.is_none() {
                                uninstall = Some(map.next_value()?);
                            }
                        }
                        "purge" => {
                            if purge.is_none() {
                                purge = Some(map.next_value()?);
                            }
                        }
                        "hash" => {
                            if hash.is_none() {
                                hash = Some(map.next_value()?);
                            }
                        }
                        _ => {
                            // Ignore unknown fields for forward compatibility
                            let _ = map.next_value::<de::IgnoredAny>();
                        }
                    }
                }

                Ok(RawPax {
                    name: name.ok_or_else(|| de::Error::missing_field("name"))?,
                    description: description.ok_or_else(|| de::Error::missing_field("description"))?,
                    version: version.ok_or_else(|| de::Error::missing_field("version"))?,
                    origin: origin.ok_or_else(|| de::Error::missing_field("origin"))?,
                    build_dependencies: build_dependencies.unwrap_or_default(),
                    runtime_dependencies: runtime_dependencies.unwrap_or_default(),
                    build: build.ok_or_else(|| de::Error::missing_field("build"))?,
                    install: install.ok_or_else(|| de::Error::missing_field("install"))?,
                    uninstall: uninstall.ok_or_else(|| de::Error::missing_field("uninstall"))?,
                    purge: purge.ok_or_else(|| de::Error::missing_field("purge"))?,
                    hash: hash.ok_or_else(|| de::Error::missing_field("hash"))?,
                })
            }
        }

        deserializer.deserialize_map(RawPaxVisitor)
    }
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
