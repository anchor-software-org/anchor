{ pkgs, ... }:

{
  languages.rust = {
    enable = true;

    # The devenv lock file pins the nixpkgs Rust toolchain used here.
    channel = "nixpkgs";
  };

  languages.javascript = {
    enable = true;
    directory = "./frontend";

    pnpm = {
      enable = true;
      install.enable = true;
    };
  };

  packages = with pkgs; [
    # Project and native build tooling.
    git
    cargo-tauri
    pkg-config
    cmake
    gnumake
    protobuf
    clang
    llvmPackages.libclang
    nasm
    yasm
    perl
    go
    curl
    file
    imagemagick
    android-tools

    # Tauri and GTK runtime/build dependencies.
    webkitgtk_4_1
    gtk3
    glib
    librsvg
    libayatana-appindicator
    xdotool
    openssl
    dbus

    # Wayland capture and hardware encoding dependencies.
    wayland
    libxkbcommon
    libdrm
    mesa
    libgbm
    libva
    intel-media-driver
    x264

    # Other native dependencies.
    sqlite
    zlib
  ];

  # Used by Rust crates that generate bindings with bindgen.
  env.LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

  # WebKitGTK and the application load some of these libraries dynamically,
  # so they must remain discoverable by every process launched in the shell.
  env.LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
    pkgs.webkitgtk_4_1
    pkgs.gtk3
    pkgs.glib
    pkgs.librsvg
    pkgs.libayatana-appindicator
    pkgs.openssl
    pkgs.dbus
    pkgs.wayland
    pkgs.libxkbcommon
    pkgs.libdrm
    pkgs.mesa
    pkgs.libgbm
    pkgs.libva
    pkgs.sqlite
    pkgs.zlib
  ];

  # NixOS normally exposes these through /run/opengl-driver. On this non-NixOS
  # host, point GLVND, GBM and VA-API at matching Nix driver sets. Mesa supplies
  # the AMD/other Gallium VA-API drivers; Intel's H.264 encoder lives in the
  # separate iHD driver package.
  env.GBM_BACKENDS_PATH = "${pkgs.mesa}/lib/gbm";
  env.LIBGL_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
  env.LIBVA_DRIVERS_PATH = "${pkgs.intel-media-driver}/lib/dri:${pkgs.mesa}/lib/dri";
  env.__EGL_VENDOR_LIBRARY_DIRS = "${pkgs.mesa}/share/glvnd/egl_vendor.d";

  scripts.dev.exec = ''
    exec cargo tauri dev "$@"
  '';

  scripts.build-image.exec = ''
    exec "$DEVENV_ROOT/../scripts/build-appimage-container.sh" "$@"
  '';

  scripts.check-all.exec = ''
    cargo check
    pnpm --dir frontend run check
  '';

  # Run a CI job exactly as GitHub runs it (../scripts/ci.sh job list).
  scripts.ci.exec = ''
    exec "$DEVENV_ROOT/../scripts/ci.sh" "$@"
  '';

  enterShell = ''
    echo "Anchor development environment"
    rustc --version
    cargo --version
    pnpm --version
  '';
}
