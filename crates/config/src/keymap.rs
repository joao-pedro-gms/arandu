//! Keymap redefinível com presets embutidos (`default` e `vim`).
//!
//! O arquivo fica em `<config_dir>/keymap.toml`:
//!
//! ```toml
//! preset = "vim"
//!
//! [overrides]
//! toggle_preview = [{ key = "P" }]
//! cycle_theme = [{ key = "T", mods = { ctrl = true } }]
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// Ações executáveis por atalho.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    TogglePreview,
    ToggleGitPanel,
    TogglePanel,
    NavigateUp,
    NavigateDown,
    Enter,
    GoBack,
    Copy,
    Move,
    Rename,
    Delete,
    Undo,
    CycleTheme,
    Quit,
    ShowHints,
    TagFiles,
    Checksums,
    BatchRename,
    Compress,
    Extract,
    ToggleViewMode,
    ToggleQuickOpen,
    ToggleGitLog,
}

impl Action {
    /// Nome usado no TOML (`snake_case`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TogglePreview => "toggle_preview",
            Self::ToggleGitPanel => "toggle_git_panel",
            Self::TogglePanel => "toggle_panel",
            Self::NavigateUp => "navigate_up",
            Self::NavigateDown => "navigate_down",
            Self::Enter => "enter",
            Self::GoBack => "go_back",
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Rename => "rename",
            Self::Delete => "delete",
            Self::Undo => "undo",
            Self::CycleTheme => "cycle_theme",
            Self::Quit => "quit",
            Self::ShowHints => "show_hints",
            Self::TagFiles => "tag_files",
            Self::Checksums => "checksums",
            Self::BatchRename => "batch_rename",
            Self::Compress => "compress",
            Self::Extract => "extract",
            Self::ToggleViewMode => "toggle_view_mode",
            Self::ToggleQuickOpen => "toggle_quick_open",
            Self::ToggleGitLog => "toggle_git_log",
        }
    }

    /// Todas as ações, na ordem canônica do arquivo.
    pub const ALL: [Action; 23] = [
        Self::TogglePreview,
        Self::ToggleGitPanel,
        Self::TogglePanel,
        Self::NavigateUp,
        Self::NavigateDown,
        Self::Enter,
        Self::GoBack,
        Self::Copy,
        Self::Move,
        Self::Rename,
        Self::Delete,
        Self::Undo,
        Self::CycleTheme,
        Self::Quit,
        Self::ShowHints,
        Self::TagFiles,
        Self::Checksums,
        Self::BatchRename,
        Self::Compress,
        Self::Extract,
        Self::ToggleViewMode,
        Self::ToggleQuickOpen,
        Self::ToggleGitLog,
    ];

    /// Converte um nome `snake_case` em ação.
    pub fn from_name(name: &str) -> Result<Self, ConfigError> {
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == name)
            .ok_or_else(|| ConfigError::UnknownAction(name.to_owned()))
    }
}

/// Modificadores de uma combinação de teclas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
    };

    pub const CTRL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
        logo: false,
    };

    fn matches(self, other: Self, key_is_letter: bool) -> bool {
        self.ctrl == other.ctrl
            && self.alt == other.alt
            && self.logo == other.logo
            && (key_is_letter || self.shift == other.shift)
    }
}

/// Uma tecla (nome no vocabulário do egui, ex.: `F3`, `ArrowUp`, `J`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyBinding {
    #[serde(default)]
    pub mods: Modifiers,
    pub key: String,
}

impl KeyBinding {
    /// Cria um binding sem modificadores.
    pub fn key(key: impl Into<String>) -> Self {
        Self {
            mods: Modifiers::NONE,
            key: key.into(),
        }
    }

    /// Cria um binding com modificadores.
    pub fn with_mods(mods: Modifiers, key: impl Into<String>) -> Self {
        Self {
            mods,
            key: key.into(),
        }
    }

    /// `true` quando a tecla é uma letra (usada no matching case-insensitive).
    pub fn key_is_letter(&self) -> bool {
        let mut chars = self.key.chars();
        matches!((chars.next(), chars.next()), (Some(ch), None) if ch.is_ascii_alphabetic())
    }

    /// O binding casa com uma tecla pressionada?
    ///
    /// Letras são comparadas sem diferenciar maiúsculas/minúsculas e aceitam
    /// Shift livremente; demais teclas exigem Shift igual.
    pub fn matches(&self, pressed: &KeyBinding) -> bool {
        let same_key = if self.key_is_letter() {
            self.key.eq_ignore_ascii_case(&pressed.key)
        } else {
            self.key == pressed.key
        };
        same_key && self.mods.matches(pressed.mods, self.key_is_letter())
    }
}

