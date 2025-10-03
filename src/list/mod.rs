use crate::database::Database;
use crate::{Command, PostAction, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "list",
        vec![String::from("ls")],
        "List installed packages",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, _args: Option<&[String]>) -> PostAction {
    // open database
    let db = match Database::open("/opt/pax/db/pax.db") {
        Ok(db) => db,
        Err(e) => {
            println!("Failed to open database: {}", e);
            return PostAction::Return;
        }
    };

    // list packages
    let packages = match db.list_packages() {
        Ok(pkgs) => pkgs,
        Err(e) => {
            println!("Failed to list packages: {}", e);
            return PostAction::Return;
        }
    };

    if packages.is_empty() {
        println!("No packages installed");
        return PostAction::Return;
    }

    println!("\n\x1B[36mInstalled Packages:\x1B[0m\n");
    
    let mut total_size = 0u64;
    
    for pkg in &packages {
        // format size
        let size_mb = pkg.size as f64 / 1024.0 / 1024.0;
        total_size += pkg.size as u64;
        
        println!("\x1B[33m{}\x1B[0m {} ({:.2} MB)", pkg.name, pkg.version, size_mb);
        
        // truncate long descriptions
        let desc = if pkg.description.len() > 70 {
            format!("{}...", &pkg.description[..67])
        } else {
            pkg.description.clone()
        };
        
        println!("  {}", desc);
        println!("  Origin: {}", pkg.origin);
        println!();
    }

    let total_mb = total_size as f64 / 1024.0 / 1024.0;
    println!("\x1B[36mTotal:\x1B[0m {} packages ({:.2} MB)", packages.len(), total_mb);

    PostAction::Return
}

