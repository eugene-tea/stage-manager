# Releasing Stage Manager

The public build is a DMG, signed with a Developer ID and notarized by Apple, published on
GitHub Releases, with a Homebrew cask in the `eugene-tea/homebrew-tap` repository and the info
page on GitHub Pages. Every step that publishes something is the owner's to run.

Dev builds are unaffected: `make sign`, `make run` and `make install` keep using the local
self-signed identity (`scripts/sign.sh`).

## One-time setup

1. **Developer ID certificate.** With the Apple Developer account active, open Xcode →
   Settings → Accounts, select the team, Manage Certificates…, **+** → **Developer ID
   Application**. (Or create it at developer.apple.com → Certificates with a signing request
   from Keychain Access.) Check that it is in the login keychain:

   ```sh
   security find-identity -v -p codesigning
   # 1) ABCD… "Developer ID Application: Your Name (TEAMID1234)"
   ```

2. **Notary credentials.** Create an app-specific password at account.apple.com (Sign-In and
   Security → App-Specific Passwords), then store a keychain profile for `notarytool`:

   ```sh
   xcrun notarytool store-credentials stage-manager-notary \
     --apple-id you@example.com --team-id TEAMID1234 --password <app-specific password>
   ```

   If `xcrun` says the Xcode license is not accepted, run `sudo xcodebuild -license` once (or
   point `DEVELOPER_DIR` at the Command Line Tools, which ship `notarytool` and `stapler`).

3. **Environment** (in your shell profile, or on the `make` line):

   ```sh
   export DEVELOPER_ID="Developer ID Application: Your Name (TEAMID1234)"   # or its SHA-1
   export NOTARY_PROFILE=stage-manager-notary
   ```

4. **GitHub Pages.** Repository Settings → Pages → Build and deployment → Deploy from a
   branch → `master`, folder `/docs`. The page is `docs/index.html` (static, no scripts;
   `docs/.nojekyll` serves the folder as is). It appears at
   https://eugene-tea.github.io/stage-manager/.

5. **Homebrew tap.** Create the public repository `eugene-tea/homebrew-tap` with a `Casks/`
   folder. Users then install with `brew install --cask eugene-tea/tap/stage-manager`.

## Each release

1. Set the version in `Cargo.toml` (`version = "X.Y.Z"`); the bundle, the DMG and the cask
   take it from there. Run `make check`.
2. Optional: `DRY_RUN=1 make release` builds everything except the signing and notary steps,
   which it prints. Its DMG holds an unsigned app: never publish it.
3. `make release` (or `make release DEVELOPER_ID=… NOTARY_PROFILE=…`):
   - builds and bundles a fresh app;
   - signs it with the Developer ID: hardened runtime, secure timestamp,
     `packaging/entitlements.plist` (empty: the app needs no entitlements; the file says why);
   - notarizes the app on its own (a zip of it, `xcrun notarytool submit --keychain-profile …
     --wait`) and staples the ticket to the bundle (`xcrun stapler staple`, then `validate`):
     installed copies verify an update with `spctl` before installing it, and a stapled app
     passes without reaching Apple, so an update never fails on a flaky network;
   - builds `target/release/Stage-Manager.dmg` (`scripts/dmg.sh`, dmgbuild, no Finder
     scripting) around the stapled app and signs it;
   - submits the DMG the same way and staples its ticket too; on any rejection it prints the
     notary log and stops;
   - checks Gatekeeper: `spctl -a -t open --context context:primary-signature -vv` on the DMG
     and `spctl -a -t exec -vv` on the app;
   - copies the DMG to `Stage-Manager-X.Y.Z.dmg` for your archive, and writes and prints the
     Homebrew cask (`target/release/stage-manager.rb`) with the version and the DMG's sha256.
4. Try the DMG on a clean account or Mac: open it, drag the app to Applications, launch, and
   check that no Gatekeeper warning appears.
5. Publish (the DMG's name stays `Stage-Manager.dmg` in every release, so
   `https://github.com/eugene-tea/stage-manager/releases/latest/download/Stage-Manager.dmg`,
   the info page's Download button, always serves the newest):

   ```sh
   gh release create vX.Y.Z target/release/Stage-Manager.dmg \
     --title "Stage Manager X.Y.Z" --notes-file <release notes>.md
   ```

6. Homebrew: copy `target/release/stage-manager.rb` to `Casks/stage-manager.rb` in the tap,
   commit (`stage-manager X.Y.Z`) and push. Check it with `brew tap eugene-tea/tap`, then
   `brew audit --cask --online eugene-tea/tap/stage-manager` and
   `brew install --cask eugene-tea/tap/stage-manager`. The cask says `auto_updates true`: the
   app installs its own updates (Install and Relaunch, or automatically when that is on), so
   `brew upgrade` leaves it to the app unless `--greedy`.

**What every release must keep, or installed copies cannot update themselves:** the asset
named `Stage-Manager.dmg` on the release; the same Developer ID team (the running app reads
its own team and designated requirement and the new app must match both); the bundle ID
`dev.eugene.stagemanager`; the DMG signed with `-i dev.eugene.stagemanager.dmg`; the app
notarized with its ticket stapled (`make release` does both); a higher version, and a release
version: a tag with a pre-release part (`v1.3.0-rc.1`) is never offered or installed. The first lines of the release notes show in the update alert (up to six
lines, Markdown reduced to plain text), so lead with what users will notice.

The tag, the release, the tap and the Pages deploy are public: each one only on the owner's
word.

## Art

- `make icon` regenerates the app icon from `art/icon.py` (an Icon Composer document,
  `art/AppIcon.icon`, compiled with Xcode 26's `actool` to `resources/Assets.car`, plus the
  fallback `resources/AppIcon.icns` and the info page's icons). The outputs are committed, so
  building needs no Xcode. `ICON_VARIANT=graphite|aurora` builds another variant. The
  `.icon` opens in Icon Composer (`/Applications/Xcode.app/Contents/Applications/Icon
  Composer.app`) for hand tuning; then compile it with `ICON_FROM_DOCUMENT=1 make icon` (plain `make icon`
  regenerates the document from the script and drops such edits).
- `make dmg-art` re-renders the DMG window's background (light and dark, 1x and 2x) from
  `art/dmg.py` and the words in `art/dmg/text.json`, with headless Chrome. Keep the words in
  step with the approvals the app asks for and honest about the network (the update check).
- `make social-art` re-renders the info page's link-preview card,
  `docs/assets/social-card.png` (1200 × 630), from `art/social.py` with headless Chrome. The
  info page's Open Graph and Twitter tags point at it. GitHub's own social preview for the
  repository cannot be set through the API: upload the same file by hand in the repository's
  Settings → General → Social preview.
- `make dmg` builds a DMG of the locally signed app for checking the window; it is not for
  publishing.
