use semver::Version;
use serde::{Deserialize, Serialize};
use settings::get_metadata_dir;
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command as RunCommand,
};
use tokio::runtime::Runtime;

#[derive(PartialEq, Eq, Debug, Hash)]
pub struct ProcessedMetaData {
    pub name: String,
    pub kind: MetaDataKind,
    pub description: String,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub dependencies: Vec<DependKind>,
    pub runtime_dependencies: Vec<DependKind>,
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub hash: String,
}

impl ProcessedMetaData {
    pub async fn to_installed(&self, sources: &[String]) -> InstalledVersion {
        InstalledVersion {
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
                    if let Ok(child) = child.into_future().await {
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
            uninstall: self.uninstall.to_string(),
            hash: self.hash.to_string(),
        }
    }
    pub fn install_package(self, sources: &[String], runtime: &Runtime) -> Result<(), String> {
        let kind = self.kind.clone();
        let name = self.name.to_string();
        let (path, loaded_data) = check_type_conflicts(&name, &kind)?;
        let metadata = runtime.block_on(self.to_installed(sources));
        let mut loaded_data = if let Some(data) = loaded_data {
            data
        } else {
            InstalledMetaData {
                name: name.clone(),
                kind,
                installed: Vec::new(),
            }
        };
        let deps = metadata.dependencies.clone();
        let ver = metadata.version.to_string();
        if !loaded_data
            .installed
            .iter()
            .any(|x| x.version == metadata.version)
        {
            loaded_data.installed.push(metadata);
        }
        loaded_data.write(&path)?;
        for dep in deps {
            dep.write_dependent(&name, &ver)?;
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
    Url(String),
    Github {
        user: String,
        repo: String,
        commit: String,
    },
}

pub async fn get_metadata(
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
pub enum PseudoProcessed {
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
    pub async fn to_processed(&self, sources: &[String]) -> Result<PseudoProcessed, String> {
        match self {
            DependKind::Latest(latest) => {
                if let Some(data) = get_metadata(latest, None, sources, true).await {
                    let mut path = get_metadata_dir()?;
                    path.push(format!("{}.yaml", &data.name));
                    if let Ok(mut file) = File::open(&path) {
                        let mut metadata = String::new();
                        if file.read_to_string(&mut metadata).is_ok()
                            && let Ok(mut subdata) =
                                serde_norway::from_str::<InstalledMetaData>(&metadata)
                        {
                            subdata.installed.sort_by_key(|x| {
                                Version::parse(&x.version).unwrap_or(Version::new(0, 0, 0))
                            });
                            if let Some(sub_latest) = subdata.installed.last()
                                && Version::parse(&sub_latest.version)
                                    .unwrap_or(Version::new(0, 0, 0))
                                    >= Version::parse(&data.version)
                                        .unwrap_or(Version::new(0, 0, 0))
                            {
                                return Ok(PseudoProcessed::Specific(Specific {
                                    name: data.name,
                                    version: sub_latest.version.to_string(),
                                }));
                            }
                        }
                    }
                    Ok(PseudoProcessed::MetaData(Box::new(data)))
                } else {
                    Err(latest.to_string())
                }
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
    name: String,
    version: String,
}

impl Specific {
    pub fn write_dependent(&self, their_name: &str, their_ver: &str) -> Result<(), String> {
        let mut path = get_metadata_dir()?;
        path.push(format!("{}.yaml", self.name));
        if path.exists() && path.is_file() {
            let data = if let Ok(mut file) = File::open(&path) {
                let mut metadata = String::new();
                if file.read_to_string(&mut metadata).is_err() {
                    return Err(format!(
                        "Failed to read dependency `{}`'s config!",
                        self.name
                    ));
                }
                let mut data = match serde_norway::from_str::<InstalledMetaData>(&metadata) {
                    Ok(data) => data,
                    Err(_) => {
                        return Err(format!(
                            "Failed to parse dependency `{}`'s data!",
                            self.name
                        ));
                    }
                };
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
                    }
                } else {
                    return Err(format!(
                        "`{}` didn't contain version {}!",
                        self.name, self.version
                    ));
                }

                data
            } else {
                return Err(format!(
                    "Failed to read dependency `{}`'s metadata!",
                    self.name
                ));
            };
            let mut file = match File::create(&path) {
                Ok(file) => file,
                Err(_) => {
                    return Err(format!(
                        "Failed to open dependency `{}`'s metadata as WO!",
                        self.name
                    ));
                }
            };
            let data = match serde_norway::to_string(&data) {
                Ok(data) => data,
                Err(_) => {
                    return Err(format!(
                        "Failed to parse dependency `{}`'s metadata to string!",
                        self.name
                    ));
                }
            };
            match file.write_all(data.as_bytes()) {
                Ok(_) => Ok(()),
                Err(_) => Err(format!(
                    "Failed to write to dependency `{}`'s metadata file!",
                    self.name
                )),
            }
        } else {
            Err(format!("Cannot find data for dependency `{}`!", self.name))
        }
    }
    pub fn _remove(&self, kind: &str) -> Result<(), String> {
        let mut path = get_metadata_dir()?;
        path.push(format!("{}.yaml", self.name));
        let mut data = if path.is_file() {
            if let Ok(mut file) = File::open(&path) {
                let mut metadata = String::new();
                if file.read_to_string(&mut metadata).is_err() {
                    return Err(format!("Failed to read {kind} `{}`'s config!", self.name));
                }
                match serde_norway::from_str::<InstalledMetaData>(&metadata) {
                    Ok(data) => data,
                    Err(_) => {
                        return Err(format!("Failed to parse {kind} `{}`'s data!", self.name));
                    }
                }
            } else {
                return Err(format!("Failed to read {kind} `{}`'s metadata!", self.name));
            }
        } else {
            return Err(format!(
                "Failed to locate {kind} `{}`'s metadata!",
                self.name
            ));
        };
        data._remove_package(&self.version)
    }
}

#[derive(PartialEq, Deserialize, Serialize, Debug)]

struct InstalledMetaData {
    name: String,
    kind: MetaDataKind,
    installed: Vec<InstalledVersion>,
}

impl InstalledMetaData {
    pub fn write(&self, path: &Path) -> Result<(), String> {
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
    pub fn _remove_package(&mut self, version: &str) -> Result<(), String> {
        let kind = self.kind.clone();
        let name = self.name.to_string();
        let (path, loaded_data) = check_type_conflicts(&name, &kind)?;
        let mut loaded_data = match loaded_data {
            Some(data) => data,
            None => return Err(format!("Package {name} is not installed!")),
        };
        // let (metadata, version) = if let Some(version) = version {
        let (metadata, version) = match self.installed.iter().find(|x| x.version == *version) {
            Some(metadata) => (metadata, version.to_string()),
            None => {
                return Err(format!(
                    "Failed to locate version {version} for package`{name}`."
                ));
            }
        };
        // } else {
        //     let installed = &mut self.installed;
        //     installed.sort_by_key(|x| {
        //         Version::parse(&x.version.clone()).unwrap_or(Version::new(0, 0, 0))
        //     });
        //     match installed.last() {
        //         Some(metadata) => (metadata, metadata.version.clone()),
        //         None => return Err(format!("Failed to locate package `{name}`.")),
        //     }
        // };
        if metadata.dependent {
            for dependent in &metadata.dependents {
                dependent._remove("dependent")?
            }
            Ok(())
        } else {
            for dependency in &metadata.dependencies {
                dependency._remove("dependency")?
            }
            if let Some(index) = loaded_data
                .installed
                .iter()
                .position(|x| x.version == version)
            {
                loaded_data.installed.remove(index);
            };
            if !loaded_data.installed.is_empty() {
                let mut file = match File::open(&path) {
                    Ok(file) => file,
                    Err(_) => return Err(format!("Failed to read `{name}`'s metadata!")),
                };
                let data = match serde_norway::to_string(&loaded_data) {
                    Ok(data) => data,
                    Err(_) => {
                        return Err(format!("Failed to parse `{name}`'s metadata into string!"));
                    }
                };
                match file.write_all(data.as_bytes()) {
                    Ok(_) => Ok(()),
                    Err(_) => Err(format!("Failed to write to `{name}`'s file!")),
                }
            } else if std::fs::remove_file(&path).is_err() {
                Err(format!("Failed to remove `{name}`'s metadata file!"))
            } else {
                Ok(())
            }
        }
    }
}

#[derive(PartialEq, Deserialize, Serialize, Debug, Clone)]
pub struct InstalledVersion {
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub dependencies: Vec<Specific>,
    pub dependents: Vec<Specific>,
    pub uninstall: String,
    pub hash: String,
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
            build: self.build,
            install: self.install,
            uninstall: self.uninstall,
            hash: self.hash,
        })
    }
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
                return Err(format!("Failed to read `{name}`'s config!"));
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
            Err(format!("Failed to read `{name}`'s metadata!"))
        }
    } else {
        Err(format!("`{name}`'s metadata file is of unexpected type!"))
    }
}
