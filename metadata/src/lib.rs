use semver::Version;
use serde::{Deserialize, Serialize};
use settings::get_settings;
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

pub fn build_deps(
    args: &[String],
    sources: &[String],
    runtime: &Runtime,
    priordeps: &mut HashSet<ProcessedMetaData>,
    dependent: bool,
) -> Result<Vec<ProcessedMetaData>, String> {
    let mut metadatas = match runtime.block_on(get_metadatas(args, sources, dependent)) {
        Ok(data) => data,
        Err(faulty) => {
            return err!("Failed to locate package {faulty}.");
        }
    };
    let deps_vec = match runtime.block_on(get_deps(&metadatas, sources)) {
        Ok(data) => data,
        Err(faulty) => {
            return err!("Failed to parse dependency {faulty}!");
        }
    };
    let mut deps = HashSet::new();
    deps.extend(deps_vec);
    let diff = deps
        .difference(priordeps)
        .collect::<Vec<&ProcessedMetaData>>()
        .iter()
        .map(|x| x.name.clone())
        .collect::<Vec<String>>();
    priordeps.extend(deps);
    if !diff.is_empty() {
        let data = build_deps(&diff, sources, runtime, priordeps, true)?;
        for processed in data {
            if !metadatas
                .iter()
                .any(|x| x.name == processed.name && x.version == processed.version)
            {
                metadatas.push(processed);
            }
        }
    }
    Ok(metadatas)
}

async fn get_metadatas(
    apps: &[String],
    sources: &[String],
    dependent: bool,
) -> Result<Vec<ProcessedMetaData>, String> {
    print!("\x1B[2K\rReading package lists... 0%");
    let mut metadatas = Vec::new();
    let mut children = Vec::new();
    for app in apps {
        children.push(get_metadata(app, None, sources, dependent));
    }
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        print!("\rReading package lists... {}% ", i * 100 / count);
        let _ = std::io::stdout().flush();
        if let Some(child) = child.await {
            metadatas.push(child);
        } else {
            return Err(apps[i].to_string());
        }
    }
    print!("\rReading package lists... Done!");
    Ok(metadatas)
}

async fn get_deps(
    metadatas: &[ProcessedMetaData],
    sources: &[String],
) -> Result<Vec<ProcessedMetaData>, String> {
    print!("\x1B[2K\rCollecting dependencies... 0%");
    let mut deps = Vec::new();
    let mut children = Vec::new();
    for metadata in metadatas {
        children.push(metadata.get_dep(sources));
    }
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        print!("\rCollecting dependencies... {}% ", i * 100 / count);
        let _ = std::io::stdout().flush();
        match child.await {
            Ok(dep) => deps.extend(dep),
            Err(faulty) => return Err(faulty),
        }
    }
    print!("\rCollecting dependencies... Done!");
    Ok(deps)
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash)]
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

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash)]
pub enum ProcessedInstallKind {
    PreBuilt(PreBuilt),
    Compilable(ProcessedCompilable),
}
#[derive(PartialEq, Eq, Debug, Deserialize, Serialize, Hash, Clone)]
pub struct PreBuilt {
    pub critical: Vec<String>,
    pub configs: Vec<String>,
}
#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash)]
pub struct ProcessedCompilable {
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub purge: String,
}

