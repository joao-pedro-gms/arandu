//! Interface gráfica do Arandu, construída com egui/eframe.
//!
//! Os textos são carregados dos catálogos em `locales/` via `rust-i18n`, que
//! suporta TOML nativamente — por isso mantemos os catálogos em TOML, o mesmo
//! formato usado pela configuração da aplicação.

rust_i18n::i18n!("locales", fallback = "en");

use arandu_config::Config;
use eframe::egui;

/// Nome exibido da aplicação.
pub const APP_NAME: &str = "Arandu";

/// Aplicação principal do Arandu.
pub struct AranduApp {
    config: Config,
}

impl AranduApp {
    /// Cria a aplicação aplicando o locale da configuração.
    pub fn new(config: Config) -> Self {
        rust_i18n::set_locale(&config.locale);
        Self { config }
    }

    /// Configuração carregada na inicialização.
    pub fn config(&self) -> &Config {
        &self.config
    }
}

impl Default for AranduApp {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl eframe::App for AranduApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let welcome = rust_i18n::t!("welcome").to_string();
        let hint = rust_i18n::t!("welcome_hint").to_string();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(64.0);
                ui.heading(welcome);
                ui.label(format!("{APP_NAME} v{}", env!("CARGO_PKG_VERSION")));
                ui.add_space(8.0);
                ui.label(hint);
            });
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn loads_toml_locales() {
        rust_i18n::set_locale("pt-BR");
        assert_eq!(rust_i18n::t!("welcome"), "Bem-vindo ao Arandu");

        rust_i18n::set_locale("en");
        assert_eq!(rust_i18n::t!("welcome"), "Welcome to Arandu");
    }
}
