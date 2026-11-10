//! Configuração, temas e keymap do Arandu, persistidos em TOML.
//!
//! O arquivo padrão fica no diretório de configuração do usuário
//! (via [`directories`]), em `arandu/config.toml`; temas extras vivem em
//! `arandu/themes/*.toml` e o keymap em `arandu/keymap.toml`.

mod error;
pub mod keymap;

pub use error::ConfigError;
pub use keymap::{Action, KeyBinding, Keymap, Modifiers};

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

/// Tema visual declarativo, com paleta completa em `#rrggbb`.
///
/// Campos opcionais herdam do preset embutido correspondente (`dark` decide se
/// a base é [`Theme::builtin_dark`] ou [`Theme::builtin_light`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    /// Nome do tema (usado para referência e para achar `themes/<name>.toml`).
    #[serde(default = "default_theme_name")]
    pub name: String,
    /// Se o tema é escuro.
    #[serde(default = "default_dark")]
    pub dark: bool,
    /// Cor de destaque em hexadecimal (`#rrggbb`).
    #[serde(default = "default_accent")]
    pub accent: String,
    /// Fundo da janela.
    #[serde(default)]
    pub background: Option<String>,
    /// Fundo de painéis/barras secundárias.
    #[serde(default)]
    pub panel: Option<String>,
    /// Texto principal.
    #[serde(default)]
    pub text: Option<String>,
    /// Texto secundário/atenuado.
    #[serde(default)]
    pub text_muted: Option<String>,
    /// Bordas e separadores.
    #[serde(default)]
    pub border: Option<String>,
}

impl Theme {
    /// Preset escuro embutido.
    pub fn builtin_dark() -> Self {
        Self {
            name: "arandu-dark".to_owned(),
            dark: true,
            accent: "#7aa2f7".to_owned(),
            background: Some("#1b1e24".to_owned()),
            panel: Some("#22262e".to_owned()),
            text: Some("#d5dae3".to_owned()),
            text_muted: Some("#8b93a1".to_owned()),
            border: Some("#3a404c".to_owned()),
        }
    }

    /// Preset claro embutido.
    pub fn builtin_light() -> Self {
        Self {
            name: "arandu-light".to_owned(),
            dark: false,
            accent: "#3568d4".to_owned(),
            background: Some("#f7f8fa".to_owned()),
            panel: Some("#eceef2".to_owned()),
            text: Some("#1f2329".to_owned()),
            text_muted: Some("#5b6470".to_owned()),
            border: Some("#c9ced6".to_owned()),
        }
    }

    /// Base embutida correspondente ao modo claro/escuro.
    pub fn builtin(dark: bool) -> Self {
        if dark {
            Self::builtin_dark()
        } else {
            Self::builtin_light()
        }
    }

    /// Presets embutidos mais os temas do diretório de configuração.
    ///
    /// Erros de arquivos individuais são registrados em `stderr` e não impedem
    /// o carregamento dos demais.
    pub fn available() -> Vec<Theme> {
        let mut themes = vec![Self::builtin_dark(), Self::builtin_light()];
        if let Ok(dir) = Config::config_dir() {
            let loaded = Self::load_dir(&dir);
            for error in loaded.errors {
                eprintln!("arandu: tema ignorado: {error}");
            }
            themes.extend(loaded.themes);
        }
        themes
    }

    /// Lê `<dir>/themes/*.toml`, pulando arquivos inválidos.
    ///
    /// Os erros são devolvidos em [`ThemeLoad::errors`] em vez de abortar o
    /// carregamento dos demais temas.
    pub fn load_dir(dir: impl AsRef<Path>) -> ThemeLoad {
        let themes_dir = dir.as_ref().join("themes");
        let mut loaded = ThemeLoad::default();
        let entries = match std::fs::read_dir(&themes_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return loaded,
            Err(error) => {
                loaded.errors.push(ConfigError::Io(error));
                return loaded;
            }
        };

        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        paths.sort();

        for path in paths {
            match Self::load_from(&path) {
                Ok(theme) => loaded.themes.push(theme),
                Err(error) => loaded.errors.push(error),
            }
        }
        loaded
    }

