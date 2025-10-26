# PAX Package Manager

PAX is the official package manager for Oreon Linux. It's designed to be fast, reliable, and support multiple package formats including PAX native packages, APT, RPM, and GitHub repositories.

## Installation

PAX comes pre-installed with Oreon Linux starting in Oreon 11.

## Basic Usage

### Installing Packages

```bash
# Install a package
pax install package-name

# Install from specific repository
pax install package-name --from apt

# Install multiple packages
pax install package1 package2 package3

# Install with dependencies
pax install package-name --with-deps
```

### Removing Packages

```bash
# Remove a package
pax remove package-name

# Remove with dependencies
pax remove package-name --with-deps

# Purge package (remove config files too)
pax purge package-name
```

### Updating and Upgrading

```bash
# Update package lists
pax update

# Upgrade all packages
pax upgrade

# Upgrade specific package
pax upgrade package-name
```

### Searching Packages

```bash
# Search for packages
pax search keyword

# Search in specific repository
pax search keyword --from github

# List installed packages
pax list

# Show package info
pax info package-name
```

## Repository Management

PAX supports multiple repository types:

### PAX Repositories
```bash
# Add PAX repository
pax repo add https://pax.example.com

# List repositories
pax repo list

# Test repository connectivity
pax repo test https://pax.example.com
```

### APT Repositories
```bash
# Add APT repository
pax repo add apt://archive.ubuntu.com/ubuntu

# Install from APT
pax install package-name --from apt
```

### RPM Repositories
```bash
# Add RPM repository
pax repo add rpm://mirror.centos.org/centos/8/BaseOS/x86_64/os

# Install from RPM
pax install package-name --from rpm
```

### GitHub Repositories
```bash
# Add GitHub repository
pax repo add gh/user/repo

# Install from GitHub
pax install package-name --from github
```

## Configuration

PAX repo config is stored in `/etc/pax/sources.conf`:

## Package Creation

### Using pax-builder

```bash
# Initialize new package
pax-builder init my-package

# Build package
pax-builder build pax.yaml

# Build for specific architecture
pax-builder build pax.yaml --target x86_64v3

# Clean build directory
pax-builder clean
```

### Package Specification (pax.yaml)

```yaml
name: my-package
version: "1.0.0"
description: "A sample package"
author: "Your Name"
license: "MIT"
homepage: "https://example.com"
repository: "https://github.com/user/repo"

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
  provides:
    - "my-package"
  conflicts: []
```

## Advanced Features

### Package Holds

```bash
# Hold package from upgrades
pax hold package-name

# Hold specific version
pax hold package-name --version 1.0.0

# List held packages
pax hold list

# Remove hold
pax hold remove package-name
```

### Rollback Support

```bash
# Create rollback point
pax rollback create "Before installing new package"

# List rollback points
pax rollback list

# Rollback to specific point
pax rollback restore rollback_1234567890
```

### Package Verification

```bash
# Verify package integrity
pax verify package-name

# Verify all packages
pax verify --all

# Add trusted key
pax verify add-key key-id public-key
```

### Service Management

```bash
# Enable service
pax service enable service-name

# Disable service
pax service disable service-name

# Start service
pax service start service-name

# Stop service
pax service stop service-name

# Restart service
pax service restart service-name
```

## Troubleshooting

### Common Issues

**Package not found:**
```bash
# Update package lists
pax update

# Check available repositories
pax repo list

# Search for similar packages
pax search keyword
```

**Installation fails:**
```bash
# Check dependencies
pax info package-name

# Install dependencies manually
pax install dependency1 dependency2

# Try with verbose output
pax install package-name --verbose
```

**Permission denied:**
```bash
# Run with sudo
sudo pax install package-name

# Check file permissions
ls -la /etc/pax/
```

### Log Files

PAX logs are stored in `/var/log/pax.log`. You can view recent logs:

```bash
# View recent logs
tail -f /var/log/pax.log

# View logs for specific operation
grep "install" /var/log/pax.log
```

### Debug Mode

```bash
# Enable debug logging
pax --debug install package-name

# Verbose output
pax --verbose install package-name
```

## Performance Tips

### Caching

PAX automatically caches downloaded packages and metadata. Cache is stored in `/var/cache/pax/`.

```bash
# Clear cache
pax cache clear

# Show cache statistics
pax cache stats
```

### Parallel Downloads

PAX downloads packages in parallel by default. You can configure the number of concurrent downloads:

```bash
# Set concurrent downloads
pax config set download.concurrent 4
```

## Security

### Package Signing

PAX supports package signing with GPG and Ed25519:

```bash
# Verify package signature
pax verify package-name --signature

# Add trusted key
pax verify add-key key-id public-key
```

### Repository Authentication

```bash
# Add repository credentials
pax repo auth add https://private-repo.com --username user --password pass

# Use API key
pax repo auth add https://api-repo.com --api-key your-key
```

## Contributing

### Development Setup

```bash
# Clone repository
git clone https://github.com/oreon11/pax.git
cd pax

# Build in development mode
cargo build

# Run tests
cargo test

# Run with debug output
cargo run -- --debug install package-name
```

### Adding New Package Formats

To add support for a new package format:

1. Create parser in `metadata/src/parsers/your_format/mod.rs`
2. Add format to `MetaDataKind` enum
3. Implement `RawYourFormat` struct with `process()` method
4. Add format to `OriginKind` enum in settings
5. Update processed metadata handling

### Testing

```bash
# Run all tests
cargo test

# Run specific test module
cargo test parsers

# Run integration tests
cargo test --test integration
```