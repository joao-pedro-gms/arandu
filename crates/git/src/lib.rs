//! Integração Git do Arandu — **reservado para a fase 2**.
//!
//! Este crate ainda não expõe API. Na fase 2 ele receberá o painel Git
//! completo (branches, staging visual, blame e log gráfico) usando
//! [`gix`](https://github.com/GitoxideLabs/gitoxide) como implementação pura em
//! Rust, em vez de shells para o binário `git`.
//!
//! Dependências previstas: `gix` (com features enxutas) e, se necessário,
//! `gix-status`/`gix-diff` para status e diffs.