    /// Carrega um único arquivo de tema. O campo `name` é obrigatório.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Theme, ConfigError> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path)?;
        let raw: ThemeFile = toml::from_str(&contents)?;
        let name = raw.name.as_deref().unwrap_or_default().trim();
        if name.is_empty() {
            return Err(ConfigError::MissingThemeName {
                path: path.display().to_string(),
            });
        }
        let builtin = Self::builtin(raw.dark);
        let theme = Theme {
            name: name.to_owned(),
            dark: raw.dark,
            accent: raw.accent.unwrap_or(builtin.accent),
            background: raw.background,
            panel: raw.panel,
            text: raw.text,
            text_muted: raw.text_muted,
            border: raw.border,
        };
        theme.validate()?;
        Ok(theme)
    }

    /// Resolve a paleta mesclando os campos definidos sobre o preset embutido
    /// correspondente ao modo claro/escuro.
    pub fn resolved(&self) -> Theme {
        let base = Self::builtin(self.dark);
        self.merge_over(&base)
    }

    /// Mescla `self` sobre `base`; campos vazios/`None` herdam de `base`.
    pub fn merge_over(&self, base: &Theme) -> Theme {
        Theme {
            name: if self.name.trim().is_empty() {
                base.name.clone()
            } else {
                self.name.clone()
            },
            dark: self.dark,
            accent: if self.accent.trim().is_empty() {
                base.accent.clone()
            } else {
                self.accent.clone()
            },
            background: self.background.clone().or_else(|| base.background.clone()),
            panel: self.panel.clone().or_else(|| base.panel.clone()),
            text: self.text.clone().or_else(|| base.text.clone()),
            text_muted: self.text_muted.clone().or_else(|| base.text_muted.clone()),
            border: self.border.clone().or_else(|| base.border.clone()),
        }
    }

    /// Valida o formato `#rrggbb` de todas as cores definidas.
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_color("accent", &self.accent)?;
        for (field, value) in [
            ("background", &self.background),
            ("panel", &self.panel),
            ("text", &self.text),
            ("text_muted", &self.text_muted),
            ("border", &self.border),
        ] {
            if let Some(value) = value {
                validate_color(field, value)?;
            }
        }
        Ok(())
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::builtin_dark()
    }
}

/// Resultado de [`Theme::load_dir`]: temas válidos + erros dos inválidos.
#[derive(Debug, Default)]
pub struct ThemeLoad {
    pub themes: Vec<Theme>,
    pub errors: Vec<ConfigError>,
}

/// Representação de arquivo de tema: `name` obrigatório, `dark` padrão `true`.
#[derive(Debug, Deserialize)]
struct ThemeFile {
    name: Option<String>,
    #[serde(default = "default_dark")]
    dark: bool,
    accent: Option<String>,
    background: Option<String>,
    panel: Option<String>,
    text: Option<String>,
    text_muted: Option<String>,
    border: Option<String>,
}

fn default_theme_name() -> String {
    "arandu-dark".to_owned()
}

fn default_accent() -> String {
    "#7aa2f7".to_owned()
}

fn default_dark() -> bool {
    true
}

