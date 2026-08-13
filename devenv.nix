{ pkgs, lib, config, inputs, ... }:

{
  # Empurra builds do devenv (inclusive `nix build .#default` dentro do shell)
  # para o cachix — o CI de release usa isso; máquinas consomem via flake.
  cachix.push = "luiseduardobatista";

  # https://devenv.sh/packages/
  packages = with pkgs; [
    cargo rustc
    cmake clang pkg-config glslang libclang shaderc
    libxkbcommon vulkan-headers vulkan-loader wayland
  ];

  # bindgen (whisper-rs-sys) procura o libclang em LIBCLANG_PATH; o runtime
  # precisa de libxkbcommon/vulkan-loader/wayland (substitui o shellHook).
  env.LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
  env.LD_LIBRARY_PATH = lib.makeLibraryPath [
    pkgs.libxkbcommon
    pkgs.vulkan-loader
    pkgs.wayland
  ];
}
