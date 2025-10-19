use std::{
    fs::{self, File},
    io::Write,
    process::Command,
};

use serde::{Deserialize, Serialize};
use settings::{OriginKind, SettingsYaml};
use utils::{Range, VerReq, Version, err};

use crate::{
    QueuedChanges, get_metadata_path,
    installed::{InstalledInstallKind, InstalledMetaData},
    processed::ProcessedMetaData,
};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct DepVer {
    pub name: String,
    pub range: Range,
}

impl DepVer {
    pub fn get_installed_specific(&self) -> Result<Specific, String> {
        let metadata = InstalledMetaData::open(&self.name)?;
        Ok(Specific {
            name: metadata.name,
            version: Version::parse(&metadata.version)?,
        })
    }
    pub async fn pull_metadata(
        self,
        sources: Option<&[OriginKind]>,
        dependent: bool,
    ) -> Result<ProcessedMetaData, String> {
        let sources = match sources {
            Some(sources) => sources,
            None => &SettingsYaml::get_settings()?.sources,
        };
        let mut versions = None;
        let mut g_source = None;
        let mut sources = sources.iter();
        while let (Some(source), None) = (sources.next(), &versions) {
            match source {
                OriginKind::Pax(pax) => {
                    let endpoint = format!("{pax}/package/{}", self.name);
                    let Ok(response) = reqwest::get(endpoint).await else {
                        continue;
                    };
                    let Ok(body) = response.text().await else {
                        continue;
                    };
                    let vers = body
                        .split(',')
                        .flat_map(Version::parse)
                        .collect::<Vec<Version>>();
                    if !vers.is_empty() {
                        versions = Some(vers);
                        g_source = Some(source.clone());
                    }
                }
                OriginKind::Github {
                    user: _,
                    repo: _,
                    commit: _,
                } => {
                    // thingy
                    println!("Github is not implemented yet!");
                }
            }
        }
        let (Some(mut versions), Some(source)) = (versions, g_source) else {
            return err!("Failed to locate package `{}`!", &self.name);
        };
        match &self.range.lower {
            VerReq::Gt(gt) => versions.retain(|x| x > gt),
            VerReq::Ge(ge) => versions.retain(|x| x >= ge),
            VerReq::Eq(eq) => versions.retain(|x| x == eq),
            VerReq::NoBound => (),
            fuck => {
                return err!(
                    "Unexpected `lower` version requirement of {fuck:?} for `{}`!",
                    self.name
                );
            }
        };
        match &self.range.upper {
            VerReq::Le(le) => versions.retain(|x| x <= le),
            VerReq::Lt(lt) => versions.retain(|x| x < lt),
            VerReq::Eq(_) | VerReq::NoBound => (),
            fuck => {
                return err!(
                    "Unexpected `upper` version requirement of {fuck:?} for `{}`!",
                    self.name
                );
            }
        };
        versions.sort();
        let Some(ver) = versions.last().map(|x| x.to_string()) else {
            return err!(
                "A guaranteed to be populated Vec was found to be empty. You should never see this error message."
            );
        };
        ProcessedMetaData::get_metadata(&self.name, Some(&ver), &[source], dependent)
            .await
            .ok_or(format!(
                "Failed to locate package `{}` version {ver}!",
                self.name
            ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Specific {
    pub name: String,
    pub version: Version,
}

impl Specific {
    pub fn write_dependent(&self, their_name: &str, their_ver: &str) -> Result<(), String> {
        let (path, data) = get_metadata_path(&self.name)?;
        if path.exists()
            && path.is_file()
            && let Some(mut data) = data
        {
            if data.version == self.version.to_string() {
                let their_dep = Self {
                    name: their_name.to_string(),
                    version: Version::parse(their_ver)?,
                };
                if let Some(found) = data
                    .dependents
                    .iter_mut()
                    .find(|x| x.name == their_dep.name)
                {
                    found.version = their_dep.version;
                } else if !data.dependents.contains(&their_dep)
                    && let Ok(their_metadata) = InstalledMetaData::open(their_name)
                    && their_metadata.version == their_ver
                {
                    data.dependents.push(their_dep);
                }
            }
            let mut file = match File::create(&path) {
                Ok(file) => file,
                Err(_) => {
                    return err!(
                        "Failed to open dependency `{}`'s metadata as WO!",
                        self.name
                    );
                }
            };
            let data = match serde_norway::to_string(&data) {
                Ok(data) => data,
                Err(_) => {
                    return err!(
                        "Failed to parse dependency `{}`'s metadata to string!",
                        self.name
                    );
                }
            };
            match file.write_all(data.as_bytes()) {
                Ok(_) => Ok(()),
                Err(_) => err!(
                    "Failed to write to dependency `{}`'s metadata file!",
                    self.name
                ),
            }
        } else {
            err!("Cannot find data for dependency `{}`!", self.name)
        }
    }
    pub fn get_dependents(&self, queued: &mut QueuedChanges) -> Result<(), String> {
        let data = InstalledMetaData::open(&self.name)?;
        if data.version == self.version.to_string() {
            for dependent in &data.dependents {
                if queued.insert_primary(dependent.clone()) {
                    dependent.get_dependents(queued)?;
                }
            }
            Ok(())
        } else {
            err!(
                "`{}` didn't contain version {}!",
                self.name,
                self.version.to_string()
            )
        }
    }
    pub fn remove(&self, purge: bool) -> Result<(), String> {
        let msg = if purge { "Purging" } else { "Removing" };
        println!("{} {} version {}...", msg, self.name, self.version);
        let (path, data) = get_metadata_path(&self.name)?;
        let Some(data) = data else {
            // Since packages are interlinked, chances are another package
            // has already removed this one, and therefore we are just holding
            // a stale package `Specific`!
            println!(
                "\x1B[33m[WARN] Skipping `{}`\x1B[0m (This is likely the result of queueing a package for removal twice)...",
                self.name
            );
            return Ok(());
        };
        let data = match data.lock(&path, &self.name)? {
            Some(data) => data,
            None => return Ok(()),
        };
        for dep in &data
            .dependencies
            .iter()
            .flat_map(|x| x.get_installed_specific())
            .collect::<Vec<Specific>>()
        {
            data.clear_dependencies(dep)?;
            dep.remove(purge)?;
        }
        match data.install_kind {
            InstalledInstallKind::PreBuilt(_) => {
                return err!("PreBuilt is not implemented yet!"); //thingy
            }
            InstalledInstallKind::Compilable(compilable) => {
                // I'm not sure if the `purge` script is run IN PLACE OF, or
                // AFTER the `uninstall` script. This is due to change.
                let (script, msg) = if purge {
                    (compilable.purge, "purge")
                } else {
                    (compilable.uninstall, "remove")
                };
                let mut command = Command::new("/usr/bin/bash");
                if command.arg("-c").arg(script).status().is_err() {
                    return err!("Failed to {msg} package `{}`!", self.name);
                }
            }
        }
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(_) => err!("Failed to remove `{}`!", &self.name),
        }
    }
}
