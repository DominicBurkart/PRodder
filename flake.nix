{
  description = "PRodder - automated PR draft management";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        src = craneLib.cleanCargoSource (craneLib.path ./.);

        commonArgs = {
          inherit src;
          strictDeps = true;
          buildInputs = with pkgs; [ openssl ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ];
          nativeBuildInputs = with pkgs; [ pkg-config ];
        };

        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          pname = "prodder-deps";
        });

        prodder = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "prodder";
          CARGO_PROFILE_RELEASE_LTO = "true";
          CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
          CARGO_PROFILE_RELEASE_PANIC = "abort";
          CARGO_PROFILE_RELEASE_STRIP = "true";
          CARGO_PROFILE_RELEASE_OPT_LEVEL = "s";
        });

        vectorConfig = pkgs.writeTextFile {
          name = "prodder-vector-config";
          destination = "/etc/vector/vector.toml";
          text = builtins.readFile ./vector.toml;
        };

        container = pkgs.dockerTools.buildLayeredImage {
          name = "prodder";
          tag = "latest";

          contents = with pkgs; [
            cacert
            curl
            vector
            vectorConfig
          ];

          config = {
            Cmd = [ "${prodder}/bin/launcher" ];
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "PATH=/bin:/usr/bin:${prodder}/bin:${pkgs.curl}/bin:${pkgs.vector}/bin"
            ];
          };
        };

      in
      {
        packages = {
          inherit prodder container;
          default = container;
        };

        checks = {
          prodder-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- --deny warnings";
          });

          prodder-fmt = craneLib.cargoFmt {
            inherit (commonArgs) src;
          };

          prodder-test = craneLib.cargoTest (commonArgs // {
            inherit cargoArtifacts;
            cargoTestExtraArgs = "--all-features --workspace";
          });
        };

        devShells.default = craneLib.devShell {
          packages = with pkgs; [
            rustToolchain
            rust-analyzer
            pkg-config
            openssl
            podman
            skopeo
            jq
            nixpkgs-fmt
            nil
          ];
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
