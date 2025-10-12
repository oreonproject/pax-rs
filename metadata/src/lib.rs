use semver::Version;
use serde::{Deserialize, Serialize};
use settings::SettingsYaml;
use std::hash::Hash;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command as RunCommand,
};
use tokio::runtime::Runtime;
use utils::{err, get_metadata_dir, get_update_dir, tmpfile};

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash, Clone)]
pub struct ProcessedMetaData {
    pub name: String,
    pub kind: MetaDataKind,
    pub description: String,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub dependencies: Vec<DependKind>,
    pub runtime_dependencies: Vec<DependKind>,
    pub install_kind: ProcessedInstallKind,
    pub hash: String,
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash, Clone)]
pub enum ProcessedInstallKind {
    PreBuilt(PreBuilt),
    Compilable(ProcessedCompilable),
}
#[derive(PartialEq, Eq, Debug, Deserialize, Serialize, Hash, Clone)]
pub struct PreBuilt {
    pub critical: Vec<String>,
    pub configs: Vec<String>,
}
#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash, Clone)]
pub struct ProcessedCompilable {
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub purge: String,
}

impl ProcessedMetaData {
    pub async fn to_installed(&self, sources: &[String]) -> InstalledMetaData {
        InstalledMetaData {
            locked: false,
            name: self.name.clone(),
            kind: self.kind.clone(),
            version: self.version.to_string(),
            origin: self.origin.clone(),
            dependent: self.dependent,
            dependencies: {
                let mut children = Vec::new();
                let mut dependencies = Vec::new();
                for dep in &self.dependencies {
                    children.push(dep.to_processed(sources));
                }
                for child in children {
                    if let Ok(child) = child.await {
                        match child {
                            PseudoProcessed::MetaData(data) => {
                                dependencies.push(Specific {
                                    name: data.name,
                                    version: data.version,
                                });
                            }
                            PseudoProcessed::Specific(specific) => dependencies.push(specific),
                            PseudoProcessed::Volatile => (),
                        }
                    }
                }
                dependencies
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
    pub fn install_package(
        self,
        sources: &[String],
        runtime: &Runtime,
    ) -> Result<Option<PathBuf>, String> {
        let name = self.name.to_string();
        println!("Installing {name}...");
        let (path, loaded_data) = get_metadata_path(&name)?;
        let metadata = runtime.block_on(self.to_installed(sources));
        let deps = metadata.dependencies.clone();
        let ver = metadata.version.to_string();
        if let Some(loaded_data) = loaded_data {
            if Version::parse(&loaded_data.version).unwrap_or(Version::new(0, 0, 0))
                >= Version::parse(&metadata.version).unwrap_or(Version::new(0, 0, 0))
            {
                return Ok(None);
            } else {
                // Handle conflict??
            }
        }
        // Run install thingy
        metadata.write(&path)?;
        for dep in deps {
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
        sources: &[String],
        dependent: bool,
    ) -> Option<Self> {
        let mut metadata = None;
        for source in sources {
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
        sources: &[String],
        prior: &mut HashSet<Specific>,
    ) -> Result<InstallPackage, String> {
        let mut package = InstallPackage {
            metadata: metadata.clone(),
            dependencies: Vec::new(),
        };
        for dep in &metadata.dependencies {
            let dep = match dep {
                DependKind::Latest(latest) => {
                    if let Some(data) = Self::get_metadata(latest, None, sources, true).await {
                        Some(data)
                    } else {
                        return err!("Failed to locate latest version of dependency `{latest}`");
                    }
                }
                DependKind::Specific(specific) => {
                    if let Some(data) =
                        Self::get_metadata(&specific.name, Some(&specific.version), sources, true)
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
                DependKind::Volatile(volatile) => {
                    let mut command = RunCommand::new("/usr/bin/which");
                    command.arg(volatile);
                    command.stdout(std::process::Stdio::null());
                    command.stderr(std::process::Stdio::null());
                    if let Ok(Some(status)) = command.status().map(|x| x.code())
                        && status == 0
                    {
                        None
                    } else if let Some(data) =
                        Self::get_metadata(volatile, None, sources, true).await
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
                    version: dep.version.to_string(),
                };
                if !prior.contains(&specific) {
                    prior.insert(specific);
                    let child = Box::pin(Self::get_depends(&dep, sources, prior)).await?;
                    package.dependencies.push(child);
                }
            }
        }
        Ok(package)
    }
    pub fn upgrade_package(
        package: &Self,
        current_ver: Option<&str>,
        target_ver: &str,
        sources: &[String],
        runtime: &Runtime,
    ) -> Result<(), String> {
        let installed = InstalledMetaData::open(&package.name)?;
        let mut to_upgrade = Vec::new();
        if let Some(ver) = current_ver {
            if installed.version == ver {
                to_upgrade.push(installed);
            } else {
                return err!("`{}` didn't contain version {}!", package.name, ver);
            }
        } else if !installed.dependent {
            to_upgrade.push(installed);
        }
        for old_pkg in to_upgrade {
            let new_data = old_pkg
                .origin
                .get_ver(target_ver, old_pkg.dependent, runtime)?;
            let new_version = Specific {
                name: new_data.name.to_string(),
                version: new_data.version.to_string(),
            };
            let old_version = Specific {
                name: new_data.name.to_string(),
                version: old_pkg.version.to_string(),
            };
            old_pkg.clear_dependencies(&old_version)?;
            let new_pkg = runtime.block_on(new_data.to_installed(sources));
            for old_dep in old_pkg
                .dependencies
                .iter()
                .filter(|x| !new_pkg.dependencies.contains(x))
            {
                old_dep.remove_version(true)?;
            }
            for new_dep in new_pkg
                .dependencies
                .iter()
                .filter(|x| !old_pkg.dependencies.contains(x))
            {
                let mut seen = HashSet::new();
                if let Some(data) = runtime.block_on(get_package(
                    sources,
                    &(&new_dep.name, Some(&new_dep.version)),
                    true,
                    &mut seen,
                ))? {
                    data.install(sources, runtime)?;
                }
            }
            for new_dep in new_pkg.dependents {
                new_dep.write_dependent(&new_version.name, &new_version.version)?;
            }
            old_version.remove_version(false)?;
            match new_data.install_package(sources, runtime) {
                Ok(_file) => {} // < == ;P
                Err(fault) => {
                    return err!(
                        "\x1B[0mError updating package {}!\nReported error: \"\x1B[91m{fault}\x1B[0m\"",
                        package.name
                    );
                }
            }
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash, Clone)]
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

#[derive(PartialEq, Eq, Deserialize, Serialize, Debug, Hash, Clone)]
pub enum OriginKind {
    Pax(String),
    Github {
        user: String,
        repo: String,
        commit: String,
    },
}

impl OriginKind {
    pub fn get_ver(
        &self,
        target: &str,
        dependent: bool,
        runtime: &Runtime,
    ) -> Result<ProcessedMetaData, String> {
        match self {
            Self::Pax(url) => {
                let url = url.replace("/package/", "/packages/metadata/");
                let endpoint = format!("{url}?v={target}");
                async fn get_data(endpoint: &str) -> Option<String> {
                    reqwest::get(endpoint).await.ok()?.text().await.ok()
                }
                if let Some(body) = runtime.block_on(get_data(&endpoint)) {
                    if let Ok(raw_pax) = serde_json::from_str::<RawPax>(&body) {
                        if let Some(processed) = raw_pax.process(dependent) {
                            Ok(processed)
                        } else {
                            err!("Failed to process package data at {url}!")
                        }
                    } else {
                        err!("Failed to parse package data at {url}!")
                    }
                } else {
                    err!("Failed to locate origin {url}!")
                }
            }
            Self::Github {
                user: _,
                repo: _,
                commit: _,
            } => err!("Not implemented!"),
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
enum PseudoProcessed {
    MetaData(Box<ProcessedMetaData>),
    Specific(Specific),
    Volatile,
}
#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash, Clone)]
pub enum DependKind {
    Latest(String),
    Specific(Specific),
    Volatile(String),
}

impl DependKind {
    async fn to_processed(&self, sources: &[String]) -> Result<PseudoProcessed, String> {
        match self {
            Self::Latest(latest) => {
                if let Some(data) =
                    ProcessedMetaData::get_metadata(latest, None, sources, true).await
                {
                    if let Ok(subdata) = InstalledMetaData::open(&data.name)
                        && Version::parse(&subdata.version).unwrap_or(Version::new(0, 0, 0))
                            >= Version::parse(&data.version).unwrap_or(Version::new(0, 0, 0))
                    {
                        return Ok(PseudoProcessed::Specific(Specific {
                            name: data.name,
                            version: subdata.version.to_string(),
                        }));
                    }
                    Ok(PseudoProcessed::MetaData(Box::new(data)))
                } else {
                    Err(latest.to_string())
                }
            }
            Self::Specific(Specific { name, version }) => {
                if let Some(data) =
                    ProcessedMetaData::get_metadata(name, Some(version), sources, true).await
                {
                    Ok(PseudoProcessed::MetaData(Box::new(data)))
                } else {
                    Err(name.to_string())
                }
            }
            Self::Volatile(volatile) => {
                if let Ok(Some(status)) =
                    RunCommand::new("/usr/bin/which").status().map(|x| x.code())
                {
                    if status != 0 {
                        Ok(PseudoProcessed::Volatile)
                    } else if let Some(data) =
                        ProcessedMetaData::get_metadata(volatile, None, sources, true).await
                    {
                        Ok(PseudoProcessed::MetaData(Box::new(data)))
                    } else {
                        Err(volatile.to_string())
                    }
                } else {
                    Err(volatile.to_string())
                }
            }
        }
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash, Clone)]
pub struct Specific {
    pub name: String,
    pub version: String,
}

impl Specific {
    pub fn write_dependent(&self, their_name: &str, their_ver: &str) -> Result<(), String> {
        let (path, data) = get_metadata_path(&self.name)?;
        if path.exists()
            && path.is_file()
            && let Some(mut data) = data
        {
            if data.version == self.version {
                let their_dep = Self {
                    name: their_name.to_string(),
                    version: their_ver.to_string(),
                };
                if !data.dependents.contains(&their_dep) {
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
        if data.version == self.version {
            for dependent in &data.dependents {
                if queued.insert_primary(dependent.clone()) {
                    dependent.get_dependents(queued)?;
                }
            }
            Ok(())
        } else {
            err!("`{}` didn't contain version {}!", self.name, self.version)
        }
    }
    pub fn remove_version(&self, purge: bool) -> Result<(), String> {
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
                "\x1B[33m[WARN] Skipping `{}`\x1B[0m (is it installed?)...",
                self.name
            );
            return Ok(());
        };
        if data.lock(&path, &self.name)?.is_none() {
            return Ok(());
        };
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(_) => err!("Failed to remove `{}`!", &self.name),
        }
    }
}

#[derive(PartialEq, Deserialize, Serialize, Debug, Clone)]
pub struct InstalledMetaData {
    pub locked: bool,
    pub name: String,
    pub kind: MetaDataKind,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub dependencies: Vec<Specific>,
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
        if self.dependents.is_empty() {
            if fs::remove_file(path).is_err() {
                err!("Failed to remove {}!", &self.name)
            } else {
                Ok(None)
            }
        } else if !path.exists() || path.is_file() {
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
        if self.locked {
            println!(
                "\x1B[33m[WARN] Package `{}` is busy!\x1B[0m Skipping...",
                self.name
            );
            return Ok(None);
        }
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
            let mut data = InstalledMetaData::open(&dependency.name)?;
            let Some(index) = data.dependents.iter().position(|x| x == specific) else {
                return err!(
                    "`{}` {} didn't contain dependent {}!",
                    data.name,
                    data.version,
                    specific.name
                );
            };
            data.dependents.remove(index);
            let mut path = path.clone();
            path.push(format!("{}.yaml", dependency.name));
            data.write(&path)?;
        }
        Ok(())
    }
}

#[derive(PartialEq, Deserialize, Serialize, Debug, Clone)]
pub enum InstalledInstallKind {
    PreBuilt(PreBuilt),
    Compilable(InstalledCompilable),
}
#[derive(PartialEq, Deserialize, Serialize, Debug, Clone)]
pub struct InstalledCompilable {
    pub uninstall: String,
    pub purge: String,
}

#[derive(PartialEq, Deserialize, Debug)]
struct RawPax {
    name: String,
    description: String,
    version: String,
    origin: String,
    dependencies: Vec<String>,
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
        let dependencies = {
            let mut deps = Vec::new();
            for dep in &self.dependencies {
                let val = if let Some(dep) = dep.strip_prefix('!') {
                    DependKind::Volatile(dep.to_string())
                } else if let Some((name, ver)) = dep.split_once(':') {
                    DependKind::Specific(Specific {
                        name: name.to_string(),
                        version: ver.to_string(),
                    })
                } else {
                    DependKind::Latest(dep.to_string())
                };
                deps.push(val);
            }
            deps
        };
        let runtime_dependencies = {
            let mut deps = Vec::new();
            for dep in &self.runtime_dependencies {
                let val = if let Some(dep) = dep.strip_prefix('!') {
                    DependKind::Volatile(dep.to_string())
                } else if let Some((name, ver)) = dep.split_once(':') {
                    DependKind::Specific(Specific {
                        name: name.to_string(),
                        version: ver.to_string(),
                    })
                } else {
                    DependKind::Latest(dep.to_string())
                };
                deps.push(val);
            }
            deps
        };
        Some(ProcessedMetaData {
            name: self.name,
            kind: MetaDataKind::Pax,
            description: self.description,
            version: self.version,
            origin,
            dependent,
            dependencies,
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
/* #region Remove/Purge */
pub async fn get_local_deps(args: &[(&String, Option<&String>)]) -> Result<QueuedChanges, String> {
    print!("\x1B[2K\rCollecting dependencies... 0%");
    let mut seen = HashSet::new();
    let count = args.len();
    let mut result = QueuedChanges::new();
    for (i, dep) in args.iter().enumerate() {
        print!("\rCollecting dependencies... {}% ", i * 100 / count);
        let _ = std::io::stdout().flush();
        result.extend(get_local_dep(dep, &mut seen, true).await?);
    }
    print!("\rCollecting dependencies... Done!");
    result.dependents()?;
    Ok(result)
}

async fn get_local_dep(
    dep: &(&String, Option<&String>),
    prior: &mut HashSet<Specific>,
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
            if prior.contains(dependency) {
                continue;
            } else {
                prior.insert(dependency.clone());
            }
            let items = Box::pin(get_local_dep(
                &(&dependency.name, Some(&dependency.version)),
                prior,
                false,
            ))
            .await?;
            result.extend(items);
            result.insert_secondary(dependency.clone());
        }
        if root {
            result.insert_primary(Specific {
                name: version.name.to_string(),
                version: version.version.to_string(),
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
        let _ = std::io::stdout().flush();
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
        let _ = std::io::stdout().flush();
        data.write(&path, &mut inc)?;
    }
    println!("\rSaving upgrade data... Done!");
    Ok(())
}

async fn collect_update(
    path: PathBuf,
    sources: &[String],
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
        && let Some(data) = ProcessedMetaData::get_metadata(&name, None, sources, true).await
        && Version::parse(&data.version).unwrap_or(Version::new(0, 0, 0))
            > Version::parse(&metadata.version).unwrap_or(Version::new(0, 0, 0))
    {
        result.push(data);
    }

    Ok(result)
}
/* #endregion Update */
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
        ProcessedMetaData::upgrade_package(
            package,
            None,
            &package.version,
            &settings.sources,
            &runtime,
        )?;
        package.remove_update_cache()?;
    }
    println!("Done!");
    Ok(())
}

/* #endregion Upgrade */
/* #region Install */

#[derive(Debug)]
pub struct InstallPackage {
    pub metadata: ProcessedMetaData,
    pub dependencies: Vec<InstallPackage>,
}

impl InstallPackage {
    pub fn list_deps(&self) -> HashSet<String> {
        let mut data = HashSet::new();
        data.insert(self.metadata.name.to_string());
        for dep in &self.dependencies {
            data.extend(dep.list_deps());
        }
        data
    }
    pub fn install(self, sources: &[String], runtime: &Runtime) -> Result<(), String> {
        for dep in self.dependencies {
            dep.install(sources, runtime)?;
        }
        self.metadata.install_package(sources, runtime)?;
        Ok(())
    }
}

pub async fn get_packages(
    args: &[(&String, Option<&String>)],
) -> Result<Vec<InstallPackage>, String> {
    print!("\x1B[2K\rBuilding dependency tree... 0%");
    let settings = SettingsYaml::get_settings()?;
    let mut children = Vec::new();
    let mut seen = HashSet::new();
    let count = args.len();
    for (i, package) in args.iter().enumerate() {
        if let Some(data) = get_package(&settings.sources, package, false, &mut seen).await? {
            children.push(data);
        }
        print!("\rBuilding dependency tree... {}%", i * 100 / count);
        let _ = std::io::stdout().flush();
    }
    print!("\rBuilding dependency tree... Done!");
    Ok(children)
}

async fn get_package(
    sources: &[String],
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
    // else {handle conflicts!!!}
    match ProcessedMetaData::get_depends(&metadata, sources, prior).await {
        Ok(data) => Ok(Some(data)),
        Err(fault) => err!("{fault}"),
    }
}

/* #endregion Install */
