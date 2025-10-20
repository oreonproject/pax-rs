use serde::Deserialize;
use settings::OriginKind;
use utils::{Range, VerReq, Version};

use crate::{
    DepVer, DependKind,
    parsers::MetaDataKind,
    processed::{ProcessedCompilable, ProcessedInstallKind, ProcessedMetaData},
};

#[derive(Debug, Deserialize)]
pub struct RawPax {
    name: String,
    description: String,
    version: String,
    origin: String,
    build_dependencies: Vec<String>,
    runtime_dependencies: Vec<String>,
    build: String,
    install: String,
    uninstall: String,
    purge: String,
    hash: String,
}

impl RawPax {
    pub fn process(self) -> Option<ProcessedMetaData> {
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
        // Yeah this needs to be done properly, so.....
        // thingy
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
