use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command as RunCommand,
};

use nix::unistd;
use serde::{Deserialize, Serialize};
use settings::{get_metadata_dir, get_settings};
use tokio::runtime::Runtime;

use crate::{Command, PostAction, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }
    let args = match args {
        None => return PostAction::Return,
        Some(args) => args,
    };
    print!("Reading sources...");
    let sources = match get_settings() {
        Ok(settings) => settings.sources,
        Err(_) => return PostAction::PullSources,
    };
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(_) => {
            println!("Error creating runtime!");
            return PostAction::Return;
        }
    };
    if let Ok(mut packages) = build_deps(args, &sources, &runtime, &mut HashSet::new(), false) {
        println!();
        packages.reverse();
        for package in packages {
            match &package.kind {
                MetaDataKind::Pax => {
                    if let Err(message) = install_pax(&package) {
                        println!(
                            "Error installing package {}!\nReported error: `{message}`",
                            package.name
                        );
                        return PostAction::Return;
                    }
                }
            }
        }
    };
    PostAction::Return
}

fn build_deps(
    args: &[String],
    sources: &[String],
    runtime: &Runtime,
    priordeps: &mut HashSet<ProcessedMetaData>,
    dependent: bool,
) -> Result<Vec<ProcessedMetaData>, ()> {
    let mut metadatas = match runtime.block_on(get_metadatas(args, sources, dependent)) {
        Ok(data) => data,
        Err(faulty) => {
            println!("\x1B[2K\rFailed to locate package {faulty}.");
            return Err(());
        }
    };
    let deps_vec = match runtime.block_on(get_deps(&metadatas, sources, true)) {
        Ok(data) => data,
        Err(faulty) => {
            println!("\x1B[2K\rFailed to parse dependency {faulty}!");
            return Err(());
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
        match build_deps(&diff, sources, runtime, priordeps, true) {
            Ok(data) => {
                for processed in data {
                    if !metadatas
                        .iter()
                        .any(|x| x.name == processed.name && x.version == processed.version)
                    {
                        metadatas.push(processed);
                    }
                }
            }
            Err(()) => return Err(()),
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
        if let Some(child) = child.into_future().await {
            metadatas.push(child);
        } else {
            return Err(apps[i].to_string());
        }
    }
    print!("\rReading package lists... Done!");
    Ok(metadatas)
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
async fn get_deps(
    metadatas: &[ProcessedMetaData],
    sources: &[String],
    dependent: bool,
) -> Result<Vec<ProcessedMetaData>, String> {
    print!("\x1B[2K\rCollecting dependencies... 0%");
    let mut deps = Vec::new();
    let mut children = Vec::new();
    for metadata in metadatas {
        children.push(get_dep(metadata, sources, dependent));
    }
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        print!("\rCollecting dependencies... {}% ", i * 100 / count);
        let _ = std::io::stdout().flush();
        match child.into_future().await {
            Ok(dep) => deps.extend(dep),
            Err(faulty) => return Err(faulty),
        }
    }
    print!("\rCollecting dependencies... Done!");
    Ok(deps)
}

async fn get_dep(
    metadata: &ProcessedMetaData,
    sources: &[String],
    dependent: bool,
) -> Result<Vec<ProcessedMetaData>, String> {
    let mut deps = Vec::new();
    // These are important to the build process, so they need to be installed prior to
    // installing the dependant, so they get pushed lower down the dependency Vec
    // (lower means it will get installed earlier).
    for dep in &metadata.dependencies {
        if let Some(metadata) = dep.process(sources, dependent).await? {
            if let Some(i) = deps.iter().position(|x| *x == metadata) {
                deps.remove(i);
            }
            deps.push(metadata);
        }
    }
    // The dependant can still be built without this dependency, so order doesn't matter.
    for dep in &metadata.runtime_dependencies {
        if let Some(metadata) = dep.process(sources, dependent).await?
            && !deps.contains(&metadata)
        {
            deps.push(metadata);
        }
    }
    Ok(deps)
}

#[derive(PartialEq, Eq, Debug, Hash)]
struct ProcessedMetaData {
    name: String,
    kind: MetaDataKind,
    description: String,
    version: String,
    origin: OriginKind,
    dependent: bool,
    dependencies: Vec<DependKind>,
    runtime_dependencies: Vec<DependKind>,
    build: String,
    install: String,
    uninstall: String,
    hash: String,
}

impl ProcessedMetaData {
    pub fn to_installed(&self) -> InstalledVersion {
        InstalledVersion {
            version: self.version.to_string(),
            origin: self.origin.clone(),
            dependent: self.dependent,
            dependencies: self.dependencies.clone(),
            dependents: Vec::new(),
            uninstall: self.uninstall.to_string(),
            hash: self.hash.to_string(),
        }
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash)]
enum MetaDataKind {
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
enum OriginKind {
    Url(String),
    Github {
        user: String,
        repo: String,
        commit: String,
    },
}

#[derive(PartialEq, Eq, Serialize, Deserialize, Debug, Hash, Clone)]
enum DependKind {
    Latest(String),
    Specific { name: String, version: String },
    Volatile(String),
}

impl DependKind {
    pub async fn process(
        &self,
        sources: &[String],
        dependent: bool,
    ) -> Result<Option<ProcessedMetaData>, String> {
        match self {
            DependKind::Latest(latest) => {
                if let Some(data) = get_metadata(latest, None, sources, dependent).await {
                    Ok(Some(data))
                } else {
                    Err(latest.to_string())
                }
            }
            DependKind::Specific { name, version } => {
                if let Some(data) = get_metadata(name, Some(version), sources, dependent).await {
                    Ok(Some(data))
                } else {
                    Err(name.to_string())
                }
            }
            DependKind::Volatile(volatile) => {
                if let Ok(Some(status)) = RunCommand::new("which").status().map(|x| x.code()) {
                    if status != 0 {
                        Ok(None)
                    } else if let Some(data) =
                        get_metadata(volatile, None, sources, dependent).await
                    {
                        Ok(Some(data))
                    } else {
                        Err(volatile.to_string())
                    }
                } else {
                    Err(volatile.to_string())
                }
            }
        }
    }
    pub fn write_dependent(&self, theirname: &str, theirver: &str) -> Result<(), String> {
        let (name, ver) = match self {
            Self::Latest(latest) => (latest, None),
            Self::Specific { name, version } => (name, Some(version)),
            Self::Volatile(_) => return Ok(()),
        };
        let mut path = get_metadata_dir()?;
        path.push(format!("{name}.yaml"));
        if path.exists() && path.is_file() {
            let data = if let Ok(mut file) = File::open(&path) {
                let mut metadata = String::new();
                if file.read_to_string(&mut metadata).is_err() {
                    return Err(format!("Failed to read dependency {name}'s config!"));
                }
                let mut data = match serde_norway::from_str::<InstalledMetaData>(&metadata) {
                    Ok(data) => data,
                    Err(_) => return Err(format!("Failed to parse dependency {name}'s data!")),
                };
                if let Some(ver) = ver {
                    if let Some(bit) = data.installed.iter_mut().find(|x| x.version == *ver) {
                        bit.dependents.push(DependKind::Specific {
                            name: theirname.to_string(),
                            version: theirver.to_string(),
                        });
                    } else {
                        return Err(format!("{name} didn't contain version {ver}!"));
                    }
                } else {
                    data.installed.sort_by_key(|x| x.version.clone());
                    if let Some(bit) = data.installed.first_mut() {
                        bit.dependents.push(DependKind::Specific {
                            name: theirname.to_string(),
                            version: theirver.to_string(),
                        });
                    } else {
                        return Err(format!("{name} contained no versions!"));
                    }
                }
                data
            } else {
                return Err(format!("Failed to read dependency {name}'s metadata!"));
            };
            let mut file = match File::create(&path) {
                Ok(file) => file,
                Err(_) => {
                    return Err(format!(
                        "Failed to open dependency {name}'s metadata as WO!"
                    ));
                }
            };
            let data = match serde_norway::to_string(&data) {
                Ok(data) => data,
                Err(_) => {
                    return Err(format!(
                        "Failed to parse dependency {name}'s metadata to string!"
                    ));
                }
            };
            match file.write_all(data.as_bytes()) {
                Ok(_) => Ok(()),
                Err(_) => Err(format!(
                    "Failed to write to dependency {name}'s metadata file!"
                )),
            }
        } else {
            Err(format!("Cannot find data for dependency {name}!"))
        }
    }
}

#[derive(PartialEq, Deserialize, Serialize, Debug)]

struct InstalledMetaData {
    kind: MetaDataKind,
    installed: Vec<InstalledVersion>,
}

impl InstalledMetaData {
    pub fn write(&self, path: &Path) -> Result<(), String> {
        // if !path.exists() {
        //     if File::create_new(&path).is_err() {
        //         return Err(String::from("Failed to create file!"));
        //     }
        // }
        if !path.exists() || path.is_file() {
            let data = match serde_norway::to_string(self) {
                Ok(data) => data,
                Err(_) => {
                    return Err(String::from(
                        "Failed to parse InstalledMetaData into string!",
                    ));
                }
            };
            let mut file = match File::create(path) {
                Ok(file) => file,
                Err(_) => return Err(String::from("Failed to open file as WO!")),
            };
            match file.write_all(data.as_bytes()) {
                Ok(_) => Ok(()),
                Err(_) => Err(String::from("Failed to write to file!")),
            }
        } else {
            Err(String::from("File is of unexpected type!"))
        }
    }
}

#[derive(PartialEq, Deserialize, Serialize, Debug)]
struct InstalledVersion {
    version: String,
    origin: OriginKind,
    dependent: bool,
    dependencies: Vec<DependKind>,
    dependents: Vec<DependKind>,
    uninstall: String,
    hash: String,
}

// #[derive(PartialEq, Deserialize, Serialize, Debug)]
// struct InstalledDepend {
//     name: String,
//     version: Option<String>,
// }

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
                    DependKind::Specific {
                        name: name.to_string(),
                        version: ver.to_string(),
                    }
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
                    DependKind::Specific {
                        name: name.to_string(),
                        version: ver.to_string(),
                    }
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
            build: self.build,
            install: self.install,
            uninstall: self.uninstall,
            hash: self.hash,
        })
    }
}