impl fmt::Display for KeyBinding {
    /// Formato compacto para a UI: modificadores na ordem
    /// Ctrl, Alt, Shift, Logo, separados por `+` (ex.: `Ctrl+Shift+Z`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (enabled, label) in [
            (self.mods.ctrl, "Ctrl"),
            (self.mods.alt, "Alt"),
            (self.mods.shift, "Shift"),
            (self.mods.logo, "Logo"),
        ] {
            if enabled {
                if !first {
                    f.write_str("+")?;
                }
                f.write_str(label)?;
                first = false;
            }
        }
        if !first {
            f.write_str("+")?;
        }
        f.write_str(display_key(&self.key))
    }
}

/// Nome curto da tecla para exibição (ex.: `Delete` vira `Del`).
fn display_key(key: &str) -> &str {
    match key {
        "Delete" => "Del",
        other => other,
    }
}

/// Keymap com preset e sobrescritas do usuário.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keymap {
    /// Preset base (`default` ou `vim`).
    pub preset: String,
    /// Sobrescritas por nome de ação (`snake_case`).
    pub overrides: BTreeMap<String, Vec<KeyBinding>>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            preset: "default".to_owned(),
            overrides: BTreeMap::new(),
        }
    }
}

impl Keymap {
    /// Carrega `<config_dir>/keymap.toml`, usando o padrão se não existir.
    pub fn load() -> Result<Self, ConfigError> {
        let path = crate::Config::config_dir()?.join("keymap.toml");
        Self::load_from(path)
    }

