# Spec de polimento de UI — Arandu

Prioridades: P0 = quebra usabilidade, P1 = polimento visual claro, P2 = nice-to-have.
Restrições duras: nunca cor hex hardcoded (sempre `ui.visuals()`), não alterar atalhos/comportamentos, manter testes verdes, regenerar snapshots se a UI mudar legitimamente.

## P0 — usabilidade

1. **Janela mínima**: garantir largura mínima razoável (ex.: 720pt) via viewport constraints no `main`/run_native, e nenhum overflow de layout quando a janela é redimensionada até o mínimo (testar mentalmente: painel < 240pt de largura deve respeitar o mínimo do splitter).
2. **Estado hover nas linhas**: linha sob o mouse com fundo sutilmente distinto (derive: `visuals().faint_bg_color` ou equivalente) em lista e grade — hoje só há seleção/foco.
3. **Foco visível do painel ativo**: o painel inativo deve ter borda/label com alpha reduzido (texto_muted) e o ativo com accent — verificar se já existe e reforçar.

## P1 — polimento visual

4. **Padding consistente**: painéis com padding interno uniforme (4-8pt nos cantos do conteúdo; breadcrumb com padding próprio); modais com margem interna consistente (8-12pt) e cantos arredondados do tema.
5. **Separadores discretos**: linha 1pt com `visuals().widgets.noninteractive.bg_stroke` entre breadcrumb/lista e entre toolbar/conteúdo/status bar (apenas onde ainda não existe).
6. **Alinhamento de colunas**: cabeçalho da lista (se houver) alinhado ao mesmo grid das colunas; colunas Tamanho/Modificado com alinhamento consistente.
7. **Densidade da status bar**: hints com separador "·" ou espaçamento uniforme; truncar com ellipsis se não couber.
8. **Cheatsheet**: duas colunas com larguras iguais, padding interno consistente, título com separador abaixo.
9. **Botões do toolbar**: tamanho alvo uniforme (min 28pt altura), spacing uniforme entre grupos (4pt interno, 8pt entre grupos).
10. **Empty states**: painel de diretório vazio com hint central sutil ("diretório vazio"); preview sem arquivo focado com hint central — texto via `visuals().weak_text_color()`.

## P2 — nice-to-have

11. **Animações sutis**: nenhuma (fora de escopo egui padrão — pular).
12. **Ícones nos botões do toolbar**: glifos ao lado/instead de texto nos botões de modo de visualização e tema — apenas se trivial.

## Validação

- `just check && just test` verdes.
- Screenshots regenerados se houver mudança visual legítima (UPDATE_SNAPSHOTS=force com lavapipe) e versionados.
- Nenhuma alteração em keymap.rs (comportamento/atalhos fora de escopo).
