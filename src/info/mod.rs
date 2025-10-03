use crate::database::Database;
use crate::repository::create_client_from_settings;
use crate::{Command, PostAction, StateBox};
use settings::get_settings;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "info",
        vec![String::from("show")],
        "Show detailed information about a package",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    let args = match args {
        None => {
            println!("Usage: pax info <package>");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    if args.is_empty() {
        println!("Usage: pax info <package>");
        return PostAction::Return;
    }

    let pkg_name = &args[0];

    // open database
    let db = match Database::open("/opt/pax/db/pax.db") {
        Ok(db) => db,
        Err(e) => {
            println!("Failed to open database: {}", e);
            return PostAction::Return;
        }
    };

    // check if installed
    let is_installed = db.is_installed(pkg_name).unwrap_or(false);

    if is_installed {
        // show installed package info
        show_installed_info(pkg_name, &db);
    } else {
        // search in repositories
        show_repository_info(pkg_name);
    }

    PostAction::Return
}

fn show_installed_info(pkg_name: &str, db: &Database) {
    let pkg_info = match db.get_package_info(pkg_name) {
        Ok(Some(info)) => info,
        Ok(None) => {
            println!("Package not found: {}", pkg_name);
            return;
        }
        Err(e) => {
            println!("Failed to get package info: {}", e);
            return;
        }
    };

    let pkg_id = db.get_package_id(pkg_name).unwrap().unwrap();

    println!("\x1B[36mPackage:\x1B[0m {}", pkg_info.name);
    println!("\x1B[36mVersion:\x1B[0m {}", pkg_info.version);
    println!("\x1B[36mDescription:\x1B[0m {}", pkg_info.description);
    println!("\x1B[36mOrigin:\x1B[0m {}", pkg_info.origin);
    println!("\x1B[36mStatus:\x1B[0m \x1B[32minstalled\x1B[0m");
    
    // format size
    let size_mb = pkg_info.size as f64 / 1024.0 / 1024.0;
    println!("\x1B[36mInstalled Size:\x1B[0m {:.2} MB", size_mb);

    // format install date
    let install_date = chrono::DateTime::from_timestamp(pkg_info.install_date, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("\x1B[36mInstall Date:\x1B[0m {}", install_date);

    println!("\x1B[36mHash:\x1B[0m {}", pkg_info.hash);

    // get dependencies
    if let Ok(deps) = db.get_dependencies(pkg_id) {
        if !deps.is_empty() {
            println!("\n\x1B[36mDependencies:\x1B[0m");
            for dep in deps {
                let constraint = dep.version_constraint
                    .map(|v| format!(" ({})", v))
                    .unwrap_or_default();
                println!("  - {}{}", dep.depends_on, constraint);
            }
        }
    }

    // get files
    if let Ok(files) = db.get_package_files(pkg_id) {
        println!("\n\x1B[36mFiles:\x1B[0m {} files", files.len());
        if files.len() <= 20 {
            for file in files {
                println!("  {}", file.path);
            }
        } else {
            println!("  (use 'pax list-files {}' to see all files)", pkg_name);
        }
    }
}

fn show_repository_info(pkg_name: &str) {
    // load settings
    let settings = match get_settings() {
        Ok(s) => s,
        Err(e) => {
            println!("Failed to load settings: {}", e);
            return;
        }
    };

    // initialize repository client
    let repo_client = match create_client_from_settings(&settings) {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to create repository client: {}", e);
            return;
        }
    };

    // search for package
    match repo_client.search_package(pkg_name) {
        Ok(Some((source, entry))) => {
            println!("\x1B[36mPackage:\x1B[0m {}", entry.name);
            println!("\x1B[36mVersion:\x1B[0m {}", entry.version);
            println!("\x1B[36mDescription:\x1B[0m {}", entry.description);
            println!("\x1B[36mRepository:\x1B[0m {}", source);
            println!("\x1B[36mStatus:\x1B[0m \x1B[33mnot installed\x1B[0m");
            
            let size_mb = entry.size as f64 / 1024.0 / 1024.0;
            println!("\x1B[36mDownload Size:\x1B[0m {:.2} MB", size_mb);

            if !entry.dependencies.is_empty() {
                println!("\n\x1B[36mDependencies:\x1B[0m");
                for dep in entry.dependencies {
                    println!("  - {}", dep);
                }
            }

            if !entry.runtime_dependencies.is_empty() {
                println!("\n\x1B[36mRuntime Dependencies:\x1B[0m");
                for dep in entry.runtime_dependencies {
                    println!("  - {}", dep);
                }
            }

            if !entry.provides.is_empty() {
                println!("\n\x1B[36mProvides:\x1B[0m");
                for provide in entry.provides {
                    println!("  - {}", provide);
                }
            }

            println!("\n\x1B[36mHash:\x1B[0m {}", entry.hash);
            println!("\n\x1B[2mInstall with: pax install {}\x1B[0m", entry.name);
        }
        Ok(None) => {
            println!("Package not found: {}", pkg_name);
        }
        Err(e) => {
            println!("Failed to search for package: {}", e);
        }
    }
}

