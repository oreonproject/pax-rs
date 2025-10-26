use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
    os::unix::fs::PermissionsExt,
};

use utils::{err, Version};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaxPackageSpec {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub dependencies: PackageDependencies,
    pub build: BuildConfig,
    pub install: InstallConfig,
    pub files: FileConfig,
    pub scripts: ScriptConfig,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDependencies {
    pub build_dependencies: Vec<Dependency>,
    pub runtime_dependencies: Vec<Dependency>,
    pub optional_dependencies: Vec<Dependency>,
    pub conflicts: Vec<Dependency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    pub version_constraint: String,
    pub optional: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    pub build_system: BuildSystem,
    pub build_commands: Vec<String>,
    pub build_dependencies: Vec<String>,
    pub build_flags: Vec<String>,
    pub environment: HashMap<String, String>,
    pub working_directory: Option<String>,
    pub target_architectures: Vec<TargetArch>,
    pub cross_compiler_prefix: Option<String>,
    pub target_sysroot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TargetArch {
    X86_64,
    X86_64v1,
    X86_64v3,
    Aarch64,
    Armv7l,
    Armv8l,
    Riscv64,
    Powerpc64le,
    S390x,
}

impl TargetArch {
    pub fn to_triple(&self) -> &'static str {
        match self {
            TargetArch::X86_64 => "x86_64-unknown-linux-gnu",
            TargetArch::X86_64v1 => "x86_64-unknown-linux-gnu",
            TargetArch::X86_64v3 => "x86_64-unknown-linux-gnu",
            TargetArch::Aarch64 => "aarch64-unknown-linux-gnu",
            TargetArch::Armv7l => "armv7-unknown-linux-gnueabihf",
            TargetArch::Armv8l => "aarch64-unknown-linux-gnu",
            TargetArch::Riscv64 => "riscv64gc-unknown-linux-gnu",
            TargetArch::Powerpc64le => "powerpc64le-unknown-linux-gnu",
            TargetArch::S390x => "s390x-unknown-linux-gnu",
        }
    }

    pub fn cross_compiler_prefix(&self) -> &'static str {
        match self {
            TargetArch::X86_64 => "x86_64-linux-gnu-",
            TargetArch::X86_64v1 => "x86_64-linux-gnu-",
            TargetArch::X86_64v3 => "x86_64-linux-gnu-",
            TargetArch::Aarch64 => "aarch64-linux-gnu-",
            TargetArch::Armv7l => "arm-linux-gnueabihf-",
            TargetArch::Armv8l => "aarch64-linux-gnu-",
            TargetArch::Riscv64 => "riscv64-linux-gnu-",
            TargetArch::Powerpc64le => "powerpc64le-linux-gnu-",
            TargetArch::S390x => "s390x-linux-gnu-",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "x86_64" | "amd64" => Some(TargetArch::X86_64),
            "x86_64v1" | "x86_64_v1" => Some(TargetArch::X86_64v1),
            "x86_64v3" | "x86_64_v3" => Some(TargetArch::X86_64v3),
            "aarch64" | "arm64" => Some(TargetArch::Aarch64),
            "armv7l" | "armv7" => Some(TargetArch::Armv7l),
            "armv8l" => Some(TargetArch::Armv8l),
            "riscv64" => Some(TargetArch::Riscv64),
            "powerpc64le" | "ppc64le" => Some(TargetArch::Powerpc64le),
            "s390x" => Some(TargetArch::S390x),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossCompileConfig {
    pub target_arch: TargetArch,
    pub compiler_prefix: String,
    pub sysroot: Option<String>,
    pub environment: HashMap<String, String>,
    pub build_flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BuildSystem {
    Make,
    CMake,
    Meson,
    Cargo,
    Npm,
    Pip,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallConfig {
    pub install_method: InstallMethod,
    pub install_commands: Vec<String>,
    pub install_directories: Vec<String>,
    pub install_files: Vec<FileMapping>,
    pub post_install_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InstallMethod {
    CopyFiles,
    RunCommands,
    ExtractArchive,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMapping {
    pub source: String,
    pub destination: String,
    pub permissions: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConfig {
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
    pub binary_files: Vec<String>,
    pub config_files: Vec<String>,
    pub documentation_files: Vec<String>,
    pub license_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptConfig {
    pub pre_install: Option<String>,
    pub post_install: Option<String>,
    pub pre_uninstall: Option<String>,
    pub post_uninstall: Option<String>,
    pub pre_upgrade: Option<String>,
    pub post_upgrade: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltPackage {
    pub spec: PaxPackageSpec,
    pub package_path: PathBuf,
    pub build_log: String,
    pub checksum: String,
    pub size: u64,
    pub build_time: u64,
    pub build_duration: u64,
}

pub struct PaxPackageBuilder {
    build_directory: PathBuf,
    output_directory: PathBuf,
    temp_directory: PathBuf,
    verbose: bool,
    target_arch: Option<TargetArch>,
    use_bubblewrap: bool,
    buildroot_directory: PathBuf,
    host_arch: String,
}

impl PaxPackageBuilder {
    pub fn new() -> Result<Self, String> {
        // Detect host architecture
        let host_arch = Self::detect_host_architecture()?;
        
        // Use user-specific directories to avoid permission issues
        let _user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let base_dir = PathBuf::from(&home_dir).join(".local/share/pax-builder");
        
        let build_dir = base_dir.join("build");
        let output_dir = PathBuf::from("results"); // Use local results directory
        let buildroot_dir = base_dir.join("buildroot");
        let temp_dir = base_dir.join("temp");

        // Create directories with proper permissions
        Self::create_directory_with_permissions(&build_dir)?;
        Self::create_directory_with_permissions(&output_dir)?;
        Self::create_directory_with_permissions(&buildroot_dir)?;
        Self::create_directory_with_permissions(&temp_dir)?;

        Ok(Self {
            build_directory: build_dir,
            output_directory: output_dir,
            temp_directory: temp_dir,
            verbose: false,
            target_arch: None,
            use_bubblewrap: true,
            buildroot_directory: buildroot_dir,
            host_arch,
        })
    }

    fn detect_host_architecture() -> Result<String, String> {
        let arch = std::env::consts::ARCH;
        match arch {
            "x86_64" => Ok("x86_64".to_string()),
            "aarch64" => Ok("aarch64".to_string()),
            "arm" => Ok("armv7l".to_string()),
            "riscv64" => Ok("riscv64".to_string()),
            "powerpc64le" => Ok("powerpc64le".to_string()),
            "s390x" => Ok("s390x".to_string()),
            _ => Err(format!("Unsupported host architecture: {}", arch)),
        }
    }

    fn create_directory_with_permissions(path: &Path) -> Result<(), String> {
        fs::create_dir_all(path)
            .map_err(|_| format!("Failed to create directory: {}", path.display()))?;
        
        // Set permissions to 755 (rwxr-xr-x)
        let mut perms = fs::metadata(path)
            .map_err(|_| format!("Failed to get metadata for: {}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .map_err(|_| format!("Failed to set permissions for: {}", path.display()))?;
        
        Ok(())
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    pub fn with_target_arch(mut self, target_arch: TargetArch) -> Result<Self, String> {
        // Validate that the target architecture matches the host architecture
        let target_arch_str = match target_arch {
            TargetArch::X86_64 | TargetArch::X86_64v1 | TargetArch::X86_64v3 => "x86_64",
            TargetArch::Aarch64 => "aarch64",
            TargetArch::Armv7l => "armv7l",
            TargetArch::Armv8l => "aarch64",
            TargetArch::Riscv64 => "riscv64",
            TargetArch::Powerpc64le => "powerpc64le",
            TargetArch::S390x => "s390x",
        };

        if target_arch_str != self.host_arch {
            return Err(format!(
                "Target architecture {} is not supported on host architecture {}. \
                PAX builder only supports native builds. Please build on a {} machine or \
                remove {} from target_architectures in your pax.yaml",
                target_arch_str, self.host_arch, target_arch_str, target_arch_str
            ));
        }

        self.target_arch = Some(target_arch);
        Ok(self)
    }

    pub fn with_bubblewrap(mut self, use_bwrap: bool) -> Self {
        self.use_bubblewrap = use_bwrap;
        self
    }

    pub fn with_output_directory(mut self, output_dir: PathBuf) -> Self {
        self.output_directory = output_dir;
        self
    }

    fn setup_native_buildroot(&self, target_arch: &TargetArch) -> Result<PathBuf, String> {
        let arch_name = match target_arch {
            TargetArch::X86_64 => "x86_64",
            TargetArch::X86_64v1 => "x86_64v1",
            TargetArch::X86_64v3 => "x86_64v3",
            TargetArch::Aarch64 => "aarch64",
            TargetArch::Armv7l => "armv7l",
            TargetArch::Armv8l => "armv8l",
            TargetArch::Riscv64 => "riscv64",
            TargetArch::Powerpc64le => "powerpc64le",
            TargetArch::S390x => "s390x",
        };
        let buildroot_path = self.buildroot_directory.join(arch_name);
        
        if !buildroot_path.exists() {
            if self.verbose {
                println!("Setting up native buildroot for {}", arch_name);
            }
            
            // Create buildroot structure
            self.create_native_buildroot_structure(&buildroot_path)?;
        }
        
        Ok(buildroot_path)
    }

    fn create_native_buildroot_structure(&self, buildroot_path: &Path) -> Result<(), String> {
        // Create essential directories
        let dirs = vec![
            "usr/bin", "usr/lib", "usr/include", "usr/share",
            "lib", "lib64", "bin", "sbin", "etc", "var", "tmp",
            "usr/local/bin", "usr/local/lib", "usr/local/include",
        ];

        for dir in dirs {
            let dir_path = buildroot_path.join(dir);
            Self::create_directory_with_permissions(&dir_path)?;
        }

        // Create symlinks for compatibility
        self.create_buildroot_symlinks(buildroot_path)?;

        Ok(())
    }

    fn create_buildroot_symlinks(&self, buildroot_path: &Path) -> Result<(), String> {
        // Create essential symlinks for build environment
        let symlinks = vec![
            ("usr/lib64", "usr/lib"),
            ("lib64", "lib"),
        ];

        for (target, link) in symlinks {
            let target_path = buildroot_path.join(target);
            let link_path = buildroot_path.join(link);
            
            if target_path.exists() && !link_path.exists() {
                std::os::unix::fs::symlink(target, &link_path)
                    .map_err(|_| format!("Failed to create symlink: {} -> {}", link_path.display(), target))?;
            }
        }

        Ok(())
    }

    pub fn build_package(&mut self, spec_path: &Path) -> Result<Vec<BuiltPackage>, String> {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Load package specification
        let spec = self.load_spec(spec_path)?;
        
        if self.verbose {
            println!("Building package: {} v{}", spec.name, spec.version);
            println!("Host architecture: {}", self.host_arch);
        }

        // Determine target architectures - only native builds
        let target_archs = self.determine_native_target_architectures(&spec)?;

        if self.verbose {
            println!("Building for architectures: {:?}", target_archs);
        }

        // Build for each target architecture
        let mut packages = Vec::new();
        let mut first_build_dir = None;
        
        for target_arch in target_archs {
            if self.verbose {
                println!("Building for target: {}", target_arch.to_triple());
            }

            // Create isolated build directory
            let build_dir = self.create_isolated_build_dir(&spec, &target_arch)?;
            
            // Store first build directory for source package
            if first_build_dir.is_none() {
                first_build_dir = Some(build_dir.clone());
            }
            
            // Clone source code
            self.clone_source_code(&spec, &build_dir)?;

            // Setup buildroot for isolated build
            let buildroot_path = self.setup_native_buildroot(&target_arch)?;

            // Run build process with proper isolation
            let build_log = self.run_isolated_build(&spec, &build_dir, &buildroot_path, &target_arch)?;

            // Create package archives
            let binary_package = self.create_binary_package(&spec, &build_dir, &target_arch)?;

            // Calculate checksum and size
            let checksum = self.calculate_checksum(&binary_package)?;
            let size = fs::metadata(&binary_package)
                .map_err(|_| "Failed to get package metadata")?
                .len();

            packages.push(BuiltPackage {
                spec: spec.clone(),
                package_path: binary_package.clone(),
                build_log,
                checksum,
                size,
                build_time: start_time,
                build_duration: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() - start_time,
            });

            if self.verbose {
                println!("Created binary package: {}", binary_package.display());
            }
        }

        // Create single source package (same for all architectures)
        if let Some(build_dir) = first_build_dir {
            let src_package = self.create_source_package(&spec, &build_dir, spec_path)?;
            if self.verbose {
                println!("Created source package: {}", src_package.display());
            }
        }

        if packages.is_empty() {
            return err!("No packages built");
        }
        
        Ok(packages)
    }

    fn determine_native_target_architectures(&self, spec: &PaxPackageSpec) -> Result<Vec<TargetArch>, String> {
        let mut target_archs = Vec::new();

        // If specific target architecture is requested via command line
        if let Some(target_arch) = &self.target_arch {
            target_archs.push(target_arch.clone());
            return Ok(target_archs);
        }

        // Filter spec architectures to only include native ones
        for arch in &spec.build.target_architectures {
            let arch_str = match arch {
                TargetArch::X86_64 | TargetArch::X86_64v1 | TargetArch::X86_64v3 => "x86_64",
            TargetArch::Aarch64 => "aarch64",
                TargetArch::Armv7l => "armv7l",
            TargetArch::Armv8l => "aarch64",
            TargetArch::Riscv64 => "riscv64",
            TargetArch::Powerpc64le => "powerpc64le",
            TargetArch::S390x => "s390x",
        };
        
            if arch_str == self.host_arch {
                target_archs.push(arch.clone());
            } else {
                println!("Skipping non-native architecture: {} (host: {})", arch_str, self.host_arch);
            }
        }

        // If no native architectures found in spec, default to host architecture
        if target_archs.is_empty() {
            let native_arch = match self.host_arch.as_str() {
                "x86_64" => TargetArch::X86_64v1, // Default to v1 for compatibility
                "aarch64" => TargetArch::Aarch64,
                "armv7l" => TargetArch::Armv7l,
                "riscv64" => TargetArch::Riscv64,
                "powerpc64le" => TargetArch::Powerpc64le,
                "s390x" => TargetArch::S390x,
                _ => return Err(format!("Unsupported host architecture: {}", self.host_arch)),
            };
            target_archs.push(native_arch);
        }

        Ok(target_archs)
    }

    fn setup_native_build_environment(&self, spec: &mut PaxPackageSpec, target_arch: &TargetArch, buildroot_path: &Path) -> Result<(), String> {
        if self.verbose {
            println!("Setting up native build environment for {}", target_arch.to_triple());
        }

        // Set up environment variables for native build
        spec.build.environment.insert("TARGET".to_string(), target_arch.to_triple().to_string());
        spec.build.environment.insert("TARGET_TRIPLE".to_string(), target_arch.to_triple().to_string());
        
        // Set compiler environment variables (use system compilers)
            if !spec.build.environment.contains_key("CC") {
            spec.build.environment.insert("CC".to_string(), "gcc".to_string());
            }
            if !spec.build.environment.contains_key("CXX") {
            spec.build.environment.insert("CXX".to_string(), "g++".to_string());
            }
        spec.build.environment.insert("AR".to_string(), "ar".to_string());
        spec.build.environment.insert("STRIP".to_string(), "strip".to_string());
        spec.build.environment.insert("RANLIB".to_string(), "ranlib".to_string());
        spec.build.environment.insert("NM".to_string(), "nm".to_string());
        spec.build.environment.insert("OBJCOPY".to_string(), "objcopy".to_string());
        spec.build.environment.insert("OBJDUMP".to_string(), "objdump".to_string());

        // Set PKG_CONFIG environment for native build
        spec.build.environment.insert("PKG_CONFIG".to_string(), "pkg-config".to_string());
        spec.build.environment.insert("PKG_CONFIG_PATH".to_string(), "/usr/lib/pkgconfig:/usr/share/pkgconfig".to_string());

        // Set Rust target if using Cargo
        if matches!(spec.build.build_system, BuildSystem::Cargo) {
            spec.build.environment.insert("CARGO_BUILD_TARGET".to_string(), target_arch.to_triple().to_string());
        }
        
        // Add architecture-specific flags
        let mut arch_cflags = Vec::new();
        match target_arch {
            TargetArch::X86_64 => {
                arch_cflags.push("-march=x86-64".to_string());
            },
            TargetArch::X86_64v1 => {
                arch_cflags.push("-march=x86-64".to_string());
                arch_cflags.push("-mtune=generic".to_string());
            },
            TargetArch::X86_64v3 => {
                arch_cflags.push("-march=x86-64-v3".to_string());
                arch_cflags.push("-mtune=generic".to_string());
            },
            TargetArch::Armv7l => {
                arch_cflags.push("-march=armv7-a".to_string());
                arch_cflags.push("-mfpu=neon".to_string());
            },
            TargetArch::Aarch64 => {
                arch_cflags.push("-march=aarch64".to_string());
            },
            TargetArch::Riscv64 => {
                arch_cflags.push("-march=rv64gc".to_string());
            },
            _ => {}
        }

        if self.verbose {
            println!("Architecture-specific CFLAGS: {:?}", arch_cflags);
        }
        
        // Add architecture-specific flags to CFLAGS and CXXFLAGS
        if !arch_cflags.is_empty() {
            let arch_flags_str = arch_cflags.join(" ");
            
            // Append to existing CFLAGS if present, otherwise set it
            if let Some(existing_cflags) = spec.build.environment.get("CFLAGS") {
                spec.build.environment.insert("CFLAGS".to_string(), format!("{} {}", existing_cflags, arch_flags_str));
            } else {
                spec.build.environment.insert("CFLAGS".to_string(), arch_flags_str.clone());
            }
            
            // Append to existing CXXFLAGS if present, otherwise set it
            if let Some(existing_cxxflags) = spec.build.environment.get("CXXFLAGS") {
                spec.build.environment.insert("CXXFLAGS".to_string(), format!("{} {}", existing_cxxflags, arch_flags_str));
            } else {
                spec.build.environment.insert("CXXFLAGS".to_string(), arch_flags_str);
            }
        }

        // Set buildroot environment
        spec.build.environment.insert("BUILDROOT".to_string(), buildroot_path.to_string_lossy().to_string());
        spec.build.environment.insert("DESTDIR".to_string(), buildroot_path.to_string_lossy().to_string());
        
        // Modify build commands to use correct DESTDIR
        for command in &mut spec.build.build_commands {
            if command.contains("DESTDIR=.") {
                *command = command.replace("DESTDIR=.", "DESTDIR=/buildroot");
            }
        }

        Ok(())
    }

    fn create_isolated_build_dir(&self, spec: &PaxPackageSpec, target_arch: &TargetArch) -> Result<PathBuf, String> {
        // Use a unique timestamp-based directory to avoid conflicts
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let build_dir = self.build_directory.join(&format!("{}-{}-{}", spec.name, target_arch.to_triple(), timestamp));
        fs::create_dir_all(&build_dir)
            .map_err(|_| "Failed to create isolated build directory")?;
        Ok(build_dir)
    }

    fn clone_source_code(&self, spec: &PaxPackageSpec, build_dir: &Path) -> Result<(), String> {
        if self.verbose {
            println!("Cloning source from: {}", spec.repository.as_ref().unwrap_or(&spec.homepage.clone().unwrap_or_default()));
        }

        let repo_url = spec.repository.as_ref()
            .or(spec.homepage.as_ref())
            .ok_or_else(|| "No repository URL specified".to_string())?;

        // Clean the build directory first
        if build_dir.exists() {
            if self.verbose {
                println!("Cleaning build directory: {}", build_dir.display());
            }
            // Try to remove the directory, but don't fail if it's not empty
            let _ = fs::remove_dir_all(build_dir);
        }
        fs::create_dir_all(build_dir)
            .map_err(|_| "Failed to create build directory")?;

        // Ensure the directory is empty for git clone
        if build_dir.exists() {
            let entries = fs::read_dir(build_dir)
                .map_err(|_| "Failed to read build directory")?;
            for entry in entries {
                if let Ok(entry) = entry {
                    let _ = fs::remove_file(entry.path());
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }

        // First try with version tag
        let output = Command::new("git")
            .arg("clone")
            .arg("--depth")
            .arg("1")
            .arg("--branch")
            .arg(&format!("v{}", spec.version))
            .arg(repo_url)
            .arg(".")
            .current_dir(build_dir)
            .output()
            .map_err(|_| "Failed to execute git clone")?;

        if !output.status.success() {
            if self.verbose {
                println!("Failed to clone with version tag v{}, trying without tag", spec.version);
                println!("Git error: {}", String::from_utf8_lossy(&output.stderr));
            }
            
            // Try without version tag
            let output = Command::new("git")
                .arg("clone")
                .arg("--depth")
                .arg("1")
                .arg(repo_url)
                .arg(".")
                .current_dir(build_dir)
                .output()
                .map_err(|_| "Failed to execute git clone")?;

            if !output.status.success() {
                if self.verbose {
                    println!("Git clone failed: {}", String::from_utf8_lossy(&output.stderr));
                }
                return err!("Git clone failed: {}", String::from_utf8_lossy(&output.stderr));
            }
        }

        Ok(())
    }

    fn run_isolated_build(&self, spec: &PaxPackageSpec, build_dir: &Path, buildroot_path: &Path, target_arch: &TargetArch) -> Result<String, String> {
        let mut build_log = String::new();
        
        if self.verbose {
            println!("Running isolated build for {}", spec.name);
        }

        // Set up build environment
        let mut spec_for_build = spec.clone();
        self.setup_native_build_environment(&mut spec_for_build, target_arch, buildroot_path)?;

        // Run build commands
        for command in &spec_for_build.build.build_commands {
            if self.verbose {
                println!("Running: {}", command);
            }

            let output = if self.use_bubblewrap {
                self.run_bubblewrap_build(command, build_dir, buildroot_path, &spec_for_build.build.environment)?
            } else {
                self.run_direct_build(command, build_dir, &spec_for_build.build.environment)?
            };

            build_log.push_str(&format!("Command: {}\n", command));
            build_log.push_str(&String::from_utf8_lossy(&output.stdout));
            build_log.push_str(&String::from_utf8_lossy(&output.stderr));

            if !output.status.success() {
                if self.verbose {
                    println!("Build command failed: {}", command);
                    println!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
                    println!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
                }
                return err!("Build command failed: {}\nOutput: {}", command, build_log);
            }
        }

        Ok(build_log)
    }

    fn run_bubblewrap_build(&self, command: &str, build_dir: &Path, buildroot_path: &Path, env_vars: &HashMap<String, String>) -> Result<std::process::Output, String> {
        let mut bwrap_cmd = Command::new("bwrap");
        
        // Basic bubblewrap arguments for isolation
        bwrap_cmd.args(&[
            "--unshare-all",
            "--new-session", 
            "--die-with-parent",
            "--ro-bind", "/usr", "/usr",
            "--ro-bind", "/lib", "/lib",
            "--ro-bind", "/lib64", "/lib64",
            "--ro-bind", "/bin", "/bin",
            "--ro-bind", "/sbin", "/sbin",
            "--ro-bind", "/etc", "/etc",
            "--dev-bind", "/dev", "/dev",
            "--tmpfs", "/tmp",
            "--tmpfs", "/var/tmp",
        ]);

        // Bind build directory
        bwrap_cmd.arg("--bind").arg(build_dir).arg("/build");
        
        // Bind buildroot for installation
        bwrap_cmd.arg("--bind").arg(buildroot_path).arg("/buildroot");
        
        // Set working directory
        bwrap_cmd.arg("--chdir").arg("/build");
        
        // Add environment variables
        for (key, value) in env_vars {
            bwrap_cmd.arg("--setenv").arg(key).arg(value);
        }
        
        // Execute command
        bwrap_cmd.arg("--");
        bwrap_cmd.arg("sh").arg("-c").arg(command);
        
        bwrap_cmd.output()
            .map_err(|_| format!("Failed to execute bubblewrap build command: {}", command))
    }

    fn run_direct_build(&self, command: &str, build_dir: &Path, env_vars: &HashMap<String, String>) -> Result<std::process::Output, String> {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.current_dir(build_dir);
        
        // Set environment variables
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
        
        cmd.output()
            .map_err(|_| format!("Failed to execute direct build command: {}", command))
    }


    fn create_binary_package(&self, spec: &PaxPackageSpec, _build_dir: &Path, target_arch: &TargetArch) -> Result<PathBuf, String> {
        let arch_name = match target_arch {
            TargetArch::X86_64 => "x86_64",
            TargetArch::X86_64v1 => "x86_64v1",
            TargetArch::X86_64v3 => "x86_64v3",
            TargetArch::Aarch64 => "aarch64",
            TargetArch::Armv7l => "armv7l",
            TargetArch::Armv8l => "armv8l",
            TargetArch::Riscv64 => "riscv64",
            TargetArch::Powerpc64le => "powerpc64le",
            TargetArch::S390x => "s390x",
        };
        let package_name = format!("{}-{}-{}.pax", spec.name, spec.version, arch_name);
        let package_path = self.output_directory.join(package_name);
        
        // Create temporary directory for package contents
        let temp_pkg_dir = self.temp_directory.join(format!("pkg-{}", arch_name));
        fs::create_dir_all(&temp_pkg_dir)
            .map_err(|_| "Failed to create temp package directory")?;

        // Copy installed files from buildroot to temp directory
        let buildroot_path = self.buildroot_directory.join(arch_name);
        if buildroot_path.exists() {
            // Copy usr directory if it exists
            let usr_src = buildroot_path.join("usr");
            if usr_src.exists() {
                let output = Command::new("cp")
                    .arg("-r")
                    .arg(&usr_src)
                    .arg(&temp_pkg_dir)
                    .output()
                    .map_err(|_| "Failed to copy usr directory")?;

                if !output.status.success() {
                    return err!("Failed to copy usr directory: {}", String::from_utf8_lossy(&output.stderr));
                }
            }

            // Copy other directories that might exist
            for dir in &["lib", "lib64", "bin", "sbin", "etc"] {
                let src_dir = buildroot_path.join(dir);
                let dest_dir = temp_pkg_dir.join(dir);
                if src_dir.exists() && !dest_dir.exists() {
        let output = Command::new("cp")
            .arg("-r")
                        .arg(&src_dir)
            .arg(&temp_pkg_dir)
            .output()
                        .map_err(|_| format!("Failed to copy {} directory", dir))?;

        if !output.status.success() {
                        return err!("Failed to copy {} directory: {}", dir, String::from_utf8_lossy(&output.stderr));
                    }
                }
            }
        } else {
            return err!("Buildroot not found. Build may have failed.");
        }

        // Create manifest.yaml with placeholder hash (will be updated later)
        let manifest_path = temp_pkg_dir.join("manifest.yaml");
        let placeholder_hash = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        let manifest = self.create_pax_manifest(spec, target_arch, &placeholder_hash)?;
        fs::write(&manifest_path, &manifest).map_err(|e| {
            format!("Failed to write manifest.yaml: {}", e)
        })?;
        
        // Create tar.gz archive with manifest.yaml included (first pass)
        let output = Command::new("tar")
            .arg("-czf")
            .arg(&package_path)
            .arg("-C")
            .arg(&temp_pkg_dir)
            .arg(".")
            .output()
            .map_err(|_| "Failed to create binary package")?;

        if !output.status.success() {
            return err!("Failed to create binary package: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Calculate actual hash of the created package
        let actual_hash = self.calculate_checksum(&package_path)?;
        
        // Update manifest with actual hash
        let updated_manifest = manifest.replace(&format!("hash: \"{}\"", placeholder_hash), &format!("hash: \"{}\"", actual_hash));
        
        // Write updated manifest back to temp directory
        fs::write(&manifest_path, &updated_manifest).map_err(|e| {
            format!("Failed to update manifest.yaml: {}", e)
        })?;
        
        // Recreate package archive with updated manifest
        let output = Command::new("tar")
            .arg("-czf")
            .arg(&package_path)
            .arg("-C")
            .arg(&temp_pkg_dir)
            .arg(".")
            .output()
            .map_err(|_| "Failed to recreate binary package with updated manifest")?;

        if !output.status.success() {
            return err!("Failed to recreate binary package: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Clean up temp directory
        fs::remove_dir_all(&temp_pkg_dir).ok();

        Ok(package_path)
    }

    fn create_source_package(&self, spec: &PaxPackageSpec, build_dir: &Path, spec_path: &Path) -> Result<PathBuf, String> {
        let package_name = format!("{}-{}.src.pax", spec.name, spec.version);
        let package_path = self.output_directory.join(package_name);
        
        // Create temporary directory for source package
        let temp_src_dir = self.temp_directory.join("src-package");
        fs::create_dir_all(&temp_src_dir)
            .map_err(|_| "Failed to create temp source directory")?;

        // Copy pax.yaml
        fs::copy(spec_path, temp_src_dir.join("pax.yaml"))
            .map_err(|_| "Failed to copy pax.yaml")?;

        // Copy source code
        let output = Command::new("cp")
            .arg("-r")
            .arg(".")
            .arg(&temp_src_dir)
            .current_dir(build_dir)
            .output()
            .map_err(|_| "Failed to copy source code")?;

        if !output.status.success() {
            return err!("Failed to copy source code: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Create tar.gz archive
        let output = Command::new("tar")
            .arg("-czf")
            .arg(&package_path)
            .arg("-C")
            .arg(&temp_src_dir)
            .arg(".")
            .output()
            .map_err(|_| "Failed to create source package")?;

        if !output.status.success() {
            return err!("Failed to create source package: {}", String::from_utf8_lossy(&output.stderr));
        }

        // Clean up temp directory
        fs::remove_dir_all(&temp_src_dir).ok();

        Ok(package_path)
    }

    fn load_spec(&self, spec_path: &Path) -> Result<PaxPackageSpec, String> {
        let mut file = File::open(spec_path)
            .map_err(|_| format!("Failed to open spec file: {}", spec_path.display()))?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|_| format!("Failed to read spec file: {}", spec_path.display()))?;

        serde_norway::from_str(&contents)
            .map_err(|_| format!("Failed to parse spec file: {}", spec_path.display()))
    }


    fn create_pax_manifest(&self, spec: &PaxPackageSpec, _target_arch: &TargetArch, hash: &str) -> Result<String, String> {
        // Create manifest in the format expected by PAX package manager

        // Convert build dependencies to simple string list
        let build_deps: Vec<String> = spec.dependencies.build_dependencies
            .iter()
            .map(|dep| dep.name.clone())
            .collect();

        // Convert runtime dependencies to simple string list
        let runtime_deps: Vec<String> = spec.dependencies.runtime_dependencies
            .iter()
            .map(|dep| dep.name.clone())
            .collect();

        // Create build commands string
        let build_cmds = spec.build.build_commands.join(" && ");

        // Create install commands string
        let install_cmds = match &spec.install.install_method {
            InstallMethod::RunCommands => spec.install.install_commands.join(" && "),
            InstallMethod::CopyFiles => "copy_files".to_string(),
            InstallMethod::ExtractArchive => "extract_archive".to_string(),
            InstallMethod::Custom => "custom_install".to_string(),
        };

        // Create uninstall commands (reverse of install)
        let uninstall_cmds = match &spec.install.install_method {
            InstallMethod::RunCommands => {
                // For now, just remove the installed directories
                spec.install.install_directories.iter()
                    .map(|dir| format!("rm -rf {}", dir))
                    .collect::<Vec<String>>()
                    .join(" && ")
            },
            InstallMethod::CopyFiles => "remove_files".to_string(),
            InstallMethod::ExtractArchive => "remove_extracted".to_string(),
            InstallMethod::Custom => "custom_uninstall".to_string(),
        };

        // Create purge commands (same as uninstall for now)
        let purge_cmds = uninstall_cmds.clone();

        // Create origin string
        let origin = if let Some(repo) = &spec.repository {
            if repo.contains("github.com") {
                // Extract user/repo from GitHub URL
                if let Some(path) = repo.split("github.com/").nth(1) {
                    format!("gh/{}", path.trim_end_matches(".git"))
                } else {
                    format!("pax/{}", spec.name)
                }
            } else {
                format!("pax/{}", spec.name)
            }
        } else {
            format!("pax/{}", spec.name)
        };

        // Create the manifest
        let manifest = format!(r#"name: "{}"
description: "{}"
version: "{}"
origin: "{}"
build_dependencies:
{}
runtime_dependencies:
{}
build: "{}"
install: "{}"
uninstall: "{}"
purge: "{}"
hash: "{}"
"#,
            spec.name,
            spec.description,
            spec.version,
            origin,
            build_deps.iter().map(|dep| format!("  - \"{}\"", dep)).collect::<Vec<String>>().join("\n"),
            runtime_deps.iter().map(|dep| format!("  - \"{}\"", dep)).collect::<Vec<String>>().join("\n"),
            build_cmds,
            install_cmds,
            uninstall_cmds,
            purge_cmds,
            hash
        );

        Ok(manifest)
    }

    fn calculate_checksum(&self, path: &Path) -> Result<String, String> {
        use sha2::{Sha256, Digest};
        use std::io::Read;

        let mut file = File::open(path)
            .map_err(|_| format!("Failed to open file: {}", path.display()))?;

        let mut hasher = Sha256::new();
        let mut buffer = [0; 8192];

        loop {
            let bytes_read = file.read(&mut buffer)
                .map_err(|_| format!("Failed to read file: {}", path.display()))?;
            
            if bytes_read == 0 {
                break;
            }

            hasher.update(&buffer[..bytes_read]);
        }

        Ok(format!("{:x}", hasher.finalize()))
    }

    fn calculate_initial_hash(&self, dir: &Path) -> Result<String, String> {
        use sha2::{Sha256, Digest};
        use std::io::Read;
        use std::fs;

        let mut hasher = Sha256::new();
        
        // Walk through directory and hash all files (excluding manifest.yaml)
        for entry in fs::read_dir(dir)
            .map_err(|e| format!("Failed to read directory: {}", e))? {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let path = entry.path();
            
            if path.is_file() && path.file_name() != Some(std::ffi::OsStr::new("manifest.yaml")) {
                let mut file = File::open(&path)
                    .map_err(|e| format!("Failed to open file: {}", e))?;
                
                let mut buffer = [0; 8192];
                loop {
                    let bytes_read = file.read(&mut buffer)
                        .map_err(|e| format!("Failed to read file: {}", e))?;
                    
                    if bytes_read == 0 {
                        break;
                    }
                    
                    hasher.update(&buffer[..bytes_read]);
                }
            }
        }
        
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn validate_spec(&self, spec_path: &Path) -> Result<Vec<String>, String> {
        let spec = self.load_spec(spec_path)?;
        let mut errors = Vec::new();

        // Validate required fields
        if spec.name.is_empty() {
            errors.push("Package name is required".to_string());
        }

        if spec.version.is_empty() {
            errors.push("Package version is required".to_string());
        }

        if spec.description.is_empty() {
            errors.push("Package description is required".to_string());
        }

        if spec.author.is_empty() {
            errors.push("Package author is required".to_string());
        }

        // Validate version format
        if Version::parse(&spec.version).is_err() {
            errors.push(format!("Invalid version format: {}", spec.version));
        }

        // Validate build configuration
        if spec.build.build_commands.is_empty() {
            errors.push("At least one build command is required".to_string());
        }

        // Validate install configuration
        match spec.install.install_method {
            InstallMethod::CopyFiles => {
                if spec.install.install_files.is_empty() {
                    errors.push("Install files are required for CopyFiles method".to_string());
                }
            }
            InstallMethod::RunCommands => {
                if spec.install.install_commands.is_empty() {
                    errors.push("Install commands are required for RunCommands method".to_string());
                }
            }
            _ => {}
        }

        Ok(errors)
    }

    pub fn clean_build_directory(&self) -> Result<(), String> {
        if self.build_directory.exists() {
            fs::remove_dir_all(&self.build_directory)
                .map_err(|_| "Failed to clean build directory")?;
        }
        Ok(())
    }

    pub fn get_build_stats(&self) -> BuildStats {
        BuildStats {
            build_directory: self.build_directory.clone(),
            output_directory: self.output_directory.clone(),
            temp_directory: self.temp_directory.clone(),
        }
    }
}

#[derive(Debug)]
pub struct BuildStats {
    pub build_directory: PathBuf,
    pub output_directory: PathBuf,
    pub temp_directory: PathBuf,
}

impl Default for PaxPackageBuilder {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| {
            // Fallback to a basic configuration
            Self {
                build_directory: PathBuf::from("/tmp/pax-build"),
                output_directory: PathBuf::from("/tmp/pax-output"),
                temp_directory: PathBuf::from("/tmp/pax-temp"),
                verbose: false,
                target_arch: None,
                use_bubblewrap: true,
                buildroot_directory: PathBuf::from("/tmp/pax-buildroot"),
                host_arch: "x86_64".to_string(),
            }
        })
    }
}
