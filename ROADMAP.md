# Roadmap

O roadmap detalhado com justificativas vive em [SCOPE.md](SCOPE.md). Esta página
é o painel de progresso — atualize os checkboxes via PRs.

## Fase 1 — MVP: núcleo do gerenciador de arquivos

- [ ] Workspace Cargo com crates `core`, `config`, `ui` (+ `git`/`index` reservados)
- [ ] Navegação dual pane (splitter arrastável, foco por painel)
- [ ] Modos de visualização: ícones, lista, colunas (virtualização com `show_rows`)
- [ ] Operações: copiar/mover/renomear/deletar/criar com **fila + progresso**
- [ ] **Undo/redo** de operações + **lixeira** do SO
- [ ] Preview pane: texto com syntax highlight (syntect), imagens
- [ ] Temas TOML + configuração + keymap redefinível (preset Vim)
- [ ] Ferramentas: compressão (zip/tar/7z), checksums, rename em lote
- [ ] CI verde: fmt, clippy `-D warnings`, testes (Ubuntu + Windows)

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

- [ ] Disponibilidade do crate `arandu` no crates.io
- [ ] Estratégia de testes (screenshot tests egui?)
- [ ] i18n — em que fase entra
- [ ] Crate para preview de PDF
- [ ] Plugins: WASM vs Lua
- [ ] FS remoto (SSH/SFTP) no escopo?
- [ ] Metadados próprios (tags, cores, notas, favoritos)
- [ ] Telemetria/crash reports
