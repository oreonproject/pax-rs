use settings::{OriginKind, SettingsYaml};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::PathBuf,
};
use tokio::runtime::Runtime;
use utils::{Range, VerReq, Version, err, get_metadata_dir, get_update_dir};

use crate::depend_kind::DependKind;
use crate::installed::{InstalledInstallKind, InstalledMetaData};
use crate::parsers::{MetaDataKind, pax};
use crate::processed::ProcessedMetaData;
use crate::versioning::{DepVer, Specific};

pub mod depend_kind;
pub mod installed;
pub mod parsers;
pub mod processed;
pub mod versioning;

fn get_metadata_path(name: &str) -> Result<(PathBuf, Option<InstalledMetaData>), String> {
    let mut path = get_metadata_dir()?;
    path.push(format!("{name}.yaml"));
    if !path.exists() {
        Ok((path, None))
    } else if path.is_file() {
        if let Ok(mut file) = File::open(&path) {
            let mut metadata = String::new();
            if file.read_to_string(&mut metadata).is_err() {
                return err!("Failed to read `{name}`'s config!");
            }
            if let Ok(data) = serde_norway::from_str::<InstalledMetaData>(&metadata) {
                Ok((path, Some(data)))
            } else {
                err!("Failed to parse data into InstalledMetaData!")
            }
        } else {
            err!("Failed to read `{name}`'s metadata!")
        }
    } else {
        err!("`{name}`'s metadata file is of unexpected type!")
    }
}

#[derive(Debug)]
pub struct QueuedChanges {
    pub primary: HashSet<Specific>,
    pub secondary: HashSet<Specific>,
}

impl QueuedChanges {
    pub fn new() -> Self {
        Self {
            primary: HashSet::new(),
            secondary: HashSet::new(),
        }
    }
    pub fn extend(&mut self, other: Self) {
        self.primary.extend(other.primary);
        self.secondary.extend(other.secondary);
    }
    pub fn insert_primary(&mut self, other: Specific) -> bool {
        self.primary.insert(other)
    }
    pub fn insert_secondary(&mut self, other: Specific) {
        self.secondary.insert(other);
    }
    pub fn is_empty(&self) -> bool {
        self.primary.is_empty()
    }
    pub fn has_deps(&self) -> bool {
        !self.secondary.is_empty()
    }
    pub fn dependents(&mut self) -> Result<(), String> {
        let mut items = self.primary.iter().cloned().collect::<Vec<Specific>>();
        items.extend_from_slice(&self.secondary.iter().cloned().collect::<Vec<Specific>>());
        for item in items {
            item.get_dependents(self)?;
        }
        Ok(())
    }
}

impl Default for QueuedChanges {
    fn default() -> Self {
        Self::new()
    }
}
/* #region Install */
#[derive(Debug)]
pub struct InstallPackage {
    pub metadata: ProcessedMetaData,
    pub build_deps: Vec<InstallPackage>,
    pub run_deps: Vec<InstallPackage>,
}

impl InstallPackage {
    pub fn list_deps(&self, top: bool) -> HashSet<String> {
        let mut data = HashSet::new();
        if !top {
            data.insert(self.metadata.name.to_string());
        }
        for dep in &self.run_deps {
            data.extend(dep.list_deps(false));
        }
        data
    }
    pub fn install(self, runtime: &Runtime) -> Result<(), String> {
        let mut collected: Vec<ProcessedMetaData> = self.collect()?;
        let depends = collected
            .iter()
            .map(|x| {
                x.build_dependencies
                    .iter()
                    .chain(x.runtime_dependencies.iter())
                    .collect::<Vec<&DependKind>>()
            })
            .collect::<Vec<Vec<&DependKind>>>()
            .into_iter()
            .flatten()
            .flat_map(|x| x.as_dep_ver())
            .collect::<Vec<DepVer>>();
        let mut sets = Vec::new();
        while let Some(metadata) = &collected.first() {
            let name = metadata.name.to_string();
            let set = collected
                .extract_if(.., |x| x.name == name)
                .collect::<Vec<ProcessedMetaData>>();
            sets.push(set);
        }
        let sets = sets.into_iter();
        let mut filtered: Vec<ProcessedMetaData> = Vec::new();
        for mut set in sets {
            if set.is_empty() {
                continue;
            } else if set.len() == 1
                && let Some(metadata) = set.first()
            {
                filtered.push(metadata.clone());
            } else if let Some(name) = set.first().map(|x| x.name.to_string()) {
                let Some(range) = depends.iter().filter(|x| x.name == name).try_fold(
                    Range {
                        lower: VerReq::NoBound,
                        upper: VerReq::NoBound,
                    },
                    |acc, x| x.range.negotiate(Some(acc)),
                ) else {
                    return err!("No dependent of `{name}` could negotiate a common version!");
                };
                set.sort_by_key(|x| Version::parse(&x.version));
                set.reverse();
                let mut chosen = None;
                for metadata in set {
                    let ver_req = VerReq::Eq(Version::parse(&metadata.version)?);
                    let new_range = Range {
                        lower: ver_req.clone(),
                        upper: ver_req,
                    };
                    if new_range.negotiate(Some(range.clone())).is_some() {
                        chosen = Some(metadata);
                        break;
                    }
                }
                let Some(chosen) = chosen else {
                    return err!(
                        "No version of dependent `{name}` fell in the negotiated version range!"
                    );
                };
                filtered.push(chosen);
            }
        }
        for metadata in filtered {
            runtime.block_on(metadata.install_package())?;
        }
        Ok(())
    }
    pub fn collect(self) -> Result<Vec<ProcessedMetaData>, String> {
        let mut result = Vec::new();
        for dep in self.build_deps {
            let data = dep.collect()?;
            result.extend_from_slice(&data);
        }
        for dep in self.run_deps {
            let data = dep.collect()?;
            result.extend_from_slice(&data);
        }
        result.push(self.metadata);
        Ok(result)
    }
}

