use std::{collections::HashSet, process::Command};

use serde::{Deserialize, Serialize};
use settings::OriginKind;
use utils::{Range, VerReq, Version, err};

use crate::{DepVer, InstallPackage, Specific, processed::ProcessedMetaData};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum DependKind {
    Latest(String),
    Specific(DepVer),
    Volatile(String),
}

impl DependKind {
    pub fn as_dep_ver(&self) -> Option<DepVer> {
        match self {
            Self::Latest(latest) => {
                // let version = VerReq::Eq(Version::parse(&get_latest(latest).await.ok()?).ok()?);
                // Maybe set a `lower` to VerReq::Ge(currently_installed_version); `upper` to VerReq::NoBound
                let version = VerReq::NoBound;
                Some(DepVer {
                    name: latest.to_string(),
                    range: Range {
                        lower: version.clone(),
                        upper: version,
                    },
                })
            }
            Self::Specific(specific) => Some(specific.clone()),
            Self::Volatile(volatile) => {
                let mut command = Command::new("/usr/bin/which");
                command.arg(volatile);
                command.stdout(std::process::Stdio::null());
                command.stderr(std::process::Stdio::null());
                if let Ok(Some(status)) = command.status().map(|x| x.code())
                    && status == 0
                {
                    None
                } else {
                    Some(DepVer {
                        name: volatile.to_string(),
                        range: Range {
                            lower: VerReq::NoBound,
                            upper: VerReq::NoBound,
                        },
                    })
                }
            }
        }
    }
    pub async fn batch_as_installed(
        deps: &[Self],
        sources: &[OriginKind],
        prior: &mut HashSet<Specific>,
    ) -> Result<Vec<InstallPackage>, String> {
        let mut result = Vec::new();
        for dep in deps {
            let dep = match dep {
                Self::Latest(latest) => {
                    if let Some(data) =
                        ProcessedMetaData::get_metadata(latest, None, sources, true).await
                    {
                        Some(data)
                    } else {
                        return err!("Failed to locate latest version of dependency `{latest}`");
                    }
                }
                Self::Specific(dep_ver) => {
                    let specific = dep_ver.clone().pull_metadata(Some(sources), true).await?;
                    if let Some(data) = ProcessedMetaData::get_metadata(
                        &specific.name,
                        Some(&specific.version.to_string()),
                        sources,
                        true,
                    )
                    .await
                    {
                        Some(data)
                    } else {
                        return err!(
                            "Failed to locate dependency `{}` version {}!",
                            specific.name,
                            specific.version
                        );
                    }
                }
                Self::Volatile(volatile) => {
                    let mut command = Command::new("/usr/bin/which");
                    command.arg(volatile);
                    command.stdout(std::process::Stdio::null());
                    command.stderr(std::process::Stdio::null());
                    if let Ok(Some(status)) = command.status().map(|x| x.code())
                        && status == 0
                    {
                        None
                    } else if let Some(data) =
                        ProcessedMetaData::get_metadata(volatile, None, sources, true).await
                    {
                        Some(data)
                    } else {
                        return err!(
                            "Failed to locate latest version of volatile dependency `{volatile}`"
                        );
                    }
                }
            };
            if let Some(dep) = dep {
                let specific = Specific {
                    name: dep.name.to_string(),
                    version: Version::parse(&dep.version)?,
                };
                if !prior.contains(&specific) {
                    prior.insert(specific);
                    let child =
                        Box::pin(ProcessedMetaData::get_depends(&dep, sources, prior)).await?;
                    result.push(child);
                }
            }
        }
        Ok(result)
    }
    pub fn name(&self) -> String {
        match self {
            Self::Latest(latest) => latest.to_string(),
            Self::Specific(specific) => specific.name.to_string(),
            Self::Volatile(volatile) => volatile.to_string(),
        }
    }
}
