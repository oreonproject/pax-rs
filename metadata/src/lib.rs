use serde::{Deserialize, Serialize};
use settings::{OriginKind, SettingsYaml};
use std::hash::Hash;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command as RunCommand,
};
use tokio::runtime::Runtime;
use utils::{Range, VerReq, Version, err, get_metadata_dir, get_update_dir, tmpfile};

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
    pub fn install_package(self, _sources: &[OriginKind]) -> Result<Option<PathBuf>, String> {
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
        // Run install thingy
        metadata.write(&path)?;
        for dep in deps {
            let dep = dep.get_installed_specific()?;
            dep.write_dependent(&name, &ver)?;
        }
        let tmpfile = match tmpfile() {
            Some(file) => file,
            None => return err!("Failed to reserve a file for {name}!"),
        };
        Ok(Some(tmpfile))
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
            .try_for_each(|x| match x.install_package(sources) {
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
                metadata.install_package(sources)?;
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
                    metadata.install_package(sources)?;
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
        self.clone().install_package(sources)?;
        Ok(())
    }
    pub fn as_specific(&self) -> Result<Specific, String> {
        Ok(Specific {
            name: self.name.to_string(),
            version: Version::parse(&self.version)?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum MetaDataKind {
    Pax,
}

impl std::fmt::Display for MetaDataKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            MetaDataKind::Pax => write!(f, "pax"),
        }
    }
}

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
                let mut command = RunCommand::new("/usr/bin/which");
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
                    let mut command = RunCommand::new("/usr/bin/which");
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

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct DepVer {
    name: String,
    range: Range,
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
        let data = if let Some(data) = data {
            data
        } else {
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
        // Run uninstall/purge thingy...
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(_) => err!("Failed to remove `{}`!", &self.name),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstalledMetaData {
    pub locked: bool,
    pub name: String,
    pub kind: MetaDataKind,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub dependencies: Vec<DepVer>,
    pub dependents: Vec<Specific>,
    pub install_kind: InstalledInstallKind,
    pub hash: String,
}

impl InstalledMetaData {
    pub fn open(name: &str) -> Result<Self, String> {
        let mut path = get_metadata_dir()?;
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
    pub fn write(self, path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() || path.is_file() {
            let data = match serde_norway::to_string(&self) {
                Ok(data) => data,
                Err(_) => {
                    return err!("Failed to parse InstalledMetaData into string!");
                }
            };
            let mut file = match File::create(path) {
                Ok(file) => file,
                Err(_) => return err!("Failed to open file as WO!"),
            };
            match file.write_all(data.as_bytes()) {
                Ok(_) => Ok(Some(self)),
                Err(_) => err!("Failed to write to file!"),
            }
        } else {
            err!("File is of unexpected type!")
        }
    }
    pub fn lock(mut self, path: &Path, name: &str) -> Result<Option<Self>, String> {
        self.locked = true;
        if let Some(data) = self.write(path)? {
            Ok(Some(data))
        } else {
            println!(
                "\x1B[33m[WARN] Skipping `{}` as it has no dependencies.\x1B[0m",
                name
            );
            println!(
                "\x1B[91m=== THIS IS UNEXPECTED BEHAVIOR, AND USUALLY INDICATES BROKEN PACKAGES! ===\x1B[0m..."
            );
            Ok(None)
        }
    }
    // This feels a _little_ bit hacky, so I will probably remove it.
    pub fn clear_dependencies(&self, specific: &Specific) -> Result<(), String> {
        let path = get_metadata_dir()?;
        for dependency in &self.dependencies {
            let mut data = self.clone();
            let Some(index) = data
                .dependencies
                .iter()
                .position(|x| x.get_installed_specific().is_ok_and(|x| x == *specific))
            else {
                return err!(
                    "`{}` {} didn't contain dependent `{}`!",
                    data.name,
                    data.version,
                    specific.name
                );
            };
            data.dependencies.remove(index);
            let mut path = path.clone();
            path.push(format!("{}.yaml", dependency.name));
            data.write(&path)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum InstalledInstallKind {
    PreBuilt(PreBuilt),
    Compilable(InstalledCompilable),
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstalledCompilable {
    pub uninstall: String,
    pub purge: String,
}

#[derive(Debug, Deserialize, PartialEq)]
struct RawPax {
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
    pub fn process(self, dependent: bool) -> Option<ProcessedMetaData> {
        let origin = if self.origin.starts_with("gh/") {
            let split = self
                .origin
                .split('/')
                .skip(1)
                .map(|x| x.to_string())
                .collect::<Vec<String>>();
            if split.len() == 3 {
                OriginKind::Github {
                    user: split[0].clone(),
                    repo: split[1].clone(),
                    commit: split[2].clone(),
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
            dependent,
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
    pub fn install(self, sources: &[OriginKind]) -> Result<(), String> {
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
            metadata.install_package(sources)?;
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
