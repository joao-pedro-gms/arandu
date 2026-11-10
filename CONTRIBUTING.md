# Contribuindo com o Arandu

Obrigado pelo interesse! O projeto está em fase inicial de definição, então
contribuições de discussão valem tanto quanto código.

## Antes de codar

1. Leia [SCOPE.md](SCOPE.md) — as decisões de arquitetura já foram fechadas;
   propostas de mudança são bem-vindas via issue de discussão.
2. Veja [ROADMAP.md](ROADMAP.md) para o que está em aberto.
3. Abra uma issue antes de PRs grandes — alinha escopo e evita retrabalho.

## Fluxo de trabalho

- Branchs a partir de `main`: `feat/…`, `fix/…`, `docs/…`
- Commits em português ou inglês, no imperativo, escopo curto
  (ex: `feat(core): fila de operações com progresso`)
- PRs pequenos e revisáveis; CI deve estar verde (fmt + clippy `-D warnings` + testes)

## Padrões de código

- `cargo fmt` e `cargo clippy -- -D warnings` sem avisos
- Erros: `thiserror` em libs, `anyhow` no binário
- Sem `unsafe` sem justificativa em comentário + issue
- Mensagens de commit e código em português (documentação) ou inglês (APIs públicas)

## Reportando bugs

Use a issue template de bug com: SO, versão, passos para reproduzir e, se possível,
um diretório mínimo que reproduza o problema.

## Licença

Ao contribuir, você concorda que seu código é licenciado sob MIT + Apache-2.0 (dual),
igual ao restante do projeto.
