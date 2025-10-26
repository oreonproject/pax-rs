use std::{
    env,
    path::{Path, PathBuf},
    process::exit,
};

use pax_builder::{PaxPackageBuilder, BuiltPackage, TargetArch};

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        print_usage();
        exit(1);
    }

    let command = &args[1];
    
    match command.as_str() {
        "build" => {
            if args.len() < 3 {
                eprintln!("Error: Package specification file required");
                print_usage();
                exit(1);
            }
            
            let spec_path = Path::new(&args[2]);
            let verbose = args.contains(&"--verbose".to_string()) || args.contains(&"-v".to_string());
            
            // Parse --target flag
            let target_arch = parse_target_flag(&args);
            
            // Parse --output-dir flag
            let output_dir = parse_output_dir_flag(&args);
            
            // Parse --no-bubblewrap flag
            let use_bubblewrap = !args.contains(&"--no-bubblewrap".to_string());
            
            match build_package(spec_path, verbose, target_arch, output_dir, use_bubblewrap) {
                Ok(built_packages) => {
                    println!("Package built successfully!");
                    for built_package in built_packages {
                        println!("Package: {}", built_package.package_path.display());
                        println!("Size: {} bytes", built_package.size);
                        println!("Checksum: {}", built_package.checksum);
                        println!("Build time: {} seconds", built_package.build_duration);
                        println!(); // Empty line between packages
                    }
                }
                Err(e) => {
                    eprintln!("Build failed: {}", e);
                    exit(1);
                }
            }
        }
        "validate" => {
            if args.len() < 3 {
                eprintln!("Error: Package specification file required");
                print_usage();
                exit(1);
            }
            
            let spec_path = Path::new(&args[2]);
            
            match validate_spec(spec_path) {
                Ok(errors) => {
                    if errors.is_empty() {
                        println!("Package specification is valid!");
                    } else {
                        println!("Package specification has errors:");
                        for error in errors {
                            println!("  • {}", error);
                        }
                        exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("Validation failed: {}", e);
                    exit(1);
                }
            }
        }
        "init" => {
            if args.len() < 3 {
                eprintln!("Error: Package name required");
                print_usage();
                exit(1);
            }
            
            let package_name = &args[2];
            let output_dir = args.get(3).map(|s| Path::new(s)).unwrap_or(Path::new("."));
            
            match init_package(package_name, output_dir) {
                Ok(_) => {
                    println!("Package template created for: {}", package_name);
                    println!("Edit the pax.yaml file and run 'pax-builder build pax.yaml'");
                }
                Err(e) => {
                    eprintln!("Failed to create package template: {}", e);
                    exit(1);
                }
            }
        }
        "clean" => {
            match clean_build_directory() {
                Ok(_) => {
                    println!("Build directory cleaned");
                }
                Err(e) => {
                    eprintln!("Failed to clean build directory: {}", e);
                    exit(1);
                }
            }
        }
        "help" | "--help" | "-h" => {
            print_usage();
        }
        _ => {
            eprintln!("Error: Unknown command '{}'", command);
            print_usage();
            exit(1);
        }
    }
}

fn parse_target_flag(args: &[String]) -> Option<TargetArch> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--target" && i + 1 < args.len() {
            return TargetArch::from_str(&args[i + 1]);
        }
    }
    None
}

fn parse_output_dir_flag(args: &[String]) -> Option<PathBuf> {
    for (i, arg) in args.iter().enumerate() {
        if arg == "--output-dir" && i + 1 < args.len() {
            return Some(PathBuf::from(&args[i + 1]));
        }
    }
    None
}

fn build_package(spec_path: &Path, verbose: bool, target_arch: Option<TargetArch>, output_dir: Option<PathBuf>, use_bubblewrap: bool) -> Result<Vec<BuiltPackage>, String> {
    let mut builder = PaxPackageBuilder::new()?.with_verbose(verbose);
    
    // Set target architecture if specified
    if let Some(target) = target_arch {
        builder = builder.with_target_arch(target)?;
    }
    
    // Set output directory if specified
    if let Some(output) = output_dir {
        builder = builder.with_output_directory(output);
    }
    
    // Set bubblewrap usage
    builder = builder.with_bubblewrap(use_bubblewrap);
    
    // Validate spec first
    let errors = builder.validate_spec(spec_path)?;
    if !errors.is_empty() {
        return Err(format!("Package specification validation failed: {:?}", errors));
    }
    
    builder.build_package(spec_path)
}

fn validate_spec(spec_path: &Path) -> Result<Vec<String>, String> {
    let builder = PaxPackageBuilder::new()?;
    builder.validate_spec(spec_path)
}

