use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

use crate::{Command, StateBox, command::PostAction};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        Vec::new(),
        None,
        install_packages,
        hierarchy,
    )
}

fn install_packages(_states: &StateBox, args: Option<&[String]>) -> PostAction {
    let args = match args {
        None => return PostAction::Return,
        Some(args) => args,
    };
    let sources = match get_sources() {
        None => return PostAction::PullSources,
        Some(sources) => sources,
    };
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(_) => {
            println!("Error creating runtime!");
            return PostAction::Return;
        }
    };
    let metadatas = match runtime.block_on(get_metadatas(&sources, args)) {
        Ok(data) => data,
        Err(faulty) => {
            println!("\rFailed to locate package {faulty}.");
            return PostAction::Return;
        }
    };
    println!("{metadatas:?}");
    PostAction::Return
}

fn get_sources() -> Option<String> {
    println!("Reading sources...");
    let path = PathBuf::from("/etc/pax.d/sources.txt");
    let mut file = File::open(&path).ok()?;
    let mut sources = String::new();
    file.read_to_string(&mut sources).ok()?;
    Some(sources)
}

async fn get_metadatas(sources: &str, apps: &[String]) -> Result<Vec<MetaData>, String> {
    print!("Reading metadata (0%)... ");
    let mut metadatas = Vec::new();
    let mut children = Vec::new();
    for app in apps {
        children.push(get_metadata(sources, app));
    }
    let count = children.len();
    for (i, child) in children.into_iter().enumerate() {
        print!("\rReading metadata ({}%)... ", i * 100 / count);
        let _ = std::io::stdout().flush();
        if let Some(child) = child.into_future().await {
            metadatas.push(child.0);
        } else {
            return Err(apps[i].to_string());
        }
    }
    println!("\rReading metadata (100%)... Done!");
    Ok(metadatas)
}

async fn get_metadata(sources: &str, app: &str) -> Option<(MetaData, usize)> {
    let mut metadata = None;
    for (i, source) in sources.trim().split('\n').enumerate() {
        metadata = {
            let endpoint = format!("{source}/packages/metadata/{app}");
            let body = reqwest::get(endpoint).await.ok()?.text().await.ok()?;
            Some((serde_json::from_str::<MetaData>(&body).ok()?, i))
        };
    }
    metadata
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
    binary: String,
    install: String,
    uninstall: String,
}
