# Maintainer: twodxme <tim@wraplabs.com>
pkgname=almanac-git
_pkgname=almanac
pkgver=r50.c349dbe
pkgrel=1
pkgdesc="See everything installed, where it came from, and what needs you"
arch=('any')
url="https://github.com/twodxme/almanac"
license=('MIT')
depends=('python' 'gtk4' 'libadwaita' 'python-gobject' 'python-cairo' 'pacman'
         'polkit')
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
makedepends=('git' 'gettext')
provides=("$_pkgname")
conflicts=("$_pkgname")
source=("git+$url.git")
sha256sums=('SKIP')

pkgver() {
  cd "$srcdir/$_pkgname"
  printf "r%s.%s" "$(git rev-list --count HEAD)" "$(git rev-parse --short HEAD)"
}

check() {
  cd "$srcdir/$_pkgname"
  python3 test_app.py
}

package() {
  cd "$srcdir/$_pkgname"
  local appdir="$pkgdir/usr/share/$_pkgname"

  # The application itself.
  install -dm755 "$appdir"
  install -Dm644 *.py "$appdir/"
  install -Dm644 icons/*.svg -t "$appdir/icons/"

  # Translations, compiled into the system locale directory.
  for po in po/*.po; do
    [ -e "$po" ] || continue
    lang=$(basename "$po" .po)
    install -dm755 "$pkgdir/usr/share/locale/$lang/LC_MESSAGES"
    msgfmt -o "$pkgdir/usr/share/locale/$lang/LC_MESSAGES/almanac.mo" "$po"
  done

  # Installed this way, Almanac detects that pacman owns it and defers its
  # own updates to the package manager rather than offering a git pull.
  install -dm755 "$pkgdir/usr/bin"
  cat > "$pkgdir/usr/bin/$_pkgname" <<'LAUNCH'
#!/bin/bash
exec python3 /usr/share/almanac/app.py "$@"
LAUNCH
  chmod 755 "$pkgdir/usr/bin/$_pkgname"

  install -Dm644 docs/almanac.desktop \
    "$pkgdir/usr/share/applications/$_pkgname.desktop"
  install -Dm644 docs/io.github.twodxme.Almanac.metainfo.xml \
    "$pkgdir/usr/share/metainfo/io.github.twodxme.Almanac.metainfo.xml"
  install -Dm644 docs/io.github.twodxme.Almanac.svg \
    "$pkgdir/usr/share/icons/hicolor/scalable/apps/io.github.twodxme.Almanac.svg"

  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 README.md "$pkgdir/usr/share/doc/$pkgname/README.md"
}
