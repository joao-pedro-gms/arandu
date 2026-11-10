use thiserror::Error;

/// Erros de leitura/escrita e serialização da configuração.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("não foi possível determinar o diretório de configuração do usuário")]
    NoConfigDir,

    #[error("erro de I/O ao acessar a configuração")]
    Io(#[from] std::io::Error),

    #[error("TOML inválido: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("não foi possível serializar a configuração: {0}")]
    Serialize(#[from] toml::ser::Error),
}
