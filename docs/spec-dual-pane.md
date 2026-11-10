# Spec — Navegação dual pane (etapa `feat/dual-pane-navigation`)

Convenções: Total Commander/Krusader (dual pane), Finder/Nautilus (breadcrumb).
Units: egui points. Fonte default 14pt.

## 1. Estrutura da janela (topo → base)

```
┌──────────────────────────────────────────────────────┐
│ Toolbar (h=32): [≡ menu] [breadcrumb ativo]  [🔍][⚙] │
├──────────────────────────┬────── splitter ──────────┤
│ Painel esquerdo          │ ◄│► Painel direito        │
├──────────────────────────┴──────────────────────────┤
│ Status bar (h=24): itens/selecionados · path · espaço │
└──────────────────────────────────────────────────────┘
```

- **Toolbar**: breadcrumb reflete o painel ATIVO; ícone de menu e ações globais à esquerda, busca/config à direita.
- **Status bar**: esquerda = "N itens, X selecionados"; centro = path completo do painel ativo; direita = espaço livre do disco (fase posterior; pode omitir).

## 2. Modelo de cada painel

- **Header do painel (h=28)**: breadcrumb próprio do painel (root → … → atual, clicável; botão ▾ com histórico/back-forward não entra nesta etapa). Por consistência com toolbar, manter breadcrumb SÓ no header do painel (toolbar fica só com menu/ações globais) — evita duplicação. **Decisão: breadcrumb por painel.**
- **Colunas da lista (MVP: só modo lista)**:
  | Nome (flex, min 180) | Tamanho (72, dir. direita) | Modificado (140, dir. direita) |
  - Cabeçalho de colunas (h=22) clicável p/ ordenar nesta etapa? Não — ordenação entra depois; cabeçalho é visual.
  - Diretórios primeiro, com sufixo `/` (indicador não-cor), em negrito.
- Altura de linha: **22pt**; sem padding lateral extra (pad 6pt); seleção = fundo accent a ~25% + texto; item focado (cursor de teclado) = borda accent 1pt.
- **Estado vazio**: ícone grande (48) + "Diretório vazio" centralizados; sem quebra de layout.

## 3. Foco (painel ativo)

- Painel ativo: header com accent underline 2pt + borda do painel em `stroke(accent)` 1pt.
- Painel inativo: borda `ui.visuals().widgets.noninteractive.bg_stroke` (neutro).
- Tab alterna o painel ativo. Indicador nunca depende só de cor: o painel ativo também tem o breadcrumb em estado "enabled" e o inativo levemente atenuado (60% alpha nos textos secundários).

## 4. Splitter central

- Arrastável, hit area 6pt (draw 1pt line centered). Tooltip no hover: "Arraste para redimensionar".
- Default 50/50; mínimo por painel: 240pt. Em janela < 540pt de largura útil, manter 50/50 (mínimos sobem junto; não quebrar layout).
- Clique duplo no splitter: volta a 50/50.

## 5. Interações desta etapa

- **Navegar**: duplo clique em dir → entra; Enter com dir selecionado → entra; Backspace → sobe (dir pai); breadcrumb clicável.
- **Selecionar**: clique = seleciona; Ctrl+clique = toggle; setas movem o foco; Enter em arquivo = seleciona e foca (abrir app padrão fase posterior).
- **Histórico do breadcrumb**: botão ▾ lista os ancestrares navegáveis (mesma função dos segmentos clicáveis, atalho compacto).
- i18n: todas as strings novas via rust-i18n (pt-BR/en).

## 6. Densidade/tipografia

- Base: egui 14pt; nomes de arquivos 14pt; colunas secundárias (tamanho/data) 12.5pt `weak_text`.
- Alinhamento: nome à esquerda; tamanho e data à direita.
- Datas: `YYYY-MM-DD HH:MM` (locale-independente, ordenável).

## 7. Acessibilidade/temas

- Contraste: usar sempre `ui.visuals()` (nunca cores hardcoded) — o tema TOML da fase de temas sobrescreve.
- Seleção/foco/accent vêm de `Visuals::selection`/`hyperlink`/cor accent do config.
- Suportar claro/escuro via `ctx.set_visuals` conforme `Config.theme.dark`.
