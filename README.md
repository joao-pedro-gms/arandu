# Arandu

> Gerenciador de arquivos em Rust, totalmente customizável, com integração Git profunda e visualizações para desenvolvedores.

![CI](https://github.com/joao-pedro-gms/arandu/actions/workflows/ci.yml/badge.svg)
![Licença](https://img.shields.io/badge/licença-MIT%20%2B%20Apache--2.0-blue)

**Arandu** (arquivo, em tupi) é um gerenciador de arquivos desktop para Linux e
Windows, escrito em Rust com [egui](https://github.com/emilk/egui), pensado para
ser customizável de ponta a ponta e servir tanto a desenvolvedores quanto a
usuários avançados.

## Status

🧠 **Fase de definição de escopo** — veja [SCOPE.md](SCOPE.md) para as decisões
fechadas de arquitetura e o roadmap em três fases. Código ainda não iniciado.

## Diferenciais planejados

- 🎨 **Temas e configuração declarativa em TOML** — keymap 100% redefinível,
  preset Vim opcional
- 🌿 **Painel Git completo** — branches, staging visual, blame, log gráfico
  (via [gitoxide](https://github.com/GitoxideLabs/gitoxide))
- ⚡ **Indexação global de disco** com busca fuzzy (motor [nucleo](https://github.com/voitd/nucleo),
  o mesmo do fzf)
- 👀 **Preview pane multi-formato** — texto com syntax highlight, imagens,
  markdown, hex, CSV/JSON
- 🗂️ **Dual pane** com modos ícones/lista/colunas e virtualização para
  diretórios com centenas de milhares de arquivos

## Stack

| Camada | Tecnologia |
|---|---|
| GUI | egui/eframe 0.37 |
| Git | gix (gitoxide) |
| Async | Tokio |
| Indexação/busca | walkdir + notify + nucleo |
| Config/temas | TOML + directories |

## Roadmap

1. **Fase 1 (MVP):** navegação dual pane, operações com fila/undo/lixeira,
   previews de texto/imagem, temas e keymap
2. **Fase 2:** painel Git completo, indexação global, visualizadores por tipo
3. **Fase 3:** API de plugins, sync de config, macOS

Detalhes em [SCOPE.md](SCOPE.md) e [ROADMAP.md](ROADMAP.md).

## Contribuindo

Veja [CONTRIBUTING.md](CONTRIBUTING.md). O projeto está em fase de definição —
issues de discussão sobre o escopo são muito bem-vindas.

## Licença

Dual [MIT](LICENSE-MIT) + [Apache-2.0](LICENSE-APACHE). © 2026 João Pedro Guimarães.
