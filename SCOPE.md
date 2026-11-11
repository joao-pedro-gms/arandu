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

### Sessão 2 — práticas de projeto

| Área | Decisão | Alternativas consideradas |
|---|---|---|
| Testes | **Unit + integration (tempfile) + screenshot tests (egui_kittest) desde o dia 1** | screenshot só fase 2 |
| i18n | **Desde o dia 1** (sistema de tradução em todos os widgets) | hardcoded, retrofit fase 3 |
| Preview PDF | **Nativo: `pdfrum`** (puro Rust, MIT/Apache-2.0, render CPU tiny-skia) | poppler externo, adiar |
| Plugins (fase 3) | **Lua via `mlua` (feature `luau`)** — estaticamente linkado (vendored), MIT, sandboxável, mantém o binário único | WASM (wasmtime), LuaJIT, JS |
| FS remoto | **Local direto no MVP**, refatorar para trait Vfs quando entrar | trait desde o início |
| Metadados | **Tags no MVP** (armazenamento simples), **banco (sled/sqlite) fase 2**; favoritos/recentes no MVP | completo no MVP, só xattr |
| Telemetria | **Zero telemetria**; crash reports opt-in salvos localmente | opt-in anônima |
| Dev local | **just + cargo-hack** (receitas `check`, `test`, `screenshot`, `release`) | cargo puro, cargo-make |
| Releases | **SemVer 0.x + GitHub Releases** com changelog por commits convencionais | publish crates.io a cada release, sem releases |
| Dependências | **Conservadora + cargo-deny**; nova dep exige justificativa na PR | liberal, minimalista radical |
| MSRV | **N-2** (stable atual menos 2 releases), pin + job no CI. Floor atual: **1.95**, imposto pelo eframe 0.37 | só latest, anual |
| Governança | **SECURITY.md + CODE_OF_CONDUCT.md** (Contributor Covenant) | só SECURITY, nenhum |

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

1. API de plugins em Lua/Luau (`mlua`) — decisão fechada, primeira versão
   implementada na branch `feat/plugins`
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

- [x] ~~Nome do binário/crate: `arandu` está disponível no crates.io?~~ **Resolvido (sessão 2): livre, reservar na v0.1**
- [x] ~~Estratégia de testes~~ **Resolvido (sessão 2): unit + integration + egui_kittest desde o dia 1**
- [x] ~~i18n: entra em que fase?~~ **Resolvido (sessão 2): desde o dia 1**
- [x] ~~Qual crate para PDF: mupdf (via `mupdf`?) vs pdfium (via `pdfium-render`)?~~ **Resolvido (2026-11-10): `pdfrum`** — puro Rust, MIT/Apache-2.0 (mupdf é AGPL-3.0, incompatível), sem bundling de binário (pdfium quebraria o binário único), render CPU, SSIM ~0.99999 na suíte PDFium
- [x] ~~Plugins WASM vs Lua — decisão adiada para fase 3, mas hook de arquitetura já no design~~ **Resolvido (2026-11-10): Lua via `mlua` com feature `luau`** — estaticamente linkado (vendored), MIT, sandboxável, mantém o binário único
- [x] ~~Remote FS (SSH/SFTP/FTP) — entra no escopo algum dia?~~ **Resolvido (sessão 2): local direto no MVP, refatorar para trait depois**
- [x] ~~Tags/cores em arquivos, notas, favoritos~~ **Resolvido (sessão 2): tags no MVP com storage simples; banco fase 2**
- [x] ~~CI: GitHub Actions com build Linux+Windows, clippy deny, rustfmt~~ **Resolvido (sessão 2): criado; adicionar job MSRV + cargo-deny quando houver Cargo.toml**
- [x] ~~Telemetria/crash reports~~ **Resolvido (sessão 2): zero telemetria; crash reports opt-in locais**

### Sessão 2 — 2026-11-10 — Práticas de projeto
Rodadas: testes → i18n → PDF → plugins → FS remoto → metadados → telemetria →
toolchain dev → versionamento → política de deps → MSRV → governança. Decisões
na seção "Sessão 2" da tabela. Fechado também: crate `arandu` livre no
crates.io (reservar na v0.1).

## Registro de sessões

### Sessão 1 — 2026-11-10 — Fundações
Rodadas de grill: paradigma de UI → plataformas → Git no MVP → customização →
framework GUI → operações de arquivo → visualizações → async → escala → formato
de config → atalhos → distribuição → GUI final → lib Git → indexação → fatiamento
do MVP → runtime → workspace → persistência → identidade. Todas as decisões da
tabela acima fechadas nesta sessão, com pesquisa externa (explorer) para
egui vs iced.
