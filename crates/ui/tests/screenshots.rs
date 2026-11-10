//! Snapshots de tela da interface (feature `screenshot-tests`).
//!
//! Renderizam a aplicação num harness headless do `egui_kittest` com backend
//! wgpu e comparam contra os PNGs em `crates/ui/tests/snapshots/`.
//!
//! Para (re)gerar os snapshots localmente (lavapipe):
//!
//! ```text
//! WGPU_BACKEND=vulkan \
//! VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
//! UPDATE_SNAPSHOTS=force \
//! cargo test -p arandu-ui --features screenshot-tests --test screenshots
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use arandu_config::{Config, Theme};
use arandu_ui::AranduApp;
use eframe::egui;
use egui_kittest::Harness;
use filetime::FileTime;

/// Instante fixo para os mtimes do fixture (2023-11-14 22:13:20 UTC), para a
/// coluna "Modificado" não variar entre execuções.
const FIXTURE_MTIME: i64 = 1_700_000_000;

/// Diretório do fixture.
///
/// O caminho completo aparece na barra de status, então precisa ser estável
/// entre máquinas — daí `/tmp` fixo em vez de `tempdir()` (que gera nomes
/// aleatórios). Os snapshots são comparados apenas no job Linux do CI.
fn fixture_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = if cfg!(unix) {
            PathBuf::from("/tmp/arandu-kittest-fixture")
        } else {
            std::env::temp_dir().join("arandu-kittest-fixture")
        };
        // Recria para descartar restos de uma execução anterior interrompida.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("fixture-dir")).expect("cria diretório do fixture");
        fs::write(dir.join("fixture-file.txt"), b"conteudo").expect("cria arquivo do fixture");

        let when = FileTime::from_unix_time(FIXTURE_MTIME, 0);
        for name in ["fixture-file.txt", "fixture-dir"] {
            filetime::set_file_mtime(dir.join(name), when).expect("ajusta mtime do fixture");
        }
        dir
    })
}

fn dark_app() -> AranduApp {
    AranduApp::new_at_isolated(Config::default(), fixture_dir())
}

fn light_app() -> AranduApp {
    let config = Config {
        theme: Theme::builtin_light(),
        ..Config::default()
    };
    AranduApp::new_at_isolated(config, fixture_dir())
}

fn render(app: AranduApp) -> Harness<'static, AranduApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .wgpu()
        .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
    harness.run();
    harness
}

#[test]
fn main_window_dark() {
    let mut harness = render(dark_app());
    harness.snapshot("main_window_dark");
}

#[test]
fn main_window_light() {
    let mut harness = render(light_app());
    harness.snapshot("main_window_light");
}
