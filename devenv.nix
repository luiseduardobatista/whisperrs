{ pkgs, lib, config, inputs, ... }:

{
  # https://devenv.sh/packages/
  packages = with pkgs; [
    cargo rustc clippy
    cmake clang pkg-config glslang libclang shaderc
    libxkbcommon vulkan-headers vulkan-loader wayland
    llama-cpp-vulkan # llama-server para o pós-processamento Qwen (opcional)
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
