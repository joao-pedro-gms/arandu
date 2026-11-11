set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set fallback

version := `grep -m1 '^version =' Cargo.toml | cut -d'"' -f2`
binary := if os() == "windows" { "arandu.exe" } else { "arandu" }

# Formata todo o workspace.
fmt:
    cargo fmt --all

# Verifica formatação, lints e compilação de todos os alvos.
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo check --workspace --all-targets

# Roda todos os testes do workspace.
test:
    cargo test --workspace

# Gera/atualiza os snapshots de tela (egui_kittest + wgpu headless).
screenshot:
    #!/usr/bin/env bash
    # Usa o lavapipe (Vulkan por software) quando o ICD existe; sem ele deixa
    # o wgpu escolher o backend disponível na máquina.
    icd=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json
    if [ -f "$icd" ]; then
        export WGPU_BACKEND=vulkan
        export VK_ICD_FILENAMES="$icd"
    fi
    UPDATE_SNAPSHOTS=force cargo test -p arandu-ui \
        --features screenshot-tests --test screenshots -- --test-threads=1

# Testa o feature powerset do workspace.
test-all:
    # Requer cargo-hack: cargo install cargo-hack --locked
    cargo hack test --workspace --feature-powerset --exclude-features screenshot-tests

# Checa o MSRV (1.95) em todos os alvos.
msrv:
    # Requer a toolchain 1.95 e o cargo-hack:
    #   rustup toolchain install 1.95 && cargo install cargo-hack --locked
    cargo +1.95 hack check --workspace --all-targets --rust-version

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
