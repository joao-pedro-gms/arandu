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

    /// Cor em formato diferente de `#rrggbb` (ou vazia quando obrigatória).
    #[error("cor inválida em `{field}`: {value:?} (esperado #rrggbb)")]
    InvalidColor { field: String, value: String },

    /// Arquivo de tema sem o campo obrigatório `name` (ou com valor vazio).
    #[error("tema sem campo `name` (ou nome vazio) em {path}")]
    MissingThemeName { path: String },

    /// Preset de keymap desconhecido.
    #[error("preset de keymap desconhecido: {0:?} (use `default` ou `vim`)")]
    UnknownKeymapPreset(String),

    /// Nome de ação desconhecido no `keymap.toml`.
    #[error("ação de keymap desconhecida: {0:?}")]
    UnknownAction(String),

    /// Nome de tecla fora do conjunto suportado.
    #[error("tecla inválida em `{action}`: {key:?} (nomes egui: F1..F12, Tab, ArrowUp, J, ...)")]
    InvalidKey { action: String, key: String },
}