pub async fn get_packages(
    args: &[(&String, Option<&String>)],
) -> Result<Vec<InstallPackage>, String> {
    print!("\x1B[2K\rBuilding dependency tree... 0%");
    let settings = SettingsYaml::get_settings()?;
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let count = args.len();
    for (i, package) in args.iter().enumerate() {
        if let Some(data) = get_package(&settings.sources, package, false, &mut seen).await? {
            result.push(data);
        }
        print!("\rBuilding dependency tree... {}%", i * 100 / count);
    }
    print!("\rBuilding dependency tree... Done!");
    Ok(result)
}

async fn get_package(
    sources: &[OriginKind],
    dep: &(&String, Option<&String>),
    dependent: bool,
    prior: &mut HashSet<Specific>,
) -> Result<Option<InstallPackage>, String> {
    let (app, version) = dep;
    let metadata =
        match ProcessedMetaData::get_metadata(app, version.map(|x| x.as_str()), sources, dependent)
            .await
        {
            Some(data) => data,
            None => return err!("Failed to parse package `{app}`'s metadata!"),
        };
    if let Ok(installed) = InstalledMetaData::open(&metadata.name)
        && installed.version == metadata.version
    {
        return Ok(None);
    };
    match ProcessedMetaData::get_depends(&metadata, sources, prior).await {
        Ok(data) => Ok(Some(data)),
        Err(fault) => err!("{fault}"),
    }
}

/* #endregion Install */
/* #region Remove/Purge */
pub async fn get_local_deps(args: &[(&String, Option<&String>)]) -> Result<QueuedChanges, String> {
    print!("\x1B[2K\rCollecting dependencies... 0%");
    let mut seen = HashSet::new();
    let count = args.len();
    let mut result = QueuedChanges::new();
    for (i, dep) in args.iter().enumerate() {
        print!("\rCollecting dependencies... {}% ", i * 100 / count);
        result.extend(get_local_dep(dep, &mut seen, true).await?);
    }
    print!("\rCollecting dependencies... Done!");
    result.dependents()?;
    Ok(result)
}

async fn get_local_dep(
    dep: &(&String, Option<&String>),
    prior: &mut HashSet<String>,
    root: bool,
) -> Result<QueuedChanges, String> {
    let (dep, ver) = *dep;
    let data = InstalledMetaData::open(dep)?;
    let mut working = Vec::new();
    if let Some(ver) = ver {
        if data.version == *ver {
            working.push(data);
        }
    } else if !data.dependent {
        working.push(data);
    }
    let mut result = QueuedChanges::new();
    for version in working {
        for dependency in &version.dependencies {
            if prior.contains(&dependency.name) {
                continue;
            } else {
                prior.insert(dependency.name.to_string());
            }
            let items = Box::pin(get_local_dep(
                &(
                    &dependency.name,
                    Some(&dependency.get_installed_specific()?.version.to_string()),
                ),
                prior,
                false,
            ))
            .await?;
            result.extend(items);
            result.insert_secondary(dependency.get_installed_specific()?);
        }
        if root {
            result.insert_primary(Specific {
                name: version.name.to_string(),
                version: Version::parse(&version.version)?,
            });
        }
    }
    Ok(result)
}