fn validate_color(field: &str, value: &str) -> Result<(), ConfigError> {
    let valid = {
        let hex = value.trim().trim_start_matches('#');
        hex.len() == 6 && hex.chars().all(|ch| ch.is_ascii_hexdigit())
    };
    if valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidColor {
            field: field.to_owned(),
            value: value.to_owned(),
        })
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
        Self::from_toml(&contents)
    }

    /// Serializa a configuração como TOML.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Desserializa uma configuração a partir de TOML.
    pub fn from_toml(contents: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(contents)?;
        config.theme.validate()?;
        Ok(config)
    }

    /// Tema efetivo: config da aplicação + sobrescrita por
    /// `themes/<name>.toml` no diretório de configuração, se existir.
    pub fn resolved_theme(&self) -> Theme {
        let mut theme = self.theme.resolved();
        if let Ok(dir) = Self::config_dir() {
            let loaded = Theme::load_dir(&dir);
            for error in loaded.errors {
                eprintln!("arandu: tema ignorado: {error}");
            }
            if let Some(file) = loaded
                .themes
                .into_iter()
                .find(|candidate| candidate.name == theme.name)
            {
                theme = file.merge_over(&theme);
            }
        }
        theme
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
                background: Some("#101010".to_owned()),
                panel: None,
                text: Some("#eeeeee".to_owned()),
                text_muted: None,
                border: Some("#333333".to_owned()),
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

    #[test]
    fn theme_roundtrips_through_toml() {
        let theme = Theme {
            name: "solarized".to_owned(),
            dark: true,
            accent: "#268bd2".to_owned(),
            background: Some("#002b36".to_owned()),
            panel: None,
            text: Some("#eee8d5".to_owned()),
            text_muted: Some("#93a1a1".to_owned()),
            border: None,
        };
        let encoded = toml::to_string(&theme).expect("serializa tema");
        let decoded: Theme = toml::from_str(&encoded).expect("desserializa tema");
        assert_eq!(decoded, theme);
    }

    #[test]
    fn load_dir_skips_invalid_theme_and_reports_it() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let themes = dir.path().join("themes");
        std::fs::create_dir(&themes).expect("cria themes");
        std::fs::write(
            themes.join("a.toml"),
            "name = \"alpha\"\naccent = \"#112233\"\n",
        )
        .expect("escreve tema válido");
        std::fs::write(
            themes.join("b.toml"),
            "name = \"beta\"\naccent = \"#445566\"\ndark = false\n",
        )
        .expect("escreve tema válido");
        std::fs::write(
            themes.join("quebrado.toml"),
            "name = \"broken\"\naccent = \"roxo\"\n",
        )
        .expect("escreve tema inválido");

        let loaded = Theme::load_dir(dir.path());
        assert_eq!(loaded.themes.len(), 2);
        assert_eq!(loaded.errors.len(), 1);
        let names: Vec<_> = loaded
            .themes
            .iter()
            .map(|theme| theme.name.as_str())
            .collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
    }

    #[test]
    fn theme_file_overrides_builtin_field_by_field() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let file = dir.path().join("arandu-dark.toml");
        std::fs::write(&file, "name = \"arandu-dark\"\npanel = \"#303030\"\n")
            .expect("escreve tema");
        let loaded = Theme::load_from(&file).expect("carrega tema");

        let merged = loaded.merge_over(&Theme::builtin_dark());
        assert_eq!(merged.name, "arandu-dark");
        assert_eq!(merged.panel.as_deref(), Some("#303030"));
        // Campo não sobrescrito herda do preset.
        assert_eq!(merged.accent, Theme::builtin_dark().accent);
        assert_eq!(merged.background, Theme::builtin_dark().background);
    }

    #[test]
    fn malformed_color_is_an_error() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let file = dir.path().join("bad.toml");
        std::fs::write(&file, "name = \"bad\"\nbackground = \"#12345\"\n").expect("escreve tema");
        let error = Theme::load_from(&file).expect_err("cor inválida deve falhar");
        assert!(
            matches!(error, super::ConfigError::InvalidColor { .. }),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn missing_theme_name_is_an_error() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let file = dir.path().join("anon.toml");
        std::fs::write(&file, "accent = \"#112233\"\n").expect("escreve tema");
        let error = Theme::load_from(&file).expect_err("nome ausente deve falhar");
        assert!(
            matches!(error, super::ConfigError::MissingThemeName { .. }),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn resolved_theme_fills_none_fields_from_builtin() {
        let theme = Theme {
            name: "custom".to_owned(),
            dark: false,
            accent: String::new(),
            background: None,
            panel: Some("#abcdef".to_owned()),
            text: None,
            text_muted: None,
            border: None,
        };
        let resolved = theme.resolved();
        assert_eq!(resolved.accent, Theme::builtin_light().accent);
        assert_eq!(resolved.panel.as_deref(), Some("#abcdef"));
        assert_eq!(resolved.background, Theme::builtin_light().background);
        assert!(!resolved.dark);
    }
}
