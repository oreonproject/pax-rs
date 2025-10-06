# PAX - Natively Universal Package Manager

PAX is a package manager for Oreon Linux that can install RPM files from Legacy Oreon/Fedora/RHEL, DEB packages from Debian/Ubuntu, and our own PAX format.

## What Makes It Different

most package managers only work with their own format but we thought "why not just support everything?" so thats what we did. you can install a .deb on your system even if its not debian-based, or grab RPM packages without needing to be on Fedora. 

the way it works is quite clever - instead of each package manager doing their own thing, PAX extracts everything into it's own storage area and manages dependencies universally. so like if a DEB package needs libssl and a RPM package also needs libssl, PAX knows they're the same thing and doesn't install it twice.

## Features (the good stuff)

- Cross-distro packages - install .rpm, .deb, or .pax files doesn't matter which
- Smart dependencies - automatically figures out what you need and installs it
- Secure - uses Ed25519 signatures instead of the usual GPG keys (just to keep things simple)
- Content-addressed storage - files are stored by their hash so no duplicates
- Automatic mirrorlist - if you got multiple mirrors it'll try the fastest one
- Fallback endpoints - can't find something in one endpoint? it checks the others automatically
- No manual init needed - just works if you have endpoints.txt setup

## Quick Start

If you're on Oreon 11 (not yet released) everything should already be setup. but if you need to do it manually:

1. make sure `/etc/pax/endpoints.txt` exists with your endpoint URLs or mirrorlists
2. run `pax install packagename` 

thats it, no extra configuration needed. it figures everything out automatically.

### Installing Stuff (examples)

```bash
# install a package
sudo pax install firefox

# install multiple things at once
sudo pax install vim htop neofetch

# search for packages
pax search browser

# get info about something
pax info firefox

# see whats installed
pax list

# remove stuff you dont need
sudo pax remove firefox

# update everything
sudo pax update

# clean up cache and garbage
sudo pax clean

# compile and install from source
sudo pax compile https://github.com/user/project
sudo pax compile ./custom-package.paxmeta
```

## What is an endpoint?
For pax, endpoints are basically package repositories, but are ran on an efficient API-based delivery system (similar to a CDN).

**NOTE: the endpoint API itself is still in early development and will be hosted on another repository.**

## Endpoint Setup

PAX looks for endpoints in `/etc/pax/endpoints.txt`. the format is super simple, one URL per line:

```
http://repo.oreonproject.org
http://mirror.example.com/oreon
http://mirrorlist.oreonproject.org/mirrors
```

you can use direct endpoints or mirrorlists. if its a mirrorlist URL (has "mirrorlist" or "/mirrors" in it), PAX will fetch the list of actual mirrors and try them all until it finds whats fastest.

comments work too just start the line with #:

```
# Main endpoint
http://endpoint1.oreonproject.org

# Mirror list (will auto-expand)
http://mirrors.oreonproject.org/oreon-11

# Backup endpoint
http://endpoint-cb1.oreonproject.org
```

## How Fallback Works

if PAX can't find a package or dependency in the first endpoint, it automatically tries the next one. same with updates - if repo A doesn't have the latest version but repo B does, it'll grab it from repo B. pretty smart right?

this means you can have like a fast local mirror as your first choice and a slower but more complete mirror as backup. or mix different repos that have different packages.

## Security & Keys

packages need to be signed or PAX won't install them. endpoint public keys go in `/etc/pax/trusted-keys/`.

```bash
# add an endpoint key
sudo pax trust add repo-key.pub

# list trusted keys
pax trust list

# remove a key
sudo pax trust remove keyname
```

we use Ed25519 signatures instead of GPG because GPG is complicated and some people don't understand it. Ed25519 is just as secure but way simpler.

## Architecture (nerdy stuff)

everything gets stored in `/opt/pax/`:

```
/opt/pax/
├── store/        # actual package files (stored by SHA256 hash)
├── links/        # symlinks to binaries and libraries
└── db/           # SQLite database tracking everything
```

when you install a package PAX:
1. downloads it and checks the signature
2. verifies the SHA256 hash
3. extracts files to `/opt/pax/store/<hash>/`
4. creates symlinks in `/opt/pax/links/bin/` etc
5. updates the database with dependency info

this way multiple packages can share the same files without duplication and everything stays clean.

## Cross-Distro Magic

here's where it gets interesting. different distros name things differently - like Debian might call something `libssl1.1` while Fedora calls it `openssl-libs`. PAX has a "provides" system that maps all these to a common name so dependencies work across formats.

same with library versions. if a .deb needs `libssl.so.1.1` and a .rpm provides `libssl.so.1.1`, PAX knows they're the same and can satisfy the dependency. no manual intervention needed.

## Compiling Custom Packages

if you want to install something not in the repos, use `pax compile`:

```bash
# compile from GitHub
sudo pax compile https://github.com/user/project

# compile with custom build recipe
sudo pax compile ./myapp.paxmeta

# compile from a .paxmeta URL
sudo pax compile https://example.com/package.paxmeta
```

the compile command:
1. downloads the source code
2. builds it locally on your machine
3. installs it to the PAX store
4. creates symlinks so you can use it

you can create a `.paxmeta` file to control how things build:

```yaml
name: myapp
version: 1.0.0
description: My custom application
source: https://github.com/user/myapp/archive/v1.0.0.tar.gz
dependencies:
  - libncurses>=6.0
build: |
  ./configure --prefix=/usr
  make -j$(nproc)
  make install DESTDIR=$PAX_BUILD_ROOT
```

this is useful for:
- software not in official repos
- custom patches or modifications
- development versions from git
- personal projects

## Building PAX From Source

if you want to compile it yourself:

```bash
# install dependencies (on Fedora/RHEL/Legacy Oreon)
sudo dnf install rust cargo sqlite-devel openssl-devel libzstd-devel

# build it
cargo build --release

# binary will be at target/release/pax
sudo cp target/release/pax /usr/local/bin/
```

## Configuration

PAX auto-generates config from endpoints.txt but if you want to customize stuff, edit `/etc/pax/settings.yaml`:

```yaml
sources:
  - http://endpoint1.oreonproject.org
  
db_path: /opt/pax/db/pax.db
store_path: /opt/pax/store
cache_path: /var/cache/pax
links_path: /opt/pax/links
parallel_downloads: 3
verify_signatures: true
```

## Troubleshooting

**"No endpoints found"**
- make sure `/etc/pax/endpoints.txt` exists with at least one URL

**"Package not found"**
- try `pax search` to see whats available
- check if your endpoints are accessible

**"Verification failed"**
- you probably need to add the endpoint's public key
- use `pax trust add <keyfile>`

**"Permission denied"**
- most commands need sudo because they modify system files
- searching and info don't need root tho

## Development

the code is organized into modules for each major feature. check out the source if you want to contribute or just see how it works.

main modules:
- `database/` - SQLite stuff
- `crypto/` - signature verification
- `adapters/` - RPM, DEB, PAX format handlers
- `repository/` - endpoint downloading and mirrorlist handling
- `install/`, `remove/`, `update/` - package lifecycle
- `resolver/` - dependency resolution

## License

see LICENSE file

## Contributing

Pull requests welcome.

---

*built for Oreon but should work on any Linux distro*
