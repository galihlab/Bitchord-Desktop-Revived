{
  lib,
  rustPlatform,
  pkg-config,
  wrapGAppsHook3,
  stdenv,
  nasm,

  alsa-lib,
  dbus,
  fontconfig,
  glib-networking,
  gsettings-desktop-schemas,
  gtk3,
  libayatana-appindicator,
  libGL,
  libx11,
  libxkbcommon,
  libxcursor,
  libxi,
  libxrandr,
  openssl,
  vulkan-loader,
  wayland,
  webkitgtk_4_1,
}:
let
  cargoToml = fromTOML (builtins.readFile ../Cargo.toml);
  version = cargoToml.package.version;

  inherit (stdenv.hostPlatform) isx86_64;
in
rustPlatform.buildRustPackage {
  pname = "bitchord";
  inherit version;
  src = ./..;
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook3
  ]
  ++ lib.optionals isx86_64 [
    nasm
  ];

  buildInputs = [
    alsa-lib
    dbus
    fontconfig
    glib-networking
    gsettings-desktop-schemas
    gtk3
    libayatana-appindicator
    libGL
    libx11
    libxkbcommon
    libxcursor
    libxi
    libxrandr
    openssl
    vulkan-loader
    wayland
    webkitgtk_4_1
  ];

  # The app loads these at run time rather than linking them. wrapGAppsHook3 builds the
  # wrapper from gappsWrapperArgs only (makeWrapperArgs isn't read for a Rust package).
  preFixup = ''
    gappsWrapperArgs+=(
      --prefix LD_LIBRARY_PATH : "${
        lib.makeLibraryPath [
          libayatana-appindicator
          libGL
          libx11
          libxkbcommon
          libxcursor
          libxi
          libxrandr
          vulkan-loader
          wayland
        ]
      }"
    )
  '';

  postInstall = ''
    install -Dm644 linux/bitchord.desktop $out/share/applications/bitchord.desktop
    install -Dm644 assets/icon/icon.png $out/share/icons/hicolor/256x256/apps/bitchord.png
    install -Dm644 LICENSE $out/share/licenses/bitchord/LICENSE
  '';

  meta = {
    description = "Native BitChord music client";
    homepage = "https://github.com/LoserDenji/Bitchord-Desktop-Revived";
    license = lib.licenses.gpl3Only;
    mainProgram = "bitchord";
    platforms = lib.platforms.linux;
  };
}
