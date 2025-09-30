use std::{collections::HashSet, io::Write, process::Command as RunCommand};

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
    priordeps: &mut HashSet<String>,
) -> Result<Vec<MetaData>, ()> {
    let mut metadatas = match runtime.block_on(get_metadatas(sources, args)) {
        Ok(data) => data,
        Err(faulty) => {
            println!("\x1B[2K\rFailed to locate package {faulty}.");
            return Err(());
        }
    };
    let deps_vec = match runtime.block_on(get_deps(&metadatas)) {
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
        .collect::<Vec<&String>>()
        .iter()
        .map(|x| x.to_string())
        .collect::<Vec<String>>();
    priordeps.extend(deps);
    if !diff.is_empty() {
        match build_deps(&diff, sources, runtime, priordeps) {
            Ok(data) => metadatas.extend(data),
            Err(()) => return Err(()),
        }
    }
    Ok(metadatas)
}

async fn get_metadatas(sources: &[String], apps: &[String]) -> Result<Vec<MetaData>, String> {
    print!("\x1B[2K\rReading package lists... 0%");
    let mut metadatas = Vec::new();
    let mut children = Vec::new();
    for app in apps {
        children.push(get_metadata(sources, app));
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

async fn get_metadata(sources: &[String], app: &str) -> Option<MetaData> {
    let mut metadata = None;
    for source in sources {
        metadata = {
            let endpoint = format!("{source}/packages/metadata/{app}");
            let body = reqwest::get(endpoint).await.ok()?.text().await.ok()?;
            Some(serde_json::from_str::<MetaData>(&body).ok()?)
        };
    }
    metadata
}
async fn get_deps(metadatas: &[MetaData]) -> Result<Vec<String>, String> {
    print!("\x1B[2K\rCollecting dependencies... 0%");
    let mut deps = Vec::new();
    let mut children = Vec::new();
    for metadata in metadatas {
        children.push(get_dep(metadata));
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

async fn get_dep(metadata: &MetaData) -> Result<Vec<String>, String> {
    let mut deps = Vec::new();
    // These are important to the build process, so they need to be installed prior to
    // installing the dependant, so they get pushed lower down the dependency Vec
    // (lower means it will get installed earlier).
    for dep in &metadata.dependencies {
        if let Ok(Some(status)) = RunCommand::new("which").arg(dep).status().map(|x| x.code()) {
            if status != 0 {
                if let Some(i) = deps.iter().position(|x| x == dep) {
                    deps.remove(i);
                }
                deps.push(dep.to_string());
            }
        } else {
            return Err(dep.to_string());
        }
    }
    // The dependant can still be built without this dependency, so order doesn't matter.
    for dep in &metadata.runtime_dependencies {
        if let Ok(Some(status)) = RunCommand::new("which").arg(dep).status().map(|x| x.code()) {
            if status != 0 && !deps.iter().any(|x| x == dep) {
                deps.push(dep.to_string());
            }
        } else {
            return Err(dep.to_string());
        }
    }
    Ok(deps)
}

// async fn(){}

#[derive(PartialEq, Serialize, Deserialize, Debug)]
struct MetaData {
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
