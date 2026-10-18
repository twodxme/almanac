# Publishing to the AUR

Two packages, kept in this repository and mirrored to the AUR:

| AUR package | Builds | PKGBUILD |
|---|---|---|
| `almanac` | the latest tagged release | `packaging/aur/almanac/PKGBUILD` |
| `almanac-git` | the newest commit on `main` | `PKGBUILD` (repository root) |

They conflict with each other, so a user has one or the other.

## One-time setup

1. Create an account at <https://aur.archlinux.org/register>.
2. Add an SSH public key to it (*My Account → SSH Public Key*). A dedicated
   key is usual:
   ```bash
   ssh-keygen -t ed25519 -f ~/.ssh/aur -C aur
   printf 'Host aur.archlinux.org\n  IdentityFile ~/.ssh/aur\n  User aur\n' >> ~/.ssh/config
   ```

## First publish, and every release

```bash
# almanac (release)
git clone ssh://aur@aur.archlinux.org/almanac.git /tmp/aur-almanac
cp packaging/aur/almanac/{PKGBUILD,.SRCINFO} /tmp/aur-almanac/
git -C /tmp/aur-almanac add PKGBUILD .SRCINFO
git -C /tmp/aur-almanac commit -m "Update to 1.6.0"
git -C /tmp/aur-almanac push

# almanac-git
git clone ssh://aur@aur.archlinux.org/almanac-git.git /tmp/aur-almanac-git
cp PKGBUILD .SRCINFO /tmp/aur-almanac-git/
git -C /tmp/aur-almanac-git add PKGBUILD .SRCINFO
git -C /tmp/aur-almanac-git commit -m "Update"
git -C /tmp/aur-almanac-git push
```

Cloning a name that does not exist yet creates it on first push.

## A new release

Bump `pkgver`, reset `pkgrel=1`, refresh the checksum and `.SRCINFO`, and
test-build before pushing:

```bash
cd packaging/aur/almanac
updpkgsums                         # from pacman-contrib
makepkg --printsrcinfo > .SRCINFO
makepkg -f                         # downloads the release and runs the tests
```