impl ProcessedMetaData {
    pub async fn to_installed(&self, sources: &[String], kind: MetaDataKind) -> InstalledVersion {
        InstalledVersion {
            kind,
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
            // uninstall: self.uninstall.to_string(),
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
        let metadata = runtime.block_on(self.to_installed(sources, self.kind.clone()));
        let mut loaded_data = if let Some(data) = loaded_data {
            data
        } else {
            InstalledMetaData {
                locked: false,
                name: name.clone(),
                installed: Vec::new(),
            }
        };
        let deps = metadata.dependencies.clone();
        let ver = metadata.version.to_string();
        if loaded_data.installed.iter().any(|x| {
            Version::parse(&x.version).unwrap_or(Version::new(0, 0, 0))
                >= Version::parse(&metadata.version).unwrap_or(Version::new(0, 0, 0))
        }) {
            return Ok(None);
        }
        loaded_data.installed.push(metadata);
        loaded_data.write(&path)?;
        for dep in deps {
            dep.write_dependent(&name, &ver)?;
        }
        let tmpfile = match tmpfile() {
            Some(file) => file,
            None => return err!("Failed to reserve a file for {name}!"),
        };
        Ok(Some(tmpfile))
    }
    pub async fn get_dep(&self, sources: &[String]) -> Result<Vec<ProcessedMetaData>, String> {
        let mut deps = Vec::new();
        // These are important to the build process, so they need to be installed prior to
        // installing the dependant, so they get pushed lower down the dependency Vec
        // (lower means it will get installed earlier).
        for dep in &self.dependencies {
            if let PseudoProcessed::MetaData(metadata) = dep.to_processed(sources).await? {
                if let Some(i) = deps.iter().position(|x| *x == *metadata) {
                    deps.remove(i);
                }
                deps.push(*metadata);
            }
        }
        // The dependant can still be built without this dependency, so order doesn't matter.
        for dep in &self.runtime_dependencies {
            if let PseudoProcessed::MetaData(metadata) = dep.to_processed(sources).await?
                && !deps.contains(&metadata)
            {
                deps.push(*metadata);
            }
        }
        Ok(deps)
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
    Url(String),
    Github {
        user: String,
        repo: String,
        commit: String,
    },
}

async fn get_metadata(
    app: &str,
    version: Option<&str>,
    sources: &[String],
    dependent: bool,
) -> Option<ProcessedMetaData> {
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
            DependKind::Latest(latest) => {
                if let Some(data) = get_metadata(latest, None, sources, true).await {
                    if let Ok(mut subdata) = InstalledMetaData::open(&data.name) {
                        subdata.installed.sort_by_key(|x| {
                            Version::parse(&x.version).unwrap_or(Version::new(0, 0, 0))
                        });
                        if let Some(sub_latest) = subdata.installed.last()
                            && Version::parse(&sub_latest.version).unwrap_or(Version::new(0, 0, 0))
                                >= Version::parse(&data.version).unwrap_or(Version::new(0, 0, 0))
                        {
                            return Ok(PseudoProcessed::Specific(Specific {
                                name: data.name,
                                version: sub_latest.version.to_string(),
                            }));
                        }
                    }
                    Ok(PseudoProcessed::MetaData(Box::new(data)))
                } else {
                    Err(latest.to_string())
                }
                // }
            }
            DependKind::Specific(Specific { name, version }) => {
                if let Some(data) = get_metadata(name, Some(version), sources, true).await {
                    Ok(PseudoProcessed::MetaData(Box::new(data)))
                } else {
                    Err(name.to_string())
                }
            }
            DependKind::Volatile(volatile) => {
                if let Ok(Some(status)) = RunCommand::new("which").status().map(|x| x.code()) {
                    if status != 0 {
                        Ok(PseudoProcessed::Volatile)
                    } else if let Some(data) = get_metadata(volatile, None, sources, true).await {
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
            if let Some(bit) = data
                .installed
                .iter_mut()
                .find(|x| x.version == *self.version)
            {
                let their_dep = Specific {
                    name: their_name.to_string(),
                    version: their_ver.to_string(),
                };
                if !bit.dependents.contains(&their_dep) {
                    bit.dependents.push(their_dep);
                } else {
                    return err!("`{}` didn't contain version {}!", self.name, self.version);
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
        if let Some(data) = data.installed.iter().find(|x| x.version == self.version) {
            for dependent in &data.dependents {
                if queued.insert_rem(dependent.clone()) {
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
            println!("\x1B[33m[WARN] Skipping `{}`\x1B[0m...", self.name);
            return Ok(());
        };
        let mut data = if let Some(data) = data.lock(&path, &self.name)? {
            data
        } else {
            return Ok(());
        };
        let (index, child) = match data
            .installed
            .iter_mut()
            .enumerate()
            .find(|x| x.1.version == self.version)
        {
            Some(data) => data,
            None => {
                // Same as above.
                println!(
                    "\x1B[33m[WARN] Skipping `{}` version {}\x1B[0m...",
                    self.name, self.version
                );
                return Ok(());
            }
        };
        for dependency in &child.dependencies {
            dependency.forget_dependent(self)?;
        }
        for dependent in &child.dependents {
            dependent.remove_version(purge)?;
        }
        if purge {
            // Run purge thingy
        } else {
            // Run uninstall thingy
        }
        data.installed.remove(index);
        data.locked = false;
        data.write(&path)?;
        Ok(())
    }
    fn forget_dependent(&self, other: &Self) -> Result<(), String> {
        let (path, data) = get_metadata_path(&self.name)?;
        let data = if let Some(data) = data {
            data
        } else {
            // Something
            println!("\x1B[33m[WARN] Skipping `{}`\x1B[0m...", self.name);
            return Ok(());
        };
        let mut data = if let Some(data) = data.lock(&path, &self.name)? {
            data
        } else {
            return Ok(());
        };
        let (data_index, child) = match data
            .installed
            .iter_mut()
            .enumerate()
            .find(|x| x.1.version == self.version)
        {
            Some(data) => data,
            None => {
                return err!(
                    "Failed to remove `{}`'s dependent {}!",
                    self.name,
                    other.name
                );
            }
        };
        let child_index = match child
            .dependents
            .iter()
            .position(|x| x.name == other.name && x.version == other.version)
        {
            Some(index) => index,
            None => {
                return err!(
                    "Could'nt locate dependent `{}`s in {}!",
                    other.name,
                    self.name
                );
            }
        };
        child.dependents.remove(child_index);
        if child.dependents.is_empty() {
            data.installed.remove(data_index);
        }
        data.locked = false;
        data.write(&path)?;
        Ok(())
    }
}

#[derive(PartialEq, Deserialize, Serialize, Debug)]

struct InstalledMetaData {
    locked: bool,
    name: String,
    installed: Vec<InstalledVersion>,
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
        Ok(
            match serde_norway::from_str::<InstalledMetaData>(&metadata) {
                Ok(data) => data,
                Err(_) => return err!("Failed to parse package `{name}`'s data!"),
            },
        )
    }
    pub fn write(self, path: &Path) -> Result<Option<Self>, String> {
        if self.installed.is_empty() {
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
}

#[derive(PartialEq, Deserialize, Serialize, Debug, Clone)]
pub struct InstalledVersion {
    pub kind: MetaDataKind,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub dependencies: Vec<Specific>,
    pub dependents: Vec<Specific>,
    pub install_kind: InstalledInstallKind,
    pub hash: String,
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
            OriginKind::Url(self.origin.clone())
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

/* #region Remove/Purge */
pub async fn get_local_deps(args: &[(&String, Option<&String>)]) -> Result<QueuedChanges, String> {
    print!("\x1B[2K\rCollecting dependencies... 0%");
    let mut children = Vec::new();
    for dep in args {
        children.push(get_local_dep(dep, true));
    }
    let mut result = QueuedChanges::new();
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        print!("\rCollecting dependencies... {}% ", i * 100 / count);
        result.extend(child.await?);
    }
    print!("\rCollecting dependencies... Done!");
    result.dependents()?;
    Ok(result)
}

async fn get_local_dep(
    dep: &(&String, Option<&String>),
    root: bool,
) -> Result<QueuedChanges, String> {
    let (dep, ver) = *dep;
    let data = InstalledMetaData::open(dep)?;
    let mut working = Vec::new();
    if let Some(ver) = ver {
        if let Some(specific) = data.installed.iter().find(|x| x.version == *ver) {
            working.push(specific);
        }
    } else {
        working.extend(
            data.installed
                .iter()
                .filter(|x| !x.dependent)
                .collect::<Vec<&InstalledVersion>>(),
        );
    }
    let mut result = QueuedChanges::new();
    for version in working {
        for dependency in &version.dependencies {
            let items = Box::pin(get_local_dep(
                &(&dependency.name, Some(&dependency.version)),
                false,
            ))
            .await?;
            result.extend(items);
            result.insert_mod(dependency.clone());
        }
        if root {
            result.insert_rem(Specific {
                name: data.name.to_string(),
                version: version.version.to_string(),
            });
        }
    }
    Ok(result)
}

#[derive(Debug)]
pub struct QueuedChanges {
    pub remove: HashSet<Specific>,
    pub modify: HashSet<Specific>,
}

impl QueuedChanges {
    pub fn new() -> Self {
        QueuedChanges {
            remove: HashSet::new(),
            modify: HashSet::new(),
        }
    }
    pub fn extend(&mut self, other: Self) {
        self.remove.extend(other.remove);
        self.modify.extend(other.modify);
    }
    pub fn insert_mod(&mut self, other: Specific) {
        self.modify.insert(other);
    }
    pub fn insert_rem(&mut self, other: Specific) -> bool {
        self.remove.insert(other)
    }
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty()
    }
    pub fn has_deps(&self) -> bool {
        !self.modify.is_empty()
    }
    pub fn dependents(&mut self) -> Result<(), String> {
        let mut items = self.remove.iter().cloned().collect::<Vec<Specific>>();
        items.extend_from_slice(&self.modify.iter().cloned().collect::<Vec<Specific>>());
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
/* #endregion Remove/Purge */
/* #region Upgrade */
pub async fn collect_upgrades() -> Result<(), String> {
    let settings = get_settings()?;
    print!("\x1B[2K\rReading package lists... 0%");
    let path = get_metadata_dir()?;
    let dir = match fs::read_dir(&path) {
        Ok(dir) => dir,
        Err(_) => {
            return err!("Failed to read {} as a directory!", path.display());
        }
    };
    let mut children = Vec::new();
    for file in dir.flatten() {
        children.push(collect_upgrade(file.path(), &settings.sources));
    }
    let dir = get_update_dir()?;
    let mut result = Vec::new();
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        print!("\rReading package lists... {}%", i * 100 / count);
        result.extend(child.await?);
    }
    print!("\rReading package lists... Done!\nSaving upgrade data... 0%");
    let mut inc = 0;
    let count = result.len();
    for (i, data) in result.into_iter().enumerate() {
        print!("\rSaving upgrade data... {}%", i * 100 / count);
        data.write(&dir, &mut inc)?;
    }
    println!("\rSaving upgrade data... Done!");
    Ok(())
}

async fn collect_upgrade(
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
    for version in metadata.installed {
        let name = name.to_string();
        if version.dependents.is_empty()
            && !version.dependent
            && let Some(data) = get_metadata(&name, None, sources, true).await
            && Version::parse(&data.version).unwrap_or(Version::new(0, 0, 0))
                > Version::parse(&version.version).unwrap_or(Version::new(0, 0, 0))
        {
            result.push(data);
        }
    }
    Ok(result)
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
            if let Some(specific) = data.installed.iter_mut().find(|x| x.version == *ver) {
                if !specific.dependent {
                    println!(
                        "\x1B[33m[WARN] `{dep}` version {ver} is already independent!\x1B[0m..."
                    );
                    continue;
                }
                specific.dependent = false;
            }
        } else {
            println!("Emancipating `{dep}`...",);
            let mut collection = data
                .installed
                .iter_mut()
                .filter(|x| x.dependent)
                .collect::<Vec<&mut InstalledVersion>>();
            match collection.len() {
                0 => println!(
                    "\x1B[33m[WARN] All versions of `{dep}` are already independent!\x1B[0m...",
                ),
                1 => collection.iter_mut().for_each(|x| x.dependent = false),
                _ => {
                    return err!(
                        "Ambiguous reference to `{dep}`. Use with `-s` to specify a version."
                    );
                }
            }
        };
        data.write(&path)?;
    }
    Ok(())
}
/* #endregion Emancipate */
