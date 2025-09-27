{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        pkgName = cargoToml.package.name;
        pkgVersion = cargoToml.package.version;
      in rec {
        rustc = pkgs.rustc;

        pax = pkgs.rustPlatform.buildRustPackage {
          pname = pkgName;
          version = pkgVersion;
          src = pkgs.lib.cleanSource ./.;
          cargoLock = { lockFile = ./Cargo.lock; };

          buildPhase = ''
            export CARGO_HOME="$PWD/.cargo"
            cargo build --release --locked
          '';
          installPhase = ''
            mkdir -p $out/bin
            BIN="target/release/${pkgName}"
            cp "$BIN" $out/bin/
          '';

          nativeBuildInputs = with pkgs; [ pkg-config openssl cmake ];
          buildInputs = with pkgs; [ zlib openssl ];
        };

        packages = {
          default = pax;
        };
        defaultPackage = pax;

        devShells = {
          default = pkgs.mkShell {
            name = "rust-dev-shell";
            buildInputs = with pkgs; [
              rustc
              cargo
              rustfmt
              rust-analyzer
              sccache
            ];
            shellHook = ''
              
            '';
          };
        };
      }
    );
}
