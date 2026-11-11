# Changelog

Todas as mudanças notáveis do Arandu são documentadas aqui. O formato segue o espírito do [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/) e o versionamento é [SemVer](https://semver.org/lang/pt-BR/) em fase 0.x.

## [0.1.0] - 2026-11-11

Primeira versão funcional. Todas as fases do roadmap inicial estão completas.

### Adicionado

- **Navegação dual pane** com splitter redimensionável, breadcrumb com ancestrais, seleção múltipla e virtualização de listas
- **Modos de visualização**: lista detalhada e grade de ícones (Ctrl+1)
- **Operações de arquivo**: copiar/mover/renomear/excluir com fila sequencial, barra de progresso, cancelamento, **undo** (Ctrl+Z) e lixeira do SO
- **Previews** (F3): texto com syntax highlight (syntect), imagens, Markdown, PDF (pdfrum), CSV, JSON e visualização hexadecimal
- **Painel Git** (Ctrl+G): status, staging visual, commit, log gráfico (Ctrl+Shift+G), blame, branches e stashes — via gix
- **Quick-open** global (Ctrl+P): indexação em background com busca fuzzy estilo fzf e revelação no painel
- **Tags** nos arquivos (F4) com editor e chips na listagem
- **Ferramentas**: checksums SHA-256/MD5 (F9), rename em lote com padrões (F7), compressão/extração zip/tar (F8/F10)
- **Abrir com** (F11/Shift+F11): app configurado por extensão ou app padrão do SO
- **Temas TOML** declarativos com herança do tema base e ciclagem (Ctrl+T)
- **Keymap redefinível** em TOML com presets default e Vim
- **Hints dinâmicos** de atalhos na status bar e cheatsheet (F1)
- **API de plugins** Lua/Luau sandboxed (mlua) com comandos registráveis
- **i18n** desde o dia 1 (pt-BR/en)
- **Sync de config** entre máquinas via export/import tar.gz (`--export-config` / `--import-config`)
- CLI: `--help`, export/import de config
- CI: fmt, clippy `-D warnings`, testes e build em Ubuntu/Windows/macOS + containers Fedora/Arch, MSRV (N-2), cargo-deny, cargo-hack feature-powerset e screenshot tests com lavapipe

[0.1.0]: https://github.com/joao-pedro-gms/arandu/releases/tag/v0.1.0
