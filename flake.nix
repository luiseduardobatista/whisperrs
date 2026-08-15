{
  description = "whisper — ditado por voz em Rust (whisper.cpp + Vulkan)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    devenv.url = "github:cachix/devenv";
    devenv.inputs.nixpkgs.follows = "nixpkgs";
  };

  nixConfig = {
    extra-trusted-public-keys = [
      "devenv.cachix.org-1:w1cLUi8dv3hnoSPGAuibQv+f9TZLr6cv/Hm9XgU50cw="
      "luiseduardobatista.cachix.org-1:n72Rp2wotqSy5rQ0un3RnBbWiptb9zVfGAqU8f1xqL0="
    ];
    extra-substituters = [
      "https://devenv.cachix.org"
      "https://luiseduardobatista.cachix.org"
    ];
  };

  outputs = { self, nixpkgs, devenv, ... }@inputs: let
    systems = [ "x86_64-linux" ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
    pkgsFor = system: nixpkgs.legacyPackages.${system};
  in {
    # DevShell compartilhada com o CLI do devenv: config em ./devenv.nix.
    # Uso: `nix develop --no-pure-eval` ou `devenv shell`.
    devShells = forAllSystems (system: {
      default = devenv.lib.mkShell {
        inherit inputs;
        pkgs = pkgsFor system;
        modules = [ ./devenv.nix ];
      };
    });

    packages = forAllSystems (system: let
      pkgs = pkgsFor system;
    in {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "whisper";
        version = "0.2.2";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [
          pkgs.cmake
          pkgs.clang
          pkgs.pkg-config
          pkgs.glslang
          pkgs.shaderc
          pkgs.libclang
          pkgs.makeWrapper
        ];
        buildInputs = [ pkgs.libxkbcommon pkgs.vulkan-loader pkgs.vulkan-headers ];
        preConfigure = ''
          export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
        '';
        postInstall = ''
          wrapProgram $out/bin/whisper \
            --prefix PATH : "${pkgs.lib.makeBinPath [ pkgs.wtype pkgs.wl-clipboard pkgs.llama-cpp-vulkan ]}" \
            --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath [ pkgs.libxkbcommon pkgs.vulkan-loader ]}"
        '';
      };
    });
  };
}
