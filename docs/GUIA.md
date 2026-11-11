# Guia: como rodar o Arandu

Duas formas: **baixar o binário pronto** (mais rápido) ou **compilar do código-fonte**.

---

## Opção 1 — Binário pronto (recomendado)

Baixe o arquivo da sua plataforma em
[**Releases → v0.1.0**](https://github.com/joao-pedro-gms/arandu/releases/latest).

### Linux

```bash
tar xzf arandu-linux-x86_64.tar.gz
chmod +x arandu
./arandu
```

> Para instalar no sistema: `sudo install -m 755 arandu /usr/local/bin/arandu`
> e rode de qualquer lugar com `arandu`.

### Windows

1. Extraia o `.zip` (clique direito → "Extrair tudo").
2. Execute `arandu.exe`.

> Na primeira execução o Windows SmartScreen pode avisar ("O Windows protegeu
> o computador") — clique em **"Mais informações" → "Executar assim mesmo"**.

### macOS

```bash
tar xzf arandu-macos-universal.tar.gz
chmod +x arandu
./arandu
```

> Na primeira execução, o Gatekeeper pode bloquear por não o app não ser
> assinado. Libere em **Ajustes do Sistema → Privacidade e Segurança →
> "Abrir mesmo assim"**. O port macOS passa no CI (build + testes), mas ainda
> não foi testado em hardware Apple — se algo falhar, abra uma issue.

---

## Opção 2 — Compilar do código-fonte

### Pré-requisitos

| Item | Versão | Como instalar |
|---|---|---|
| Rust (rustup) | 1.95+ (MSRV) | <https://rustup.rs> |
| C/C++ compiler | qualquer | `gcc`/`g++` (Linux), Xcode CLT (macOS), MSVC (Windows) |
| pkg-config | — | `sudo apt install pkg-config` / `dnf install pkg-config` / `pacman -S pkgconf` |
| Git | qualquer | para o painel Git e para clonar |

### Passos

```bash
git clone https://github.com/joao-pedro-gms/arandu.git
cd arandu

# Compilar e rodar (perfil release = rápido)
cargo build --release
./target/release/arandu
```

O primeiro build demora ~5-10 min (compila todas as dependências).

### Com `just` (opcional, recomendado para desenvolvimento)

```bash
cargo install just

just check     # fmt + clippy + check
just test      # todos os testes do workspace
just build     # build de desenvolvimento
just release   # build release (copia o binário para dist/)
just screenshot  # atualiza os screenshots de teste (precisa de Vulkan/lavapipe)
```

---

## CLI

```
arandu                          inicia a interface gráfica
arandu --export-config <arq>    exporta o diretório de configuração (tar.gz)
arandu --import-config <arq>    importa configuração, com backup da atual
arandu --help                   mostra a ajuda
```

---

## Primeiros passos na interface

| Ação | Atalho (preset padrão) |
|---|---|
| Alternar painel esquerdo/direito | `Tab` |
| Copiar / Mover para o painel oposto | `F5` / `F6` |
| Renomear / Excluir (lixeira) | `F2` / `Del` |
| Desfazer operação | `Ctrl+Z` |
| Preview do arquivo focado | `F3` |
| Tags do arquivo | `F4` |
| Painel Git (status, staging, commit) | `Ctrl+G` |
| Log gráfico do Git | `Ctrl+Shift+G` |
| Busca global (quick-open) | `Ctrl+P` |
| Cheatsheet com todos os atalhos | `F1` |
| Ciclar tema | `Ctrl+T` |
| Comprimir / Extrair | `F8` / `F10` |
| Abrir com app externo | `F11` |

No preset Vim (ative em `keymap.toml`), as setas viram `hjkl` e há mais atalhos
estilo Vim — o cheatsheet `F1` sempre mostra os atalhos **do seu keymap atual**.

---

## Onde ficam os arquivos de configuração

| Sistema | Caminho |
|---|---|
| Linux | `~/.config/arandu/` |
| Windows | `%APPDATA%\arandu\` (Roaming) |
| macOS | `~/Library/Application Support/arandu/` |

Dentro dele:

- `config.toml` — idioma, tema, apps de "abrir com"
- `keymap.toml` — preset e atalhos personalizados
- `tags.toml` — tags dos arquivos
- `themes/` — temas customizados em TOML (modele pelo tema padrão)
- `plugins/` — plugins `.lua` (veja `SCOPE.md` para a API)

A config é criada automaticamente no primeiro uso. Para migrar tudo para outra
máquina, use `--export-config` / `--import-config`.

---

## Problemas comuns

**"error while loading shared libraries" (Linux)** — instale as libs básicas:
`sudo apt install libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev`
ou o equivalente da sua distro.

**Build falha por falta de compilador C++** — necessário para o mlua/Luau
(plugins): `sudo apt install g++` / `sudo dnf install gcc-c++` /
`sudo pacman -S gcc`.

**Screenshot tests falham localmente** — precisam de Vulkan com o
rasterizador de software lavapipe: `sudo apt install mesa-vulkan-drivers`.
No CI isso já é configurado automaticamente.

**O painel Git não aparece / mostra erro** — o diretório aberto precisa estar
dentro de um repositório Git (ou de um de seus subdiretórios). Repositórios
novos vazios também funcionam após o primeiro commit.
