.PHONY: all build build-backend build-shell build-extension build-webapp dev dev-backend dev-webapp clean install

all: build

# Build all components
build: build-backend build-shell build-extension build-webapp

build-backend:
	cd backend && cargo build --release

build-shell:
	cd shell-wrapper && cargo build --release

build-extension:
	cd vscode-extension && npm install && npm run compile

build-webapp:
	cd webapp && npm install && npm run build

# Development servers
dev: dev-backend dev-webapp

dev-backend:
	cd backend && cargo run

dev-webapp:
	cd webapp && npm run dev

# Install dependencies
install:
	cd vscode-extension && npm install
	cd webapp && npm install

# Install binaries to system
install-bin: build-backend build-shell
	cp target/release/hermione-backend /usr/local/bin/
	cp target/release/hermione-shell /usr/local/bin/

# Clean build artifacts
clean:
	cargo clean
	rm -rf vscode-extension/out vscode-extension/node_modules
	rm -rf webapp/dist webapp/node_modules

# Run backend with logging
run-backend:
	RUST_LOG=info cargo run --manifest-path backend/Cargo.toml

# Run shell wrapper
run-shell:
	cargo run --manifest-path shell-wrapper/Cargo.toml -- --name "Student"

# Package VS Code extension
package-extension:
	cd vscode-extension && npx vsce package
