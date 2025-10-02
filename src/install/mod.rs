use std::{collections::HashSet, io::Write, process::Command as RunCommand};

use nix::unistd;
use serde::{Deserialize, Serialize};
use settings::get_settings;
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
    if let Ok(mut packages) = build_deps(args, &sources, &runtime, &mut HashSet::new()) {
        packages.reverse();
        for _package in packages {
            //Install that shit
        }
    };
    PostAction::Return
}

fn build_deps(
    args: &[String],
    sources: &[String],
    runtime: &Runtime,
    priordeps: &mut HashSet<ProcessedMetaData>,
) -> Result<Vec<ProcessedMetaData>, ()> {
    let mut metadatas = match runtime.block_on(get_metadatas(args, sources)) {
        Ok(data) => data,
        Err(faulty) => {
            println!("\x1B[2K\rFailed to locate package {faulty}.");
            return Err(());
        }
    };
    let deps_vec = match runtime.block_on(get_deps(&metadatas, sources)) {
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
        match build_deps(&diff, sources, runtime, priordeps) {
            Ok(data) => {
                for processed in data {
                    if !metadatas.contains(&processed) {
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
) -> Result<Vec<ProcessedMetaData>, String> {
    print!("\x1B[2K\rReading package lists... 0%");
    let mut metadatas = Vec::new();
    let mut children = Vec::new();
    for app in apps {
        children.push(get_metadata(app, None, sources));
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
    println!("\rReading package lists... Done!");
    Ok(metadatas)
}

async fn get_metadata(
    app: &str,
    version: Option<&str>,
    sources: &[String],
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
                && let Some(processed) = raw_pax.process()
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
) -> Result<Vec<ProcessedMetaData>, String> {
    print!("\x1B[2K\rCollecting dependencies... 0%");
    let mut deps = Vec::new();
    let mut children = Vec::new();
    for metadata in metadatas {
        children.push(get_dep(metadata, sources));
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
    println!("\rCollecting dependencies... Done!");
    Ok(deps)
}

async fn get_dep(
    metadata: &ProcessedMetaData,
    sources: &[String],
) -> Result<Vec<ProcessedMetaData>, String> {
    let mut deps = Vec::new();
    // These are important to the build process, so they need to be installed prior to
    // installing the dependant, so they get pushed lower down the dependency Vec
    // (lower means it will get installed earlier).
    for dep in &metadata.dependencies {
        if let Some(metadata) = dep.process(sources).await? {
            if let Some(i) = deps.iter().position(|x| *x == metadata) {
                deps.remove(i);
            }
            deps.push(metadata);
        }
    }
    // The dependant can still be built without this dependency, so order doesn't matter.
    for dep in &metadata.runtime_dependencies {
        if let Some(metadata) = dep.process(sources).await?
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
    description: String,
    version: String,
    origin: OriginKind,
    dependencies: Vec<DependKind>,
    runtime_dependencies: Vec<DependKind>,
    build: String,
    install: String,
    uninstall: String,
    hash: String,
}

#[derive(PartialEq, Eq, Deserialize, Serialize, Debug, Hash)]
enum OriginKind {
    Url(String),
    Github {
        user: String,
        repo: String,
        commit: String,
    },
}

#[derive(PartialEq, Eq, Debug, Hash)]
enum DependKind {
    Latest(String),
    Specific { name: String, version: String },
    Volatile(String),
}

impl DependKind {
    pub async fn process(&self, sources: &[String]) -> Result<Option<ProcessedMetaData>, String> {
        match self {
            DependKind::Latest(latest) => {
                if let Some(data) = get_metadata(latest, None, sources).await {
                    Ok(Some(data))
                } else {
                    Err(latest.to_string())
                }
            }
            DependKind::Specific { name, version } => {
                if let Some(data) = get_metadata(name, Some(version), sources).await {
                    Ok(Some(data))
                } else {
                    Err(name.to_string())
                }
            }
            DependKind::Volatile(volatile) => {
                if let Ok(Some(status)) = RunCommand::new("which").status().map(|x| x.code()) {
                    if status != 0 {
                        Ok(None)
                    } else if let Some(data) = get_metadata(volatile, None, sources).await {
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
}

#[derive(PartialEq, Deserialize, Serialize, Debug)]

struct _InstalledMetaData {
    installed: Vec<_InstalledVersion>,
}

#[derive(PartialEq, Deserialize, Serialize, Debug)]
struct _InstalledVersion {
    version: String,
    origin: OriginKind,
    dependencies: Vec<_InstalledDepend>,
    dependents: Vec<String>,
    uninstall: String,
    hash: String,
}

#[derive(PartialEq, Deserialize, Serialize, Debug)]
struct _InstalledDepend {
    name: String,
    version: Option<String>,
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
    pub fn process(self) -> Option<ProcessedMetaData> {
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
            description: self.description,
            version: self.version,
            origin,
            dependencies,
            runtime_dependencies,
            build: self.build,
            install: self.install,
            uninstall: self.uninstall,
            hash: self.hash,
        })
    }
}
