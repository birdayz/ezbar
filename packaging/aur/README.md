# AUR packaging

`ezbar` ships to the AUR as a **source** package built with `cargo`.

- `PKGBUILD` / `.SRCINFO` — the package definition (version-controlled here).
- [`../../.github/workflows/aur-publish.yml`](../../.github/workflows/aur-publish.yml)
  pushes an update to the AUR on every `v*` tag.

## One-time setup

1. Create an account at <https://aur.archlinux.org> and add your SSH **public**
   key (My Account → SSH Public Key).

2. Bootstrap the AUR repo — the first push *creates* it:

   ```bash
   git clone ssh://aur@aur.archlinux.org/ezbar.git aur-ezbar
   cp packaging/aur/{PKGBUILD,.SRCINFO} aur-ezbar/
   cd aur-ezbar
   git add PKGBUILD .SRCINFO
   git commit -m "Initial import: ezbar 0.1.0"
   git push
   ```

3. For CI auto-updates on future tags, add three repo secrets
   (Settings → Secrets and variables → Actions):

   | secret | value |
   |--------|-------|
   | `AUR_SSH_PRIVATE_KEY` | the private key whose public half is on your AUR account |
   | `AUR_USERNAME` | your AUR username (commit author) |
   | `AUR_EMAIL` | email for the commit author |

After that, `git tag vX.Y.Z && git push --tags` builds the GitHub release **and**
publishes the new version to the AUR automatically.

## Build / test locally

```bash
cd packaging/aur
makepkg -si        # build + install
updpkgsums         # refresh sha256sums if you bump pkgver by hand
```
