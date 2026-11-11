# Arandu

> Gerenciador de arquivos em Rust, totalmente customizável, com integração Git profunda e visualizações para desenvolvedores.

![CI](https://github.com/joao-pedro-gms/arandu/actions/workflows/ci.yml/badge.svg)
![Licença](https://img.shields.io/badge/licença-MIT%20%2B%20Apache--2.0-blue)

**Arandu** (arquivo, em tupi) é um gerenciador de arquivos desktop para Linux,
Windows e macOS, escrito em Rust com [egui](https://github.com/emilk/egui), pensado para
ser customizável de ponta a ponta e servir tanto a desenvolvedores quanto a
usuários avançados.

## Status

🚀 **v0.1.0 lançada** — todas as fases do roadmap entregues (navegação dual
pane, operações com undo, previews multi-formato incluindo PDF, painel Git
completo, quick-open fuzzy, tags, plugins Lua, temas/keymap). Veja
[CHANGELOG.md](CHANGELOG.md).

**📖 [Guia de como rodar](docs/GUIA.md)** — binários prontos para Linux,
Windows e macOS, ou compile do código-fonte.

## Funcionalidades

- 🗂️ **Dual pane** com modos ícones/lista, virtualização para diretórios enormes
- 🎨 **Temas e configuração declarativa em TOML** — keymap 100% redefinível,
  preset Vim opcional
- 🌿 **Painel Git completo** — status, staging, commit, branches, stashes,
  blame e log gráfico (via [gitoxide](https://github.com/GitoxideLabs/gitoxide))
- ⚡ **Busca global** (Ctrl+P) com indexação em background e fuzzy estilo fzf
- 👀 **Preview pane multi-formato** — texto com syntax highlight, imagens,
  markdown, PDF, hex, CSV/JSON
- 🔌 **API de plugins** Lua/Luau sandboxed

## Stack

| Camada | Tecnologia |
|---|---|
| GUI | egui/eframe 0.37 |
| Git | gix (gitoxide) |
| Async | Tokio |
| Indexação/busca | walkdir + fuzzy matcher próprio |
| Config/temas | TOML + directories |
| Plugins | mlua (Lua/Luau sandboxed) |
| PDF | pdfrum |

## Roadmap

✅ Fases 1–3 completas na v0.1.0. O histórico planejado está em
[SCOPE.md](SCOPE.md) e [ROADMAP.md](ROADMAP.md); próximos passos são discutidos
nas issues.

## Sincronizar configuração entre máquinas

O diretório de configuração inteiro (`config.toml`, `keymap.toml`, `tags.toml`,
`themes/` e `plugins/`) pode ser exportado como um pacote `tar.gz` e importado
em outra máquina. Funciona com qualquer transporte: pendrive, `rsync`, drive na
nuvem etc.

```bash
# Exporta a configuração atual para um pacote
arandu --export-config ~/arandu-config.tar.gz

# Importa o pacote, sobrescrevendo a configuração local
arandu --import-config ~/arandu-config.tar.gz
```

A importação renomeia o diretório atual para `<dir>.bak-<timestamp>` antes de
extrair, e pacotes com caminhos absolutos ou `..` são rejeitados. Veja também
`arandu --help`.

## Contribuindo

Veja [CONTRIBUTING.md](CONTRIBUTING.md). Issues e PRs são bem-vindos — o
[guia](docs/GUIA.md) mostra como compilar e rodar.

## Licença

Dual [MIT](LICENSE-MIT) + [Apache-2.0](LICENSE-APACHE). © 2026 João Pedro Guimarães.
