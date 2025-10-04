use metadata::{MetaDataKind, ProcessedMetaData, get_metadata};
use nix::unistd;
use settings::get_settings;
use std::{collections::HashSet, io::Write};
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
                    let name = package.name.to_string();
                    if let Err(message) = package.install_package(&sources, &runtime) {
                        println!("Error installing package {name}!\nReported error: `{message}`",);
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
    print!("\rCollecting dependencies... Done!");
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
        if let Ok(Some(metadata)) = dep.to_processed(sources).await? {
            if let Some(i) = deps.iter().position(|x| *x == metadata) {
                deps.remove(i);
            }
            deps.push(metadata);
        }
    }
    // The dependant can still be built without this dependency, so order doesn't matter.
    for dep in &metadata.runtime_dependencies {
        if let Ok(Some(metadata)) = dep.to_processed(sources).await?
            && !deps.contains(&metadata)
        {
            deps.push(metadata);
        }
    }
    Ok(deps)
}
