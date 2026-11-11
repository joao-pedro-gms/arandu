use std::path::Path;

use anyhow::Context;
use arandu_config::{Config, export_config, import_config};
use arandu_ui::{APP_NAME, AranduApp};
use eframe::NativeOptions;
use eframe::egui;

/// Texto curto de ajuda das flags de linha de comando.
const HELP: &str = "\
Arandu — gerenciador de arquivos

Uso:
  arandu                          inicia a interface gráfica
  arandu --export-config <arq>    exporta o diretório de configuração para <arq> (tar.gz)
  arandu --import-config <arq>    importa a configuração de <arq> (tar.gz), com backup da atual
  arandu --help                   mostra esta ajuda

O pacote inclui config.toml, keymap.toml, tags.toml, themes/ e plugins/.";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.as_slice() {
        [] => {}
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{HELP}");
            return Ok(());
        }
        [flag, path] if flag == "--export-config" => {
            run_export(Path::new(path))?;
            return Ok(());
        }
        [flag, path] if flag == "--import-config" => {
            run_import(Path::new(path))?;
            return Ok(());
        }
        _ => {
            eprintln!("argumento inválido\n");
            eprintln!("{HELP}");
            std::process::exit(2);
        }
    }

    run_gui()
}

fn run_export(path: &Path) -> anyhow::Result<()> {
    let config_dir = Config::config_dir().context("descobrir o diretório de configuração")?;
    export_config(&config_dir, path).context("exportar a configuração")?;
    println!(
        "configuração exportada de {} para {}",
        config_dir.display(),
        path.display()
    );
    Ok(())
}

fn run_import(path: &Path) -> anyhow::Result<()> {
    let config_dir = Config::config_dir().context("descobrir o diretório de configuração")?;
    let backup = import_config(path, &config_dir).context("importar a configuração")?;
    println!(
        "configuração importada de {} para {}",
        path.display(),
        config_dir.display()
    );
    if let Some(backup) = backup {
        println!("backup da configuração anterior: {}", backup.display());
    }
    Ok(())
}

#[tokio::main]
async fn run_gui() -> anyhow::Result<()> {
    let config = Config::load().unwrap_or_else(|err| {
        eprintln!("aviso: não foi possível carregar a configuração ({err}); usando padrões");
        Config::default()
    });

    let options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_NAME)
            .with_inner_size([1024.0, 700.0])
            .with_min_inner_size([720.0, 480.0]),
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