    /// Carrega o keymap de um caminho, usando o padrão se não existir.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)?;
        Self::from_toml(&contents)
    }

    /// Desserializa e valida um keymap a partir de TOML.
    pub fn from_toml(contents: &str) -> Result<Self, ConfigError> {
        let keymap: Self = toml::from_str(contents)?;
        keymap.validate()?;
        Ok(keymap)
    }

    /// Serializa o keymap como TOML.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.preset != "default" && self.preset != "vim" {
            return Err(ConfigError::UnknownKeymapPreset(self.preset.clone()));
        }
        for (action_name, bindings) in &self.overrides {
            Action::from_name(action_name)?;
            for binding in bindings {
                if !is_valid_key(&binding.key) {
                    return Err(ConfigError::InvalidKey {
                        action: action_name.clone(),
                        key: binding.key.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Bindings efetivos de uma ação: sobrescrita do usuário ou preset.
    pub fn effective(&self, action: Action) -> Vec<KeyBinding> {
        if let Some(bindings) = self.overrides.get(action.as_str()) {
            return bindings.clone();
        }
        self.preset_bindings(action)
    }

    /// Bindings embutidos do preset ativo (usados quando não há override).
    pub fn preset_bindings(&self, action: Action) -> Vec<KeyBinding> {
        if self.preset == "vim" {
            vim_bindings(action)
        } else {
            default_bindings(action)
        }
    }
}

/// Nomes de teclas aceitos (vocabulário do egui, sem prefixos).
pub fn is_valid_key(key: &str) -> bool {
    if key.len() == 1 {
        let ch = key.chars().next().expect("uma letra");
        return ch.is_ascii_alphanumeric();
    }
    const NAMED: [&str; 27] = [
        "F1",
        "F2",
        "F3",
        "F4",
        "F5",
        "F6",
        "F7",
        "F8",
        "F9",
        "F10",
        "F11",
        "F12",
        "Tab",
        "Enter",
        "Escape",
        "Backspace",
        "Delete",
        "Insert",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "Space",
    ];
    NAMED.contains(&key)
}

fn kb(key: &str) -> KeyBinding {
    KeyBinding::key(key)
}

fn ctrl(key: &str) -> KeyBinding {
    KeyBinding::with_mods(Modifiers::CTRL, key)
}

fn default_bindings(action: Action) -> Vec<KeyBinding> {
    match action {
        Action::TogglePreview => vec![kb("F3")],
        Action::ToggleGitPanel => vec![ctrl("G")],
        Action::TogglePanel => vec![kb("Tab")],
        Action::NavigateUp => vec![kb("ArrowUp")],
        Action::NavigateDown => vec![kb("ArrowDown")],
        Action::Enter => vec![kb("Enter")],
        Action::GoBack => vec![kb("Backspace")],
        Action::Copy => vec![kb("F5")],
        Action::Move => vec![kb("F6")],
        Action::Rename => vec![kb("F2")],
        Action::Delete => vec![kb("Delete")],
        Action::Undo => vec![ctrl("Z")],
        Action::CycleTheme => vec![ctrl("T")],
        Action::Quit => vec![ctrl("Q")],
        Action::ShowHints => vec![kb("F1")],
        Action::TagFiles => vec![kb("F4")],
        Action::Checksums => vec![kb("F9")],
        Action::BatchRename => vec![kb("F7")],
        Action::Compress => vec![kb("F8")],
        Action::Extract => vec![kb("F10")],
        // Um único atalho cicla lista -> ícones -> lista.
        Action::ToggleViewMode => vec![ctrl("1")],
        Action::ToggleQuickOpen => vec![ctrl("P")],
        Action::ToggleGitLog => vec![KeyBinding::with_mods(
            Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            },
            "G",
        )],
    }
}

fn vim_bindings(action: Action) -> Vec<KeyBinding> {
    match action {
        Action::TogglePreview => vec![kb("P")],
        Action::ToggleGitPanel => vec![ctrl("G")],
        Action::TogglePanel => vec![kb("Tab")],
        Action::NavigateUp => vec![kb("K")],
        Action::NavigateDown => vec![kb("J")],
        Action::Enter => vec![kb("L"), kb("Enter")],
        Action::GoBack => vec![kb("H"), kb("Backspace")],
        Action::Copy => vec![kb("Y")],
        // Sem equivalente Vim canônico (o usuário pode usar `keymap.toml`);
        // `D` fica reservado para `Delete` (lixeira).
        Action::Move => vec![],
        Action::Rename => vec![kb("R")],
        Action::Delete => vec![kb("D")],
        Action::Undo => vec![kb("U")],
        Action::CycleTheme => vec![kb("T")],
        Action::Quit => vec![kb("Q")],
        Action::ShowHints => vec![kb("F1")],
        Action::TagFiles => vec![kb("F4")],
        Action::Checksums => vec![kb("F9")],
        Action::BatchRename => vec![kb("F7")],
        Action::Compress => vec![kb("F8")],
        Action::Extract => vec![kb("F10")],
        Action::ToggleViewMode => vec![ctrl("1")],
        Action::ToggleQuickOpen => vec![ctrl("P")],
        Action::ToggleGitLog => vec![KeyBinding::with_mods(
            Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            },
            "G",
        )],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Action, KeyBinding, Keymap, Modifiers};

    #[test]
    fn roundtrips_keymap_through_toml() {
        let mut keymap = Keymap {
            preset: "vim".to_owned(),
            ..Keymap::default()
        };
        keymap.overrides.insert(
            "toggle_preview".to_owned(),
            vec![KeyBinding::with_mods(Modifiers::CTRL, "P")],
        );

        let encoded = keymap.to_toml().expect("serializa keymap");
        let decoded = Keymap::from_toml(&encoded).expect("desserializa keymap");
        assert_eq!(decoded, keymap);
    }

    #[test]
    fn vim_preset_resolves_expected_bindings() {
        let keymap = Keymap {
            preset: "vim".to_owned(),
            ..Keymap::default()
        };
        let keys = |action| -> Vec<String> {
            keymap
                .effective(action)
                .into_iter()
                .map(|binding| binding.key)
                .collect()
        };
        assert_eq!(keys(Action::NavigateDown), vec!["J"]);
        assert_eq!(keys(Action::NavigateUp), vec!["K"]);
        assert_eq!(keys(Action::Enter), vec!["L", "Enter"]);
        assert_eq!(keys(Action::GoBack), vec!["H", "Backspace"]);
        assert_eq!(keys(Action::TogglePanel), vec!["Tab"]);
        assert_eq!(keys(Action::CycleTheme), vec!["T"]);
    }

    #[test]
    fn user_override_wins_over_preset() {
        let mut keymap = Keymap::default();
        keymap
            .overrides
            .insert("copy".to_owned(), vec![KeyBinding::key("C")]);
        let effective = keymap.effective(Action::Copy);
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].key, "C");
        // Ação sem override continua no preset.
        assert_eq!(keymap.effective(Action::Move)[0].key, "F6");
    }

    #[test]
    fn unknown_action_in_toml_is_an_error() {
        let error =
            Keymap::from_toml("preset = \"default\"\n[overrides]\npular = [{ key = \"J\" }]\n")
                .expect_err("ação desconhecida deve falhar");
        assert!(
            matches!(error, super::ConfigError::UnknownAction(_)),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn invalid_key_is_an_error() {
        let error = Keymap::from_toml(
            "preset = \"default\"\n[overrides]\ncopy = [{ key = \"TeclaEstranha\" }]\n",
        )
        .expect_err("tecla inválida deve falhar");
        assert!(
            matches!(error, super::ConfigError::InvalidKey { .. }),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn unknown_preset_is_an_error() {
        let error = Keymap::from_toml("preset = \"emacs\"\n").expect_err("preset inválido");
        assert!(
            matches!(error, super::ConfigError::UnknownKeymapPreset(_)),
            "erro inesperado: {error:?}"
        );
    }

    #[test]
    fn letter_binding_matches_case_insensitively_and_ignores_shift() {
        let binding = KeyBinding::key("J");
        let upper = KeyBinding::with_mods(
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            "J",
        );
        let lower = KeyBinding::key("j");
        assert!(binding.matches(&upper));
        assert!(binding.matches(&lower));
    }

    #[test]
    fn ctrl_binding_requires_ctrl() {
        let binding = KeyBinding::with_mods(Modifiers::CTRL, "Z");
        assert!(binding.matches(&KeyBinding::with_mods(Modifiers::CTRL, "Z")));
        assert!(!binding.matches(&KeyBinding::key("Z")));
    }

    #[test]
    fn display_formats_bindings_compactly() {
        assert_eq!(KeyBinding::key("F3").to_string(), "F3");
        assert_eq!(KeyBinding::key("J").to_string(), "J");
        assert_eq!(
            KeyBinding::with_mods(Modifiers::CTRL, "T").to_string(),
            "Ctrl+T"
        );
        assert_eq!(
            KeyBinding::with_mods(
                Modifiers {
                    ctrl: true,
                    alt: false,
                    shift: true,
                    logo: false,
                },
                "Z",
            )
            .to_string(),
            "Ctrl+Shift+Z"
        );
        assert_eq!(
            KeyBinding::with_mods(
                Modifiers {
                    ctrl: true,
                    alt: true,
                    shift: true,
                    logo: true,
                },
                "Z",
            )
            .to_string(),
            "Ctrl+Alt+Shift+Logo+Z"
        );
        assert_eq!(KeyBinding::key("Delete").to_string(), "Del");
    }

    #[test]
    fn show_hints_is_bound_to_f1_in_both_presets() {
        for preset in ["default", "vim"] {
            let keymap = Keymap {
                preset: preset.to_owned(),
                ..Keymap::default()
            };
            assert_eq!(
                keymap.effective(Action::ShowHints),
                vec![KeyBinding::key("F1")]
            );
        }
    }

    #[test]
    fn file_tools_actions_have_free_function_keys_in_both_presets() {
        for preset in ["default", "vim"] {
            let keymap = Keymap {
                preset: preset.to_owned(),
                ..Keymap::default()
            };
            assert_eq!(
                keymap.effective(Action::Checksums),
                vec![KeyBinding::key("F9")]
            );
            assert_eq!(
                keymap.effective(Action::BatchRename),
                vec![KeyBinding::key("F7")]
            );
            assert_eq!(
                keymap.effective(Action::Compress),
                vec![KeyBinding::key("F8")]
            );
            assert_eq!(
                keymap.effective(Action::Extract),
                vec![KeyBinding::key("F10")]
            );
        }
    }

    #[test]
    fn file_tools_actions_roundtrip_by_name() {
        assert_eq!(
            Action::from_name("checksums").expect("checksums"),
            Action::Checksums
        );
        assert_eq!(
            Action::from_name("batch_rename").expect("batch_rename"),
            Action::BatchRename
        );
        assert_eq!(
            Action::from_name("compress").expect("compress"),
            Action::Compress
        );
        assert_eq!(
            Action::from_name("extract").expect("extract"),
            Action::Extract
        );
    }

    #[test]
    fn git_panel_uses_ctrl_g_in_both_presets() {
        for preset in ["default", "vim"] {
            let keymap = Keymap {
                preset: preset.to_owned(),
                ..Keymap::default()
            };
            assert_eq!(
                keymap.effective(Action::ToggleGitPanel),
                vec![KeyBinding::with_mods(Modifiers::CTRL, "G")]
            );
        }
    }

    #[test]
    fn toggle_view_mode_is_bound_to_ctrl_1_in_both_presets() {
        for preset in ["default", "vim"] {
            let keymap = Keymap {
                preset: preset.to_owned(),
                ..Keymap::default()
            };
            assert_eq!(
                keymap.effective(Action::ToggleViewMode),
                vec![KeyBinding::with_mods(Modifiers::CTRL, "1")]
            );
        }
    }

    #[test]
    fn quick_open_is_bound_to_ctrl_p_in_both_presets() {
        for preset in ["default", "vim"] {
            let keymap = Keymap {
                preset: preset.to_owned(),
                ..Keymap::default()
            };
            assert_eq!(
                keymap.effective(Action::ToggleQuickOpen),
                vec![KeyBinding::with_mods(Modifiers::CTRL, "P")]
            );
        }
    }

    #[test]
    fn all_actions_are_unique_and_roundtrip() {
        let mut names = BTreeSet::new();
        for action in Action::ALL {
            assert!(
                names.insert(action.as_str()),
                "duplicada: {}",
                action.as_str()
            );
            assert_eq!(Action::from_name(action.as_str()).expect("ação"), action);
        }
        assert_eq!(names.len(), Action::ALL.len());
    }
}
