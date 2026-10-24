# Maintainer: twodxme <tim@wraplabs.com>
# Almanac from the latest commit. The tagged release
# is packaging/aur/almanac.
pkgname=almanac-git
_pkgname=almanac
pkgver=2.1.7.r0
pkgrel=1
pkgdesc="See everything installed, where it came from, and what needs you"
arch=('x86_64' 'aarch64')
url="https://github.com/twodxme/almanac"
license=('MIT')
# What the binary links (readelf -d on a release build: NEEDED), plus what
# it runs (pacman, pkexec).
depends=('gtk4' 'libadwaita' 'glib2' 'pango' 'gdk-pixbuf2' 'cairo' 'openssl'
         'libgcc' 'glibc' 'pacman' 'polkit')
optdepends=(
  'git: local checkouts, plugin updates and carrying local changes forward'
  'snapper: a snapshot before every package Almanac installs or updates'
  'xdg-utils: handing file types back to the app that opened them before'
  'xdg-terminal-exec: opens full upgrades in your chosen terminal'
  'omarchy: plugin catalogue, themes and system updates'
  'yay: AUR package updates and installs'
  'pacman-contrib: checkupdates, for pending repository updates'
  'flatpak: Flatpak inventory and updates'
  'mise: version-managed AI agent CLIs'
  'github-cli: raises the GitHub API ceiling from 60/h to 5000/h'
  'webp-pixbuf-loader: marketplace card artwork'
)
makedepends=('cargo' 'gettext')
makedepends+=('git')
checkdepends=('git')
# makepkg's LTO adds -flto to CFLAGS, which turns ring's C objects into GCC
# bitcode the Rust linker (lld) cannot read: undefined ring_core_* symbols.
# The crate's own LTO (profile.release, lto = "thin") still applies.
options=('!lto')
provides=("$_pkgname")
conflicts=("$_pkgname")
source=("git+$url.git")
sha256sums=('SKIP')

pkgver() {
  cd "$srcdir/$_pkgname"
  printf "%s.r%s.%s" "$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)" \
    "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

prepare() {
  cd "$srcdir/$_pkgname"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$srcdir/$_pkgname"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  cargo build --release --locked
}

check() {
  cd "$srcdir/$_pkgname"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  # Some tests build GTK widgets and need a display; CI runs them under
  # Broadway (GDK_BACKEND=broadway). Without one they skip themselves.
  cargo test --locked
}

package() {
  cd "$srcdir/$_pkgname"

  # The application itself: one binary. Run as `almanac` it takes the
  # io.github.twodxme.Almanac application id the desktop file names.
  # Installed this way, Almanac sees that pacman owns the binary and defers
  # its own updates to the package manager.
  install -Dm755 target/release/almanac "$pkgdir/usr/bin/$_pkgname"
  # The scheduled update's non-pacman half, which omarchy-auto-update calls.
  install -Dm755 target/release/almanac-autorun \
    "$pkgdir/usr/lib/$_pkgname/almanac-autorun"

  # Almanac's own symbolic glyphs, in hicolor where the icon theme finds them.
  install -Dm644 icons/*.svg -t "$pkgdir/usr/share/icons/hicolor/scalable/actions/"

  # Translations, compiled into the system locale directory.
  for po in po/*.po; do
    [ -e "$po" ] || continue
    lang=$(basename "$po" .po)
    install -dm755 "$pkgdir/usr/share/locale/$lang/LC_MESSAGES"
    msgfmt -o "$pkgdir/usr/share/locale/$lang/LC_MESSAGES/almanac.mo" "$po"
  done

  install -Dm644 docs/almanac.desktop \
    "$pkgdir/usr/share/applications/$_pkgname.desktop"
  install -Dm644 docs/io.github.twodxme.Almanac.metainfo.xml \
    "$pkgdir/usr/share/metainfo/io.github.twodxme.Almanac.metainfo.xml"
  install -Dm644 docs/io.github.twodxme.Almanac.svg \
    "$pkgdir/usr/share/icons/hicolor/scalable/apps/io.github.twodxme.Almanac.svg"

  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$_pkgname/LICENSE"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$_pkgname/README.md"
}