/* #endregion Remove/Purge */
/* #region Update */
pub async fn collect_updates() -> Result<(), String> {
    let settings = SettingsYaml::get_settings()?;
    print!("\x1B[2K\rReading package lists... 0%");
    let path = get_metadata_dir()?;
    let Ok(dir) = fs::read_dir(&path) else {
        return err!("Failed to read {} as a directory!", path.display());
    };
    let mut children = Vec::new();
    for file in dir.flatten() {
        children.push(collect_update(file.path(), &settings.sources));
    }
    let path = get_update_dir()?;
    let mut result = Vec::new();
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        print!("\rReading package lists... {}%", i * 100 / count);
        result.extend(child.await?);
    }
    print!("\rReading package lists... Done!\nSaving upgrade data... 0%");
    let Ok(dir) = fs::read_dir(&path) else {
        return err!("Failed to read {} as a directory!", path.display());
    };
    let mut old = Vec::new();
    for file in dir.flatten() {
        if let Some(name) = file.path().file_prefix() {
            old.push(ProcessedMetaData::open(&name.to_string_lossy())?.name);
        }
    }
    let mut inc = 0;
    let count = result.len();
    for (i, data) in result
        .into_iter()
        .filter(|x| !old.contains(&x.name))
        .enumerate()
    {
        print!("\rSaving upgrade data... {}%", i * 100 / count);
        data.write(&path, &mut inc)?;
    }
    println!("\rSaving upgrade data... Done!");
    Ok(())
}

async fn collect_update(
    path: PathBuf,
    sources: &[OriginKind],
) -> Result<Vec<ProcessedMetaData>, String> {
    let mut result = Vec::new();
    if path.extension().is_none_or(|x| x != "yaml") {
        return Ok(Vec::new());
    }
    let name = if let Some(name) = path.file_prefix() {
        name.to_string_lossy()
    } else {
        return Ok(Vec::new());
    };
    let metadata = InstalledMetaData::open(&name)?;
    let name = metadata.name;
    let name = name.to_string();
    if metadata.dependents.is_empty()
        && !metadata.dependent
        && let Some(data) =
            ProcessedMetaData::get_metadata(&name, None, sources, metadata.dependent).await
        && Version::parse(&data.version)? > Version::parse(&metadata.version)?
    {
        result.push(data);
    }

    Ok(result)
}
/* #endregion Update */
/* #region Upgrade */
pub fn upgrade_all() -> Result<Vec<ProcessedMetaData>, String> {
    let path = get_update_dir()?;
    let dir = match fs::read_dir(&path) {
        Ok(dir) => dir,
        Err(_) => {
            return err!("Failed to read {} as a directory!", path.display());
        }
    };
    let mut result = Vec::new();
    for file in dir.flatten() {
        let path = file.path();
        if path.extension().is_some_and(|x| x == "yaml")
            && let Some(name) = path.file_prefix()
        {
            let name = name.to_string_lossy();
            result.push(ProcessedMetaData::open(&name)?);
        }
    }
    Ok(result)
}

pub fn upgrade_only(pkgs: &[(&String, Option<&String>)]) -> Result<Vec<ProcessedMetaData>, String> {
    let base = upgrade_all()?;
    let base = base.iter();
    let mut result = HashSet::new();
    for pkg in pkgs {
        let (pkg, ver) = *pkg;
        if let Some(ver) = ver {
            let found = base
                .as_ref()
                .iter()
                .filter(|x| x.name == *pkg && x.version == *ver)
                .cloned()
                .collect::<Vec<ProcessedMetaData>>();
            result.extend(found);
        } else {
            let found = base
                .as_ref()
                .iter()
                .filter(|x| x.name == *pkg)
                .cloned()
                .collect::<Vec<ProcessedMetaData>>();
            result.extend(found);
        }
    }
    Ok(result.into_iter().collect())
}

pub fn upgrade_packages(packages: &[ProcessedMetaData]) -> Result<(), String> {
    let settings = SettingsYaml::get_settings()?;
    let Ok(runtime) = Runtime::new() else {
        return err!("Error creating runtime!");
    };
    for package in packages {
        println!("Upgrading {}...", package.name);
        package.upgrade_package(&settings.sources, &runtime)?;
        package.remove_update_cache()?;
    }
    println!("Done!");
    Ok(())
}

/* #endregion Upgrade */
/* #region Emancipate */
pub fn emancipate(data: &[(&String, Option<&String>)]) -> Result<(), String> {
    for bit in data {
        let (dep, ver) = *bit;
        let (path, data) = get_metadata_path(dep)?;
        let mut data = if let Some(data) = data {
            data
        } else {
            return err!("Cannot find data for package `{dep}`!");
        };
        if let Some(ver) = ver {
            println!("Emancipating `{dep}` version {ver}...",);
            if data.version == *ver {
                if !data.dependent {
                    println!(
                        "\x1B[33m[WARN] `{dep}` version {ver} is already independent!\x1B[0m..."
                    );
                    continue;
                }
                data.dependent = false;
            }
        } else {
            println!("Emancipating `{dep}`...",);
            if data.dependent {
                data.dependent = false;
            } else {
                println!(
                    "\x1B[33m[WARN] All versions of `{dep}` are already independent!\x1B[0m...",
                );
            }
        };
        data.write(&path)?;
    }
    Ok(())
}
/* #endregion Emancipate */
