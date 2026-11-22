# Stage Manager — build, bundle, sign, run.
#
# `make run` launches through LaunchServices so macOS checks the TCC grants against the
# bundle, not the terminal. Signing uses the stable self-signed identity
# "Stage Manager Local Signing" (see scripts/make-cert.sh); never ad-hoc.

SHELL := /bin/bash
export PATH := $(HOME)/.cargo/bin:$(PATH)

CARGO   := nice -n 18 cargo
# Cargo features of `make build` and what depends on it. The dev copy (`make dev`, `make run`)
# and the live scripts build with `probes` (the probe, demo and preview subcommands, the
# test-only ctl probes); `make install` and `make dmg` build without (as `make release` does).
FEATURES ?=
APP     := target/Stage Manager.app
FIXTURE := target/SM Fixture.app

.PHONY: build bundle sign dev run install fixture check icon dmg-art social-art dmg release

## build: cargo build --release (FEATURES=probes for the probe subcommands)
build:
	$(CARGO) build --release $(if $(FEATURES),--features $(FEATURES))

## bundle: assemble target/Stage Manager.app
bundle: build
	scripts/bundle.sh app

## sign: codesign the app with the local signing identity
sign: bundle
	scripts/sign.sh "$(APP)"

dev run: FEATURES = probes

## dev: copy the signed app to the fixed dev location (one screen-access approval for every worktree)
dev: sign
	scripts/dev-app.sh

## run: build, sign, copy to the dev location, launch through LaunchServices and stream the log (Ctrl-C quits)
run: sign
	scripts/run.sh

## install: copy the signed app to ~/Applications (refused while a copy runs from there; the old
##          copy goes only once the new one is in place, as scripts/dev-app.sh does)
install: sign
	@case "$(HOME)" in /?*) [ -d "$(HOME)" ] ;; *) false ;; esac || { echo "error: HOME is not a folder: '$(HOME)'" >&2; exit 1; }
	mkdir -p "$(HOME)/Applications"
	@SM_DEV_APP="$(HOME)/Applications/Stage Manager.app" scripts/dev-app.sh >/dev/null
	@echo "Installed $(HOME)/Applications/Stage Manager.app"

## fixture: build and sign target/SM Fixture.app (test windows)
fixture:
	scripts/bundle.sh fixture
	scripts/sign.sh "$(FIXTURE)"

## icon: regenerate the app icon from art/icon.py (needs Xcode 26+; outputs are committed)
icon:
	scripts/icon.sh

## dmg-art: re-render the DMG window background from art/dmg.py + art/dmg/text.json (headless Chrome)
dmg-art:
	scripts/dmg-art.sh

## social-art: re-render the info page's link-preview card from art/social.py (headless Chrome)
social-art:
	scripts/social-art.sh

## dmg: the installer DMG of the locally signed app -> target/release/Stage-Manager.dmg (not for publishing)
dmg: sign
	scripts/dmg.sh "$(APP)"

## release: Developer ID signed, notarized, stapled DMG + Homebrew cask (docs/RELEASING.md)
##          make release DEVELOPER_ID="Developer ID Application: …" NOTARY_PROFILE=…   (DRY_RUN=1 to check the flow)
export DEVELOPER_ID NOTARY_PROFILE DRY_RUN
release:
	scripts/release.sh

## check: cargo test + clippy -D warnings, with and without the probes
check:
	$(CARGO) test --features probes
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) clippy --all-targets --features probes -- -D warnings
