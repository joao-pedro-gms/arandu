use anyhow::Context;
use arandu_config::Config;
use arandu_ui::{APP_NAME, AranduApp};
use eframe::NativeOptions;
use eframe::egui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load().unwrap_or_else(|err| {
        eprintln!("aviso: não foi possível carregar a configuração ({err}); usando padrões");
        Config::default()
    });

    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1024.0, 700.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |_cc| Ok(Box::new(AranduApp::new(config)))),
    )
    .map_err(|err| anyhow::anyhow!("falha ao iniciar a interface: {err}"))
    .context("executar a aplicação Arandu")
}
