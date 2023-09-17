
.PHONY: build
build:
	cargo build --release

.PHONY: build-xinput
build-xinput:
	cargo build --release --features xinput --no-default-features
