BIN := uhc
CARGO ?= cargo

.PHONY: build release test lint fmt fmt-check clean install deps smoke verify dev synth

# Every target that produces a binary signs it (cli-common's dev-sign.sh), so
# the code identity stays stable between builds. uhc keeps one keychain item,
# the member-site session, and an ad-hoc signature would prompt for it on
# every rebuild.
build: SIGN_TARGET = target/debug/$(BIN)
build:
	$(CARGO) build
	@$(SIGN)

release: SIGN_TARGET = target/release/$(BIN)
release:
	$(CARGO) build --release
	@$(SIGN)

test:
	$(CARGO) test

lint:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

clean:
	$(CARGO) clean

deps:
	$(CARGO) fetch

# `--locked` builds from Cargo.lock, the versions the tests and audit ran on.
install: SIGN_TARGET = $${CARGO_INSTALL_ROOT:-$$HOME/.cargo}/bin/$(BIN)
install:
	$(CARGO) install --path . --locked --force
	@$(SIGN)

dev: SIGN_TARGET = target/debug/$(BIN)
dev:
	$(CARGO) build
	@$(SIGN)

# The synthetic statements as PDFs, for trying the CLI: `make synth`, then
# `uhc --dir target/synth import check`.
synth:
	$(CARGO) run -q --example synth-eob -- target/synth

# Offline smoke test: no network, no real statement, and never the real
# keychain, vault or browser profile: UHC_KEYCHAIN_SERVICE names a service
# that holds nothing, with the vault under target/smoke-config, and uhc
# leaves the kept profile alone. Nothing is stored, so no key is read. The round
# trip over the synthetic PDFs runs where pdftotext is installed.
HERMETIC := UHC_KEYCHAIN_SERVICE=piekstra.uhc.hermetic-tests XDG_DATA_HOME=$(CURDIR)/target/smoke-data \
	XDG_CONFIG_HOME=$(CURDIR)/target/smoke-config
smoke: build
	@./target/debug/$(BIN) --version >/dev/null
	@env -u UHC_PROFILE ./target/debug/$(BIN) info >/dev/null
	@env -u UHC_PROFILE ./target/debug/$(BIN) completions bash >/dev/null
	@rm -rf target/smoke-config
	@$(HERMETIC) ./target/debug/$(BIN) -q profiles add smoke >/dev/null
	@$(HERMETIC) ./target/debug/$(BIN) -q auth status --profile smoke --json >/dev/null
	@for c in "auth login" "auth status" "auth logout" "auth reset" "profiles list" "profiles add" \
		"profiles remove" "patients list" "records export" "claims list" "claims get" accumulators \
		"documents list" "documents download" "import check"; do \
		./target/debug/$(BIN) $$c --help >/dev/null || exit 1; done
	@if command -v pdftotext >/dev/null; then \
		want=$$(ls tests/fixtures/eob/*-statement.txt | wc -l | tr -d ' '); \
		$(CARGO) run -q --example synth-eob -- target/smoke >/dev/null && \
		XDG_CONFIG_HOME=target/smoke-config ./target/debug/$(BIN) --dir target/smoke import check --json \
			| grep -q "\"statements\": $$want," || { echo "smoke: the $$want synthetic statements did not read"; exit 1; }; \
		XDG_CONFIG_HOME=target/smoke-config env -u UHC_PROFILE ./target/debug/$(BIN) --files target/smoke claims list --json \
			| grep -q '"schema": "claim-list/v1"' || { echo "smoke: claims list --files did not read"; exit 1; }; \
	else echo "smoke: pdftotext not installed; skipped the statement round trip"; fi
	@echo "smoke ok"

verify: fmt-check lint test smoke

define SIGN
if [ -x "$$HOME/Dev/cli-common/scripts/dev-sign.sh" ]; then \
	"$$HOME/Dev/cli-common/scripts/dev-sign.sh" "$(SIGN_TARGET)"; \
else echo "cli-common/scripts/dev-sign.sh not found — $(SIGN_TARGET) left ad-hoc signed"; fi
endef
