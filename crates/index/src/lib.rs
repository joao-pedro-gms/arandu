//! Indexação e busca global do Arandu — **reservado para a fase 2**.
//!
//! Este crate ainda não expõe API. Na fase 2 ele cuidará da indexação de disco
//! e da busca fuzzy. Dependências previstas:
//!
//! - [`walkdir`](https://docs.rs/walkdir) para percorrer a árvore de diretórios;
//! - [`notify`](https://docs.rs/notify) para acompanhar mudanças em tempo real;
//! - [`nucleo`](https://docs.rs/nucleo) (motor do fzf) para matching fuzzy.
