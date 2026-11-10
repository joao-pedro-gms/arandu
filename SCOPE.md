# Arandu — Gerenciador de Arquivos em Rust

> Documento de escopo resultante das sessões de brainstorming.
> Última atualização: 2026-11-10 (sessão 1 — fundações)

## Visão

Gerenciador de arquivos desktop em Rust, totalmente customizável (temas + config
declarativa em TOML), com integração Git profunda, indexação global de disco e
visualizações ricas voltadas a desenvolvedores e usuários avançados.

- **Nome:** Arandu (arquivo, em tupi)
- **Licença:** MIT + Apache-2.0 (dual)
- **Plataformas alvo:** Linux e Windows (macOS fora do MVP)
- **Distribuição:** binários GitHub Releases (AppImage/deb no Linux, msi no
  Windows) + `cargo install`

## Decisões de arquitetura (fechadas)

| Área | Decisão | Alternativas consideradas |
|---|---|---|
| Paradigma de UI | GUI nativa em Rust | TUI, híbrido, Tauri+Web (descartado) |
| Framework GUI | **egui/eframe 0.37** | iced 0.14, libcosmic |
| Lib Git | **gix** (gitoxide) | git2, shell-out para git CLI |
| Indexação | **Walker em background + fuzzy com nucleo** (motor do fzf) | busca sob demanda, full-text index |
| Runtime async | **Tokio** | threads+canais, smol |
| Config/temas | **TOML** declarativo | JSON, KDL/YAML |
| Persistência | **directories crate** + atomic writes | XDG manual |
| Atalhos | Modal + keymap 100% redefinível, preset Vim opcional | estilo Explorer, vim-first |
| Estrutura | **Workspace Cargo multi-crate** | crate único |

### Por que egui e não iced

Pesquisa comparativa (ver fontes na sessão 1): o requisito decisivo é a lista de
arquivos com 10k–100k+ entradas. egui virtualiza nativamente
(`ScrollArea::show_rows`, `TableBuilder::body.rows`); no iced o `table()` é
eager e seria necessário escrever/manter um widget de virtualização próprio
(como a System76 fez no libcosmic para o COSMIC Files). egui ainda tem theming
mais irrestrito (Style + Painter + StyleProvider), multi-viewport nativo e
exemplo oficial de tray-icon. iced só venceria se a prioridade fosse arquitetura
Elm/testabilidade acima de tudo.

## Fases

### Fase 1 — MVP (core primeiro)

1. Navegação dual pane (layout com splitter)
2. Modos de visualização clássicos: ícones, lista, colunas
3. Operações de arquivo: copiar/mover/renomear/deletar/criar com **fila de
   operações + progresso**, **undo/redo**, lixeira do SO
4. Preview pane: texto com syntax highlight (syntect sobre `LayoutJob`), imagens
5. Temas + config em TOML, keymap redefinível
6. Ferramentas extras: compressão (zip/tar/7z), checksums, rename em lote

### Fase 2 — Diferenciais

1. Painel Git completo (gix): branches, staging visual, blame, log gráfico,
   stashes, resolução básica de conflito
2. Indexação global de disco: walker em background com notify, busca fuzzy
   (nucleo), reindexação incremental
3. Preview pane ampliado: markdown renderizado, PDF, hex para binários, tabela
   para CSV/JSON
4. Visualizadores por tipo de arquivo

### Fase 3 — Extensibilidade

1. API de plugins (candidatos: WASM ou Lua) — desenhar a API desde a Fase 1,
   implementar depois
2. Sync de config entre máquinas (dotfiles)
3. macOS, se houver demanda

## Estrutura proposta do workspace

```
arandu/
├── crates/
│   ├── core/     # operações fs, fila, undo, lixeira
│   ├── git/      # integração gix (fase 2, mas crate reservado)
│   ├── index/    # walker + nucleo (fase 2, mas crate reservado)
│   ├── config/   # TOML, temas, keymap, directories
│   └── ui/       # egui: panes, previews, widgets
└── src/          # binário principal (app shell)
```

## Detalhes técnicos anotados

- **Listas grandes:** usar `ScrollArea::show_rows` / `TableBuilder` com IDs
  estáveis; thumbnails com cache (estilo `thumbnail_cacher` do COSMIC Files)
- **Async/UI:** Tokio com UI em thread dedicada; comunicação por canais +
  `ctx.request_repaint()`; operações bloqueantes (Git, I/O pesado) em
  `spawn_blocking`
- **Diálogos nativos:** `rfd` 0.18 (preferir backend XDG portal no Linux para
  não depender de GTK); tray via `tray-icon` (exemplo oficial com eframe)
- **Syntax highlight:** DIY — syntect → `text::LayoutJob` + `TextFormat`
- **Markdown:** `egui_commonmark` 0.26
- **Imagens:** `install_image_loaders` + upload de textura uma vez por imagem
- **Binary size:** LTO + strip; alvo ~10 MB
- **egui breaking changes:** atualizações a cada ~6 semanas; manter CI verde
  para não acumular débito de upgrade

## Open questions (próximas sessões de grill)

- [ ] Nome do binário/crate: `arandu` está disponível no crates.io? (verificar)
- [ ] Estratégia de testes: unitários por crate + integration tests de fs;
      screenshot tests do egui?
- [ ] i18n: entra em que fase? (egui tem suporte, mas afeta tudo)
- [ ] Preview de PDF: qual crate? (pdfium vs mupdf vs render via poppler)
- [ ] Plugins WASM vs Lua — decisão e sandboxing
- [ ] Remote FS (SSH/SFTP/FTP) — entra no escopo algum dia?
- [ ] Tags/cores em arquivos, notas, favoritos — metadados próprios?
- [ ] CI: GitHub Actions com build Linux+Windows, clippy deny, rustfmt
- [ ] Telemetria/crash reports: sim ou não?

## Registro de sessões

### Sessão 1 — 2026-11-10 — Fundações
Rodadas de grill: paradigma de UI → plataformas → Git no MVP → customização →
framework GUI → operações de arquivo → visualizações → async → escala → formato
de config → atalhos → distribuição → GUI final → lib Git → indexação → fatiamento
do MVP → runtime → workspace → persistência → identidade. Todas as decisões da
tabela acima fechadas nesta sessão, com pesquisa externa (explorer) para
egui vs iced.
