//! Erros da API de plugins.

/// Falha ao carregar ou executar um plugin.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// Erro de leitura do arquivo `.lua`.
    #[error("erro de I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Erro devolvido pelo interpretador Lua (sintaxe ou execução).
    #[error("erro Lua: {0}")]
    Lua(#[from] mlua::Error),
    /// Comando não registrado por nenhum plugin válido.
    #[error("comando de plugin desconhecido: {0}")]
    UnknownCommand(String),
    /// Não foi possível resolver o diretório de configuração do usuário.
    #[error("diretório de plugins indisponível")]
    NoPluginDir,
}
