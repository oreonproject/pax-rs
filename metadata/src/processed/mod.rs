use serde::{Deserialize, Serialize};
use settings::OriginKind;
use std::hash::Hash;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    process::Command as RunCommand,
};
use tokio::runtime::Runtime;
use utils::{Version, err, get_update_dir, tmpfile};

use crate::{
    DepVer, DependKind, InstallPackage, InstalledInstallKind, InstalledMetaData, MetaDataKind,
    Specific, get_metadata_path,
};
use crate::{installed::InstalledCompilable, pax::RawPax};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ProcessedInstallKind {
    PreBuilt(PreBuilt),
    Compilable(ProcessedCompilable),
}
#[derive(PartialEq, Eq, Debug, Deserialize, Serialize, Hash, Clone)]
pub struct PreBuilt {
    pub critical: Vec<String>,
    pub configs: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ProcessedCompilable {
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub purge: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ProcessedMetaData {
    pub name: String,
    pub kind: MetaDataKind,
    pub description: String,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub build_dependencies: Vec<DependKind>,
    pub runtime_dependencies: Vec<DependKind>,
    pub install_kind: ProcessedInstallKind,
    pub hash: String,
}

impl ProcessedMetaData {
    pub fn to_installed(&self) -> InstalledMetaData {
        InstalledMetaData {
            locked: false,
            name: self.name.clone(),
            kind: self.kind.clone(),
            version: self.version.to_string(),
            origin: self.origin.clone(),
            dependent: self.dependent,
            dependencies: {
                let mut result = Vec::new();
                for dep in &self.runtime_dependencies {
                    if let Some(dep) = dep.as_dep_ver() {
                        result.push(dep);
                    }
                }
                result
            },
            dependents: Vec::new(),
            install_kind: match &self.install_kind {
                ProcessedInstallKind::PreBuilt(prebuilt) => {
                    InstalledInstallKind::PreBuilt(prebuilt.clone())
                }
                ProcessedInstallKind::Compilable(comp) => {
                    InstalledInstallKind::Compilable(InstalledCompilable {
                        uninstall: comp.uninstall.clone(),
                        purge: comp.purge.clone(),
                    })
                }
            },
            hash: self.hash.to_string(),
        }
    }
    pub async fn install_package(self) -> Result<(), String> {
        let name = self.name.to_string();
        println!("Installing {name}...");
        let (path, _) = get_metadata_path(&name)?;
        let mut metadata = self.to_installed();
        let deps = metadata.dependencies.clone();
        let ver = metadata.version.to_string();
        for dependent in metadata.dependents.iter_mut() {
            let their_metadata = InstalledMetaData::open(&dependent.name)?;
            *dependent = Specific {
                name: dependent.name.to_string(),
                version: Version::parse(&their_metadata.version)?,
            }
        }
        let tmpfile = match tmpfile() {
            Some(file) => file,
            None => return err!("Failed to reserve a file for {name}!"),
        };
        if let Ok(mut file) = File::create(&tmpfile) {
            let endpoint = match self.origin {
                OriginKind::Pax(pax) => format!("{pax}?v={}", self.version),
                OriginKind::Github {
                    user: _,
                    repo: _,
                    commit: _,
                } => {
                    return err!("Github is not implemented yet!"); // thingy
                }
            };
            if let Ok(response) = reqwest::get(endpoint).await {
                if let Ok(body) = response.text().await {
                    let Ok(()) = file.write_all(body.as_bytes()) else {
                        return err!("Failed to write downloaded PAX file to TMP file!");
                    };
                } else {
                    return err!("Failed to download PAX file data!");
                }
            } else {
                return err!("Failed to ");
            }
        } else {
            return err!("Failed to open temporary file {}!", tmpfile.display());
        }
        match self.install_kind {
            ProcessedInstallKind::PreBuilt(_) => {
                return err!("PreBuilt is not implemented yet!"); //thingy
            }
            ProcessedInstallKind::Compilable(compilable) => {
                let build = compilable.build.replace("{$~}", &tmpfile.to_string_lossy());
                let mut command = RunCommand::new("/usr/bin/bash");
                if command.arg("-c").arg(build).status().is_err() {
                    return err!("Failed to build package `{}`!", self.name);
                }
                let install = compilable
                    .install
                    .replace("{$~}", &tmpfile.to_string_lossy());
                let mut command = RunCommand::new("/usr/bin/bash");
                if command.arg("-c").arg(install).status().is_err() {
                    return err!("Failed to install package `{}`!", self.name);
                }
            }
        }
        metadata.write(&path)?;
        for dep in deps {
            let dep = dep.get_installed_specific()?;
            dep.write_dependent(&name, &ver)?;
        }
        Ok(())
    }
    pub fn write(self, base: &Path, inc: &mut usize) -> Result<Self, String> {
        let path = loop {
            let mut path = base.to_path_buf();
            path.push(format!("{inc}.yaml"));
            if path.exists() {
                *inc += 1;
                continue;
            }
            break path;
        };
        let mut file = match File::create(&path) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open upgrade metadata as WO!"),
        };
        let data = match serde_norway::to_string(&self) {
            Ok(data) => data,
            Err(_) => return err!("Failed to parse upgrade metadata to string!"),
        };
        match file.write_all(data.as_bytes()) {
            Ok(_) => Ok(self),
            Err(_) => err!("Failed to write upgrade metadata file!"),
        }
    }
    pub fn open(name: &str) -> Result<Self, String> {
        let mut path = get_update_dir()?;
        path.push(format!("{}.yaml", name));
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => return err!("Failed to read package `{name}`'s metadata!"),
        };
        let mut metadata = String::new();
        if file.read_to_string(&mut metadata).is_err() {
            return err!("Failed to read package `{name}`'s config!");
        }
        Ok(match serde_norway::from_str::<Self>(&metadata) {
            Ok(data) => data,
            Err(_) => return err!("Failed to parse package `{name}`'s data!"),
        })
    }
    pub async fn get_metadata(
        app: &str,
        version: Option<&str>,
        sources: &[OriginKind],
        dependent: bool,
    ) -> Option<Self> {
        let mut metadata = None;
        let mut sources = sources.iter();
        while let (Some(source), None) = (sources.next(), &metadata) {
            match source {
                OriginKind::Pax(source) => {
                    metadata = {
                        let endpoint = if let Some(version) = version {
                            format!("{source}/packages/metadata/{app}?v={version}")
                        } else {
                            format!("{source}/packages/metadata/{app}")
                        };
                        let body = reqwest::get(endpoint).await.ok()?.text().await.ok()?;
                        if let Ok(raw_pax) = serde_json::from_str::<RawPax>(&body)
                            && let Some(processed) = raw_pax.process(dependent)
                        {
                            Some(processed)
                        } else {
                            None
                        }
                    };
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
            if metadata.is_some() {
                break;
            }
        }
        metadata
    }
    pub fn remove_update_cache(&self) -> Result<(), String> {
        let path = get_update_dir()?;
        let Ok(dir) = fs::read_dir(&path) else {
            return err!("Failed to read {} as a directory!", path.display());
        };
        for file in dir.flatten() {
            if let Some(name) = file.path().file_prefix() {
                let name = name.to_string_lossy();
                let data = Self::open(&name)?;
                if data.name == self.name {
                    match fs::remove_file(file.path()) {
                        Ok(()) => return Ok(()),
                        Err(_) => return err!("Couldn't remove update cache for {}!", data.name),
                    }
                }
            }
        }
        println!(
            "\x1B[33m[WARN] cache for {} already cleared!\x1B[0m",
            self.name
        );
        Ok(())
    }
    pub async fn get_depends(
        metadata: &Self,
        sources: &[OriginKind],
        prior: &mut HashSet<Specific>,
    ) -> Result<InstallPackage, String> {
        let mut package = InstallPackage {
            metadata: metadata.clone(),
            build_deps: Vec::new(),
            run_deps: Vec::new(),
        };
        package.build_deps =
            DependKind::batch_as_installed(&metadata.build_dependencies, sources, prior).await?;
        package.run_deps =
            DependKind::batch_as_installed(&metadata.runtime_dependencies, sources, prior).await?;
        Ok(package)
    }
    pub fn upgrade_package(&self, sources: &[OriginKind], runtime: &Runtime) -> Result<(), String> {
        let version = Version::parse(&self.version)?;
        let specific = self.as_specific()?;
        let Ok(installed) = InstalledMetaData::open(&self.name) else {
            println!(
                "\x1B[33m[WARN] Skipping `{}`\x1B[0m (This is likely the result of a stale cache)...",
                self.name
            );
            return Ok(());
        };
        let children: Vec<_> = self
            .build_dependencies
            .clone()
            .into_iter()
            .flat_map(|x| x.as_dep_ver())
            .map(|x| x.pull_metadata(Some(sources), true))
            .collect();
        let mut stale_installed = installed
            .dependencies
            .iter()
            .filter(|x| {
                !self
                    .runtime_dependencies
                    .iter()
                    .any(|y| y.as_dep_ver().as_ref() == Some(*x))
            })
            .collect::<Vec<&DepVer>>();
        let mut new_deps = self
            .runtime_dependencies
            .iter()
            .filter(|x| {
                !installed
                    .dependencies
                    .iter()
                    .any(|y| Some(y) == x.as_dep_ver().as_ref())
            })
            .collect::<Vec<&DependKind>>();
        let in_place_upgrade = new_deps
            .extract_if(.., |x| stale_installed.iter().any(|y| y.name == x.name()))
            .collect::<Vec<&DependKind>>();
        stale_installed.retain(|x| !in_place_upgrade.iter().any(|y| y.name() == x.name));
        let children = children
            .into_iter()
            .map(|x| runtime.block_on(x))
            .collect::<Result<Vec<ProcessedMetaData>, String>>()?;
        children
            .into_iter()
            .try_for_each(|x| match runtime.block_on(x.install_package()) {
                Ok(_path) => Ok(()),
                Err(fault) => Err(fault),
            })?;
        for stale in stale_installed {
            stale.get_installed_specific()?.remove(false)?;
        }
        for dep in new_deps {
            if let Some(dep_ver) = dep.as_dep_ver() {
                let installed_metadata = InstalledMetaData::open(&dep_ver.name)?;
                let metadata = runtime
                    .block_on(dep_ver.pull_metadata(Some(sources), installed_metadata.dependent))?;
                runtime.block_on(metadata.install_package())?;
            }
        }
        for package in in_place_upgrade {
            if let Some(dep_ver) = package.as_dep_ver() {
                let name = dep_ver.name.to_string();
                let (path, metadata) = get_metadata_path(&name)?;
                let Some(old_metadata) = metadata else {
                    return err!("Cannot find data for package `{name}`!");
                };
                let metadata = runtime
                    .block_on(dep_ver.pull_metadata(Some(sources), old_metadata.dependent))?;
                if metadata.version != old_metadata.version {
                    runtime.block_on(metadata.install_package())?;
                }
                let mut metadata = InstalledMetaData::open(&name)?;
                if let Some(found) = metadata.dependents.iter_mut().find(|x| x.name == self.name) {
                    found.version = version.clone();
                } else {
                    metadata.dependents.push(specific.clone());
                };
                metadata.write(&path)?;
            }
        }
        runtime.block_on(self.clone().install_package())?;
        Ok(())
    }
    pub fn as_specific(&self) -> Result<Specific, String> {
        Ok(Specific {
            name: self.name.to_string(),
            version: Version::parse(&self.version)?,
        })
    }
}
