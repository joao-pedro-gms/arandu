# Roadmap

O roadmap detalhado com justificativas vive em [SCOPE.md](SCOPE.md). Esta página
é o painel de progresso — atualize os checkboxes via PRs.

## Fase 1 — MVP: núcleo do gerenciador de arquivos

- [x] Workspace Cargo com crates `core`, `config`, `ui` (+ `git`/`index` reservados) <!-- PR #1 -->
- [x] Navegação dual pane (splitter arrastável, foco por painel) <!-- PR #3 -->
- [ ] Modos de visualização: ícones, lista, colunas (virtualização com `show_rows`) <!-- PR #3: lista; ícones/colunas pendentes -->
- [x] Operações: copiar/mover/renomear/deletar com **fila + progresso** <!-- PR #4 -->
- [x] **Undo** de operações + **lixeira** do SO <!-- PR #4: undo sem redo (decisão) -->
- [x] Preview pane: texto com syntax highlight (syntect), imagens <!-- PR #5: F3, cache LRU; PDF pendente fase 2 -->
- [ ] Temas TOML + configuração + keymap redefinível (preset Vim)
- [ ] i18n desde o dia 1 (todas as strings via sistema de tradução)
- [ ] Tags nos arquivos (storage simples; banco na fase 2)
- [ ] Screenshot tests com egui_kittest no CI
- [ ] Ferramentas: compressão (zip/tar/7z), checksums, rename em lote
- [ ] justfile (check, test, screenshot, release) + cargo-hack
- [x] CI verde: fmt, clippy `-D warnings`, testes (Ubuntu + Windows) + job MSRV (N-2) + cargo-deny <!-- PR #1 -->

## Fase 2 — Diferenciais

- [ ] Painel Git completo (gix): status, staging visual, branches, blame, log gráfico, stashes
- [ ] Indexação global: walker em background (walkdir + notify) + fuzzy (nucleo)
- [ ] Preview ampliado: markdown, PDF, hex, CSV/JSON
- [ ] Visualizadores por tipo de arquivo

## Fase 3 — Extensibilidade

- [ ] API de plugins (WASM ou Lua — decisão em aberto, ver SCOPE.md)
- [ ] Sync de config entre máquinas
- [ ] macOS

## Decisões em aberto

- [ ] Crate para PDF: mupdf vs pdfium (fase 2)
- [ ] Plugins: WASM vs Lua (fase 3 — hook de arquitetura já desenhado)
- [x] ~~Disponibilidade do crate `arandu`~~ livre no crates.io
- [x] ~~Testes / i18n / telemetria / FS remoto / metadados~~ sessão 2
