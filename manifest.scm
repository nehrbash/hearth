(use-modules (guix profiles))

(specifications->manifest
 '("rust" "rust:cargo" "rust:tools" "gcc-toolchain" "pkg-config"
   "wayland" "libxkbcommon" "vulkan-loader" "guile" "guile-json" "dbus" "pipewire" "clang"))