fn install_pax(metadata: &ProcessedMetaData) -> Result<(), String> {
    let name = metadata.name.to_string();
    let (path, loaded_data) = check_type_conflicts(&metadata.name, &metadata.kind)?;
    let metadata = metadata.to_installed();
    let mut loaded_data = if let Some(data) = loaded_data {
        data
    } else {
        InstalledMetaData {
            kind: MetaDataKind::Pax,
            installed: Vec::new(),
        }
    };
    let deps = metadata.dependencies.clone();
    // let dependent = metadata.dependent;
    let ver = metadata.version.to_string();
    if !loaded_data
        .installed
        .iter()
        .any(|x| x.version == metadata.version)
    {
        loaded_data.installed.push(metadata);
    }
    loaded_data.write(&path)?;
    // if dependent {
    for dep in deps {
        dep.write_dependent(&name, &ver)?;
    }
    // }
    Ok(())
}

fn check_type_conflicts(
    name: &str,
    kind: &MetaDataKind,
) -> Result<(PathBuf, Option<InstalledMetaData>), String> {
    let mut path = get_metadata_dir()?;
    path.push(format!("{name}.yaml"));
    if !path.exists() {
        Ok((path, None))
    } else if path.is_file() {
        if let Ok(mut file) = File::open(&path) {
            let mut metadata = String::new();
            if file.read_to_string(&mut metadata).is_err() {
                return Err(format!("Failed to read {name}'s config!"));
            }
            let data = match serde_norway::from_str::<InstalledMetaData>(&metadata) {
                Ok(data) => data,
                Err(_) => return Err(String::from("Failed to parse data into InstalledMetaData!")),
            };
            if data.kind != *kind {
                Err(format!(
                    "Package is installed from {} but attempting to install from {kind}!",
                    data.kind
                ))
            } else {
                Ok((path, Some(data)))
            }
        } else {
            Err(format!("Failed to read {name}'s metadata!"))
        }
    } else {
        Err(format!("{name}'s metadata file is of unexpected type!"))
    }
}
