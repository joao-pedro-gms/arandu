set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set fallback

version := `grep -m1 '^version =' Cargo.toml | cut -d'"' -f2`
binary := if os() == "windows" { "arandu.exe" } else { "arandu" }

# Formata todo o workspace.
fmt:
    cargo fmt --all

# Verifica formatação e lints (falha em qualquer warning).
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings

# Roda todos os testes.
test:
    cargo test --workspace

# Build de debug do workspace.
build:
    cargo build --workspace

# Build de release e artefato versionado em dist/.
release:
    cargo build --workspace --release
    mkdir -p dist
    cp "target/release/{{binary}}" "dist/{{binary}}"
    cp "target/release/{{binary}}" "dist/arandu-v{{version}}-{{binary}}"

# Checa licenças, advisories e sources (requer cargo-deny).
deny:
    cargo deny check
