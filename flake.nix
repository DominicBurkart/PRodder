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

        # Stage 1 (build env): full rust toolchain + build deps.
        # crane isolates the cargo dependency build from the crate build,
        # so dependency compilation is cached independently of source changes.
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

        # Release-profile binary: LTO, stripped, abort-on-panic, opt-for-size.
        prodder = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "prodder";
          CARGO_PROFILE_RELEASE_LTO = "true";
          CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
          CARGO_PROFILE_RELEASE_PANIC = "abort";
          CARGO_PROFILE_RELEASE_STRIP = "true";
          CARGO_PROFILE_RELEASE_OPT_LEVEL = "s";
        });

        # Stage 2 (runtime env): distroless OCI image built with nix's
        # dockerTools. Contains only the closure of:
        #   * the stripped prodder binary
        #   * cacert (TLS trust store)
        #   * curl (transitively required by drafter.rs; will be dropped
        #     once PR #7 lands and reqwest replaces the curl subprocess)
        # There is no shell, no package manager, no coreutils, no busybox.
        # This is functionally equivalent to gcr.io/distroless/cc-debian.
        #
        # buildLayeredImage produces a podman-compatible OCI image.
        # streamLayeredImage produces a script that streams the image to
        # stdout, which is more memory-efficient for large images and
        # can be piped directly into `podman load`.
        containerName = "prodder";
        containerTag = "latest";

        containerContents = with pkgs; [
          cacert
          curl
        ];

        containerConfig = {
          Cmd = [ "${prodder}/bin/prodder" ];
          Env = [
            "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
            "PATH=${prodder}/bin:${pkgs.curl}/bin"
          ];
          Labels = {
            "org.opencontainers.image.source" =
              "https://github.com/DominicBurkart/PRodder";
            "org.opencontainers.image.description" =
              "PRodder - automated PR draft management (distroless)";
            "org.opencontainers.image.licenses" = "MIT OR Apache-2.0";
          };
        };

        container = pkgs.dockerTools.buildLayeredImage {
          name = containerName;
          tag = containerTag;
          contents = containerContents;
          config = containerConfig;
        };

        containerStream = pkgs.dockerTools.streamLayeredImage {
          name = containerName;
          tag = containerTag;
          contents = containerContents;
          config = containerConfig;
        };

      in
      {
        packages = {
          inherit prodder container containerStream;
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
