//! Configuração e temas do Arandu, persistidos em TOML.
//!
//! O arquivo padrão fica no diretório de configuração do usuário
//! (via [`directories`]), em `arandu/config.toml`.

mod error;

pub use error::ConfigError;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Configuração raiz da aplicação.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Locale ativo (ex.: `en`, `pt-BR`).
    pub locale: String,
    /// Tema visual da interface.
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            locale: "en".to_owned(),
            theme: Theme::default(),
        }
    }
}

/// Tema visual declarativo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// Nome do tema (usado em presets futuros).
    pub name: String,
    /// Cor de destaque em hexadecimal (`#rrggbb`).
    pub accent: String,
    /// Se o tema é escuro.
    pub dark: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: "arandu-dark".to_owned(),
            accent: "#7aa2f7".to_owned(),
            dark: true,
        }
    }
}

impl Config {
    /// Diretório de configuração do Arandu, criado se necessário.
    pub fn config_dir() -> Result<PathBuf, ConfigError> {
        let dirs = directories::ProjectDirs::from("io", "arandu", "arandu")
            .ok_or(ConfigError::NoConfigDir)?;
        Ok(dirs.config_dir().to_path_buf())
    }

    /// Caminho padrão do arquivo de configuração.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    /// Carrega a configuração do caminho padrão, retornando a padrão se o
    /// arquivo ainda não existir.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(Self::default_path()?)
    }

    /// Carrega a configuração de um caminho específico.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }

    /// Serializa a configuração como TOML.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Desserializa uma configuração a partir de TOML.
    pub fn from_toml(contents: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(contents)?)
    }

    /// Grava a configuração no caminho padrão, criando os diretórios.
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        let path = Self::default_path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Grava a configuração em um caminho específico, criando os diretórios.
    pub fn save_to(&self, path: impl AsRef<Path>) -> Result<(), ConfigError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_toml()?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Theme};

    #[test]
    fn roundtrips_config_through_toml() {
        let config = Config {
            locale: "pt-BR".to_owned(),
            theme: Theme {
                name: "custom".to_owned(),
                accent: "#f7768e".to_owned(),
                dark: false,
            },
        };

        let encoded = config.to_toml().expect("serializa config");
        let decoded = Config::from_toml(&encoded).expect("desserializa config");

        assert_eq!(decoded, config);
    }

    #[test]
    fn uses_defaults_for_missing_fields() {
        let decoded = Config::from_toml("locale = \"en\"\n").expect("desserializa config");
        assert_eq!(decoded, Config::default());
    }
}
