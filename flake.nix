{
  description = "whisper — ditado por voz em Rust (whisper.cpp + Vulkan)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }: let
    systems = [ "x86_64-linux" ];
    forAllSystems = nixpkgs.lib.genAttrs systems;
  in {
    devShells = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.mkShell {
        packages = with pkgs; [
          cargo rustc
          cmake clang pkg-config glslang libclang shaderc
          libxkbcommon vulkan-headers vulkan-loader wayland
        ];
        shellHook = ''
          export LIBCLANG_PATH="${pkgs.libclang.lib}/lib"
          export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath [ pkgs.libxkbcommon pkgs.vulkan-loader pkgs.wayland ]}:$LD_LIBRARY_PATH"
        '';
      };
    });

    packages = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "whisper";
        version = "0.1.0";
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.cmake pkgs.clang pkgs.pkg-config pkgs.glslang pkgs.shaderc ];
        buildInputs = [ pkgs.libxkbcommon pkgs.vulkan-loader ];
        postInstall = ''
          wrapProgram $out/bin/whisper \
            --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath [ pkgs.libxkbcommon pkgs.vulkan-loader ]}"
        '';
      };
    });
  };
}
