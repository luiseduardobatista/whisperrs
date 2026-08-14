{ pkgs, lib, config, inputs, ... }:

{
  # https://devenv.sh/packages/
  packages = with pkgs; [
    cargo rustc clippy
    cmake clang pkg-config glslang libclang shaderc
    libxkbcommon vulkan-headers vulkan-loader wayland
    # Ferramentas de runtime para testar o app dentro da shell (sem instalar
    # nada no sistema): wtype/wl-clipboard (inserção) e llama-server (Qwen,
    # opcional). O pipewire fica de fora: é serviço de áudio do sistema.
    wtype wl-clipboard llama-cpp-vulkan
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
