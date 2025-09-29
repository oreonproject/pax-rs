use std::{fs::File, io::Read, path::PathBuf};

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
    for app in args {
        let metadata = {
            let mut metadata = None;
            for source in sources.trim().split('\n') {
                metadata = runtime.block_on(get_metadata(source, app));
                if metadata.is_none() {
                    println!("[Bad]");
                    continue;
                }
            }
            if let Some(metadata) = metadata {
                metadata
            } else {
                println!("Cannot find specified package {app}!");
                return PostAction::Return;
            }
        };
        println!("[Ok]\n{metadata:?}");
    }
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

async fn get_metadata(source: &str, app: &str) -> Option<MetaData> {
    let endpoint = format!("{source}/packages/metadata/{app}");
    print!("GET {endpoint}... ");
    let body = reqwest::get(endpoint).await.ok()?.text().await.ok()?;
    println!("YES");
    serde_json::from_str::<MetaData>(&body).ok()
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