fn init_package(package_name: &str, output_dir: &Path) -> Result<(), String> {
    use std::fs::File;
    use std::io::Write;
    
    let spec_content = format!(r#"name: {}
version: "1.0.0"
description: "A PAX package"
author: "Your Name"
license: "MIT"
homepage: "https://example.com"
repository: "https://github.com/user/repo"
keywords:
  - example
  - package
categories:
  - development
  - tools

dependencies:
  build_dependencies:
    - name: "gcc"
      version_constraint: ">=7.0"
      optional: false
  runtime_dependencies:
    - name: "glibc"
      version_constraint: ">=2.17"
      optional: false
  optional_dependencies: []
  conflicts: []

build:
  build_system: Make
  build_commands:
    - "make"
    - "make install"
  build_dependencies:
    - "gcc"
    - "make"
  build_flags: []
  environment:
    CC: "gcc"
    CFLAGS: "-O2"
  working_directory: null
  target_architectures:
    - X86_64v1
    - X86_64v3
    - Aarch64
  cross_compiler_prefix: null
  target_sysroot: null

install:
  install_method: RunCommands
  install_commands:
    - "make install"
  install_directories:
    - "/usr/local/bin"
    - "/usr/local/lib"
  install_files: []
  post_install_commands: []

files:
  include_patterns:
    - "src/**/*"
    - "include/**/*"
    - "Makefile"
    - "README.md"
  exclude_patterns:
    - "**/*.o"
    - "**/*.a"
    - "**/*.so"
    - "target/**/*"
    - "node_modules/**/*"
  binary_files:
    - "bin/*"
  config_files:
    - "etc/*"
  documentation_files:
    - "doc/**/*"
    - "README.md"
    - "LICENSE"
  license_files:
    - "LICENSE"
    - "COPYING"

scripts:
  pre_install: null
  post_install: |
    echo "Package installed successfully"
  pre_uninstall: null
  post_uninstall: |
    echo "Package uninstalled successfully"
  pre_upgrade: null
  post_upgrade: null

metadata:
  maintainer: "Your Name <your.email@example.com>"
  section: "devel"
  priority: "optional"
"#, package_name);

    let spec_file = output_dir.join("pax.yaml");
    let mut file = File::create(&spec_file)
        .map_err(|_| format!("Failed to create spec file: {}", spec_file.display()))?;
    
    file.write_all(spec_content.as_bytes())
        .map_err(|_| format!("Failed to write spec file: {}", spec_file.display()))?;

    // Create a basic Makefile
    let makefile_content = format!(r#"# Makefile for {}

PREFIX ?= /usr/local
BINDIR = $(PREFIX)/bin
LIBDIR = $(PREFIX)/lib

all: {}

{}:
	@echo "Building {}..."
	# Add your build commands here
	@echo "Build complete"

install: {}
	@echo "Installing {}..."
	# Add your install commands here
	@echo "Installation complete"

clean:
	@echo "Cleaning..."
	# Add your clean commands here
	@echo "Clean complete"

.PHONY: all {} install clean
"#, package_name, package_name, package_name, package_name, package_name, package_name, package_name);

    let makefile = output_dir.join("Makefile");
    let mut file = File::create(&makefile)
        .map_err(|_| format!("Failed to create Makefile: {}", makefile.display()))?;
    
    file.write_all(makefile_content.as_bytes())
        .map_err(|_| format!("Failed to write Makefile: {}", makefile.display()))?;

    // Create a basic README
    let readme_content = format!(r#"# {}

A PAX package.

## Installation

```bash
pax install {}
```

## Building from Source

```bash
pax-builder build pax.yaml
```

## License

MIT
"#, package_name, package_name);

    let readme = output_dir.join("README.md");
    let mut file = File::create(&readme)
        .map_err(|_| format!("Failed to create README: {}", readme.display()))?;
    
    file.write_all(readme_content.as_bytes())
        .map_err(|_| format!("Failed to write README: {}", readme.display()))?;

    Ok(())
}

fn clean_build_directory() -> Result<(), String> {
    let builder = PaxPackageBuilder::new()?;
    builder.clean_build_directory()
}

fn print_usage() {
    println!(r#"PAX Package Builder

USAGE:
    pax-builder <COMMAND> [OPTIONS]

COMMANDS:
    build <spec>     Build a package from a specification file
    validate <spec>  Validate a package specification file
    init <name>      Create a new package template
    clean            Clean the build directory
    help             Show this help message

OPTIONS:
    -v, --verbose        Enable verbose output
    --target <arch>      Cross-compile for target architecture (x86_64v1, x86_64v3, aarch64, armv7l, riscv64, etc.)
    --output-dir <dir>   Specify output directory for packages (default: current directory)
    --no-bubblewrap      Disable bubblewrap build isolation

EXAMPLES:
    pax-builder init my-package
    pax-builder validate pax.yaml
    pax-builder build pax.yaml --verbose
    pax-builder build pax.yaml --target x86_64v3
    pax-builder build pax.yaml --output-dir ./packages
    pax-builder build pax.yaml --no-bubblewrap
    pax-builder clean

For more information, visit: https://github.com/your-org/pax-rs
"#);
}
