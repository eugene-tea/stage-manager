# Stage Manager — build, bundle, sign, run.
#
# `make run` launches through LaunchServices so macOS checks the TCC grants against the
# bundle, not the terminal. Signing uses the stable self-signed identity
# "Stage Manager Local Signing" (see scripts/make-cert.sh); never ad-hoc.

SHELL := /bin/bash
export PATH := $(HOME)/.cargo/bin:$(PATH)

CARGO   := nice -n 18 cargo
APP     := target/Stage Manager.app

.PHONY: build bundle sign run install check

## build: cargo build --release
build:
	$(CARGO) build --release

## bundle: assemble target/Stage Manager.app
bundle: build
	scripts/bundle.sh app

## sign: codesign the app with the local signing identity
sign: bundle
	scripts/sign.sh "$(APP)"

## run: build, bundle, sign, launch through LaunchServices and stream the log (Ctrl-C quits)
run: sign
	scripts/run.sh

## install: copy the signed app to ~/Applications
install: sign
	mkdir -p "$(HOME)/Applications"
	rm -rf "$(HOME)/Applications/Stage Manager.app"
	ditto "$(APP)" "$(HOME)/Applications/Stage Manager.app"
	@echo "Installed $(HOME)/Applications/Stage Manager.app"

## check: cargo test + clippy -D warnings
check:
	$(CARGO) test
	$(CARGO) clippy --all-targets -- -D warnings
