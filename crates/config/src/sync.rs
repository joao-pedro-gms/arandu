//! Exportação e importação do diretório de configuração como `tar.gz`.
//!
//! O pacote é um arquivo `tar.gz` com os itens de configuração na raiz:
//! `config.toml`, `keymap.toml`, `tags.toml`, `themes/` e `plugins/`. Itens
//! ausentes são simplesmente omitidos, e a importação **sobrescreve** o
//! diretório atual depois de renomeá-lo para `<dir>.bak-<timestamp>`.
//!
//! A extração é feita em um diretório de preparo e valida cada entrada:
//! caminhos absolutos, com `..` ou com links simbólicos/atalhos são rejeitados
//! (proteção contra zip-slip). Só depois de extrair tudo com sucesso o diretório
//! atual é trocado, então uma falha deixa a configuração anterior intacta.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use tar::{Archive, Builder, EntryType};

use crate::ConfigError;

/// Itens do diretório de configuração incluídos no pacote.
const SYNC_ITEMS: &[&str] = &[
    "config.toml",
    "keymap.toml",
    "tags.toml",
    "themes",
    "plugins",
];

/// Empacota `config_dir` em `dest` (um `tar.gz`).
///
/// Arquivos ou diretórios ausentes em `SYNC_ITEMS` são omitidos sem erro. O
/// diretório pai de `dest` é criado quando necessário.
pub fn export_config(config_dir: &Path, dest: &Path) -> Result<(), ConfigError> {
    if !config_dir.is_dir() {
        return Err(ConfigError::MissingConfigDir {
            path: config_dir.display().to_string(),
        });
    }
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(dest)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);

    for item in SYNC_ITEMS {
        let source = config_dir.join(item);
        match fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_dir() => {
                builder
                    .append_dir_all(item, &source)
                    .map_err(io::Error::other)?;
            }
            Ok(_) => {
                builder
                    .append_path_with_name(&source, item)
                    .map_err(io::Error::other)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    let encoder = builder.into_inner().map_err(io::Error::other)?;
    encoder.finish()?;
    Ok(())
}

/// Importa o pacote `src` para `config_dir`, sobrescrevendo o conteúdo atual.
///
/// Se `config_dir` já existir, ele é renomeado para
/// `<dir>.bak-<timestamp>` antes da troca. Devolve o caminho do backup, quando
/// houve um. O pacote é extraído e validado por completo em um diretório de
/// preparo; em caso de erro, a configuração anterior permanece no lugar.
pub fn import_config(src: &Path, config_dir: &Path) -> Result<Option<PathBuf>, ConfigError> {
    let file = File::open(src)?;
    let mut archive = Archive::new(GzDecoder::new(file));

    let parent = config_dir
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&parent)?;

    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let stamp = elapsed.as_nanos();
    let backup_stamp = elapsed.as_secs();
    let name = config_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arandu".to_owned());
    let staging = parent.join(format!(".{name}.import-{stamp}"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;

    if let Err(error) = extract_into(&mut archive, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let backup = if config_dir.exists() {
        let backup = backup_path(config_dir, backup_stamp);
        fs::rename(config_dir, &backup)?;
        Some(backup)
    } else {
        None
    };

    if let Err(error) = fs::rename(&staging, config_dir) {
        if let Some(backup) = &backup {
            let _ = fs::rename(backup, config_dir);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(error.into());
    }

    Ok(backup)
}

fn extract_into(
    archive: &mut Archive<GzDecoder<File>>,
    destination: &Path,
) -> Result<(), ConfigError> {
    let entries = archive.entries().map_err(io::Error::other)?;
    for entry in entries {
        let mut entry = entry.map_err(io::Error::other)?;
        let name = entry
            .path()
            .map_err(io::Error::other)?
            .to_string_lossy()
            .into_owned();

        let kind = entry.header().entry_type();
        if !is_safe_name(&name) || !is_supported_kind(kind) {
            return Err(ConfigError::UnsafeArchiveEntry { entry: name });
        }

        let unpacked = entry.unpack_in(destination).map_err(io::Error::other)?;
        if !unpacked {
            return Err(ConfigError::UnsafeArchiveEntry { entry: name });
        }
    }
    Ok(())
}

/// `true` quando o nome pode ser juntado com segurança a `destination`
/// (não é absoluto nem contém `..`).
fn is_safe_name(name: &str) -> bool {
    let path = Path::new(name);
    !path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

/// Arquivos regulares e diretórios são aceitos; links e dispositivos não.
fn is_supported_kind(kind: EntryType) -> bool {
    kind.is_file() || kind.is_dir()
}

fn backup_path(config_dir: &Path, stamp: u64) -> PathBuf {
    let name = config_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arandu".to_owned());
    let mut candidate = config_dir.with_file_name(format!("{name}.bak-{stamp}"));
    let mut counter = 1;
    while candidate.exists() {
        candidate = config_dir.with_file_name(format!("{name}.bak-{stamp}-{counter}"));
        counter += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;

    use flate2::Compression;
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;
    use tar::Builder;
    use tempfile::TempDir;

    use super::{export_config, import_config};

    fn config_tree() -> TempDir {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        fs::write(dir.path().join("config.toml"), "locale = \"pt-BR\"\n").expect("escreve config");
        fs::write(dir.path().join("keymap.toml"), "preset = \"vim\"\n").expect("escreve keymap");
        fs::write(dir.path().join("tags.toml"), "[tags]\n").expect("escreve tags");
        fs::create_dir_all(dir.path().join("themes")).expect("cria themes");
        fs::write(dir.path().join("themes/escuro.toml"), "name = \"escuro\"\n")
            .expect("escreve tema");
        fs::create_dir_all(dir.path().join("plugins")).expect("cria plugins");
        fs::write(dir.path().join("plugins/ola.lua"), "return {}\n").expect("escreve plugin");
        dir
    }

    fn entry_names(archive: &std::path::Path) -> Vec<String> {
        let file = fs::File::open(archive).expect("abre pacote");
        let mut tar = tar::Archive::new(GzDecoder::new(file));
        tar.entries()
            .expect("lê entradas")
            .map(|entry| {
                entry
                    .expect("entrada válida")
                    .path()
                    .expect("caminho válido")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn export_creates_tar_gz_with_expected_entries() {
        let origin = config_tree();
        let work = tempfile::tempdir().expect("cria destino");
        let package = work.path().join("cfg.tar.gz");

        export_config(origin.path(), &package).expect("exporta config");

        let names = entry_names(&package);
        assert!(
            names.contains(&"config.toml".to_owned()),
            "entradas: {names:?}"
        );
        assert!(
            names.contains(&"keymap.toml".to_owned()),
            "entradas: {names:?}"
        );
        assert!(
            names.contains(&"tags.toml".to_owned()),
            "entradas: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "themes/escuro.toml"),
            "entradas: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "plugins/ola.lua"),
            "entradas: {names:?}"
        );
    }

    #[test]
    fn import_restores_into_empty_directory() {
        let origin = config_tree();
        let work = tempfile::tempdir().expect("cria destino");
        let package = work.path().join("cfg.tar.gz");
        export_config(origin.path(), &package).expect("exporta config");

        let restored = work.path().join("restaurado");
        let backup = import_config(&package, &restored).expect("importa config");

        assert!(backup.is_none(), "diretório vazio não gera backup");
        assert_eq!(
            fs::read_to_string(restored.join("config.toml")).expect("lê config"),
            "locale = \"pt-BR\"\n"
        );
        assert!(restored.join("themes/escuro.toml").is_file());
        assert!(restored.join("plugins/ola.lua").is_file());
    }

    #[test]
    fn import_backs_up_existing_directory() {
        let origin = config_tree();
        let work = tempfile::tempdir().expect("cria destino");
        let package = work.path().join("cfg.tar.gz");
        export_config(origin.path(), &package).expect("exporta config");

        let target = work.path().join("arandu");
        fs::create_dir_all(&target).expect("cria config atual");
        fs::write(target.join("antigo.txt"), b"antigo").expect("escreve marcador");

        let backup = import_config(&package, &target)
            .expect("importa config")
            .expect("deve criar backup");

        assert!(backup.is_dir(), "backup deve existir: {}", backup.display());
        assert_eq!(
            fs::read(backup.join("antigo.txt")).expect("lê marcador"),
            b"antigo"
        );
        assert!(
            !target.join("antigo.txt").exists(),
            "conteúdo antigo sobrescrito"
        );
        assert!(target.join("config.toml").is_file());
    }

    #[test]
    fn import_rejects_path_traversal() {
        let work = tempfile::tempdir().expect("cria destino");
        let package = work.path().join("malicioso.tar.gz");

        let file = fs::File::create(&package).expect("cria pacote");
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        header.set_mtime(0);
        let name = b"../evil.txt";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();
        builder
            .append(&header, &b"pwned"[..])
            .expect("escreve entrada maliciosa");
        let encoder = builder.into_inner().expect("finaliza tar");
        encoder.finish().expect("finaliza gzip");

        let target = work.path().join("arandu");
        let error = import_config(&package, &target).expect_err("zip-slip deve falhar");
        assert!(
            matches!(error, crate::ConfigError::UnsafeArchiveEntry { .. }),
            "erro inesperado: {error:?}"
        );
        assert!(
            !work.path().join("evil.txt").exists(),
            "nada pode escapar do diretório de configuração"
        );
        assert!(!target.exists(), "falha não deve deixar diretório parcial");
    }

    #[test]
    fn export_skips_absent_items_and_import_accepts_minimal_package() {
        let origin = tempfile::tempdir().expect("cria origem");
        fs::write(origin.path().join("config.toml"), "locale = \"en\"\n").expect("escreve config");
        let work = tempfile::tempdir().expect("cria destino");
        let package = work.path().join("minimo.tar.gz");

        export_config(origin.path(), &package).expect("exporta config");
        let names = entry_names(&package);
        assert_eq!(names, vec!["config.toml".to_owned()]);

        let target = work.path().join("restaurado");
        import_config(&package, &target).expect("importa pacote mínimo");
        let mut contents = String::new();
        fs::File::open(target.join("config.toml"))
            .expect("abre config")
            .read_to_string(&mut contents)
            .expect("lê config");
        assert_eq!(contents, "locale = \"en\"\n");
    }
}
