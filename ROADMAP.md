# Roadmap

O roadmap detalhado com justificativas vive em [SCOPE.md](SCOPE.md). Esta página
é o painel de progresso — atualize os checkboxes via PRs.

## Fase 1 — MVP: núcleo do gerenciador de arquivos

- [x] Workspace Cargo com crates `core`, `config`, `ui` (+ `git`/`index` reservados) <!-- PR #1 -->
- [x] Navegação dual pane (splitter arrastável, foco por painel) <!-- PR #3 -->
- [x] Modos de visualização: ícones, lista (virtualização com `show_rows`) <!-- PR #3 lista; PR #12 ícones -->
- [x] Operações: copiar/mover/renomear/deletar com **fila + progresso** <!-- PR #4 -->
- [x] **Undo** de operações + **lixeira** do SO <!-- PR #4: undo sem redo (decisão) -->
- [x] Preview pane: texto com syntax highlight (syntect), imagens <!-- PR #5: F3, cache LRU; PDF pendente fase 2 -->
- [x] Temas TOML + configuração + keymap redefinível (preset Vim)
- [x] i18n desde o dia 1 (todas as strings via sistema de tradução) <!-- PR #1: rust-i18n en/pt-BR -->
- [x] Tags nos arquivos (storage simples; banco na fase 2) <!-- PR #8: TagsStore TOML, editor F4, chips -->
- [x] Screenshot tests com egui_kittest no CI <!-- job `screenshot` com lavapipe -->
- [x] Ferramentas: compressão (zip/tar), checksums (SHA-256/MD5), rename em lote <!-- PRs #9 e #10; 7z fora: sem crate madura -->
- [x] justfile (check, test, screenshot, release) + cargo-hack <!-- job `features` (powerset) -->
- [x] CI verde: fmt, clippy `-D warnings`, testes (Ubuntu + Windows) + job MSRV (N-2) + cargo-deny <!-- PR #1 -->

## Fase 2 — Diferenciais

- [x] Painel Git (gix): ~~status, staging visual~~ ✅ (PR #13); ~~log gráfico, blame~~ ✅; ~~branches e stashes~~ ✅
- [x] Indexação global: walker em background (walkdir; notify p/ fase 3) + fuzzy estilo fzf (PR #14)
- [x] Preview ampliado: markdown, PDF (pdfrum), hex, CSV/JSON (PR #15)
- [x] Visualizadores por tipo de arquivo <!-- "Abrir com" (F11/Shift+F11): app padrão do SO via `open` + `[open_with]` por extensão, sem shell; previews internos seguem para texto/imagem/etc. -->

## Fase 3 — Extensibilidade

- [x] API de plugins (Lua/Luau sandboxed — mlua; decisão registrada no SCOPE.md) <!-- PR #20 -->
- [x] Sync de config entre máquinas <!-- export/import tar.gz via CLI (`--export-config`/`--import-config`) -->
- [x] macOS <!-- CI macOS (build + testes + clippy em `macos-latest`); port funcional depende de teste em hardware Apple -->

## Decisões em aberto

- [x] ~~Crate para PDF: mupdf vs pdfium (fase 2)~~ → pdfrum (SCOPE.md)
- [x] ~~Plugins: WASM vs Lua~~ → Lua/Luau (SCOPE.md)
- [x] ~~Disponibilidade do crate `arandu`~~ livre no crates.io
- [x] ~~Testes / i18n / telemetria / FS remoto / metadados~~ sessão 2
