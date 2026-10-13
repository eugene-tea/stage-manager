# Stage Manager — build, bundle, sign, run.
#
# `make run` launches through LaunchServices so macOS checks the TCC grants against the
# bundle, not the terminal. Signing uses the stable self-signed identity
# "Stage Manager Local Signing" (see scripts/make-cert.sh); never ad-hoc.

SHELL := /bin/bash
export PATH := $(HOME)/.cargo/bin:$(PATH)

CARGO   := nice -n 18 cargo
APP     := target/Stage Manager.app
FIXTURE := target/SM Fixture.app

.PHONY: build bundle sign dev run install fixture check icon dmg-art dmg release

## build: cargo build --release
build:
	$(CARGO) build --release

## bundle: assemble target/Stage Manager.app
bundle: build
	scripts/bundle.sh app

## sign: codesign the app with the local signing identity
sign: bundle
	scripts/sign.sh "$(APP)"

## dev: copy the signed app to the fixed dev location (one screen-access approval for every worktree)
dev: sign
	scripts/dev-app.sh

## run: build, sign, copy to the dev location, launch through LaunchServices and stream the log (Ctrl-C quits)
run: sign
	scripts/run.sh

## install: copy the signed app to ~/Applications
install: sign
	mkdir -p "$(HOME)/Applications"
	rm -rf "$(HOME)/Applications/Stage Manager.app"
	ditto "$(APP)" "$(HOME)/Applications/Stage Manager.app"
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

## dmg: the installer DMG of the locally signed app -> target/release/Stage-Manager.dmg (not for publishing)
dmg: sign
	scripts/dmg.sh "$(APP)"

## release: Developer ID signed, notarized, stapled DMG + Homebrew cask (docs/RELEASING.md)
##          make release DEVELOPER_ID="Developer ID Application: …" NOTARY_PROFILE=…   (DRY_RUN=1 to check the flow)
export DEVELOPER_ID NOTARY_PROFILE DRY_RUN
release:
	scripts/release.sh

## check: cargo test + clippy -D warnings
check:
	$(CARGO) test
	$(CARGO) clippy --all-targets -- -D warnings
