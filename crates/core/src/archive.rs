//! Criação e extração de arquivos compactados (zip e tar).
//!
//! O suporte cobre os formatos com implementação madura e puramente Rust:
//! zip (via `zip` com deflate) e tar (via `tar`). **7z fica fora do escopo**:
//! não há crate mantida e puramente Rust para o formato; adicionar uma
//! dependência C/FFI quebraria a política de build portátil do projeto.
//!
//! Os entry names são relativos ao diretório pai comum dos `sources`: cada
//! origem contribui com o próprio nome (arquivo) ou com o próprio nome como
//! prefixo da árvore (diretório).
//!
//! # Segurança (zip-slip)
//!
//! Na extração, toda entrada com caminho absoluto ou contendo `..` é rejeitada
//! com erro — nada é escrito fora de `destination`.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::fs::CoreError;

/// Formatos de arquivo compactado reconhecidos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// Zip com compressão deflate.
    Zip,
    /// Tar sem compressão.
    Tar,
}

/// Erros internos da camada de compactação.
#[derive(Debug, Error)]
pub enum ArchiveError {
    /// Erro devolvido pela crate `zip`.
    #[error("erro de zip: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// Erro de I/O do sistema ou da crate `tar`.
    #[error("erro de I/O: {0}")]
    Io(#[from] io::Error),

    /// Entrada com caminho inseguro (`..`, absoluto) ou nome inválido.
    #[error("entrada insegura: {entry}")]
    UnsafeEntry {
        /// Nome da entrada rejeitada.
        entry: String,
    },

    /// Formato não reconhecido pela extensão.
    #[error("formato de arquivo não suportado: {path}")]
    UnsupportedFormat {
        /// Caminho informado.
        path: String,
    },
}

/// Detecta o formato de um arquivo pela extensão.
///
/// `.zip` → [`ArchiveFormat::Zip`]; `.tar`, `.tar.gz` e `.tgz` →
/// [`ArchiveFormat::Tar`]. Qualquer outra extensão devolve `None`.
pub fn detect_format(path: &Path) -> Option<ArchiveFormat> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.ends_with(".zip") {
        Some(ArchiveFormat::Zip)
    } else if name.ends_with(".tar") || name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveFormat::Tar)
    } else {
        None
    }
}

/// Cria um zip com `sources` em `destination`, comprimindo com deflate.
///
/// Cada origem entra com o próprio nome; diretórios são percorridos
/// recursivamente e o prefixo acompanha a árvore.
pub fn create_zip(sources: &[PathBuf], destination: &Path) -> Result<(), CoreError> {
    let file = File::create(destination).map_err(|err| CoreError::from_io(destination, err))?;
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    for source in sources {
        let name = entry_root(source)?;
        add_to_zip(&mut writer, source, &name, options)
            .map_err(|error| archive_error(source, error))?;
    }

    writer
        .finish()
        .map_err(|source| archive_error(destination, ArchiveError::Zip(source)))?;
    Ok(())
}

/// Cria um tar (sem compressão) com `sources` em `destination`.
pub fn create_tar(sources: &[PathBuf], destination: &Path) -> Result<(), CoreError> {
    let file = File::create(destination).map_err(|err| CoreError::from_io(destination, err))?;
    let mut builder = tar::Builder::new(file);

    for source in sources {
        let name = entry_root(source)?;
        let metadata =
            fs::symlink_metadata(source).map_err(|err| CoreError::from_io(source, err))?;
        let result = if metadata.is_dir() {
            builder.append_dir_all(&name, source)
        } else {
            builder.append_path_with_name(source, &name)
        };
        result.map_err(|error| archive_error(source, ArchiveError::Io(error)))?;
    }

    builder
        .finish()
        .map_err(|error| archive_error(destination, ArchiveError::Io(error)))?;
    Ok(())
}

/// Extrai `archive` para `destination`, detectando o formato pela extensão.
///
/// `destination` é criado se não existir. Entradas com caminho absoluto ou
/// contendo `..` são rejeitadas (proteção contra zip-slip).
pub fn extract(archive: &Path, destination: &Path) -> Result<(), CoreError> {
    let format = detect_format(archive).ok_or_else(|| {
        archive_error(
            archive,
            ArchiveError::UnsupportedFormat {
                path: archive.display().to_string(),
            },
        )
    })?;

    fs::create_dir_all(destination).map_err(|err| CoreError::from_io(destination, err))?;

    match format {
        ArchiveFormat::Zip => extract_zip(archive, destination),
        ArchiveFormat::Tar => extract_tar(archive, destination),
    }
}

fn extract_zip(archive: &Path, destination: &Path) -> Result<(), CoreError> {
    let file = File::open(archive).map_err(|err| CoreError::from_io(archive, err))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|source| archive_error(archive, ArchiveError::Zip(source)))?;

    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|source| archive_error(archive, ArchiveError::Zip(source)))?;

        let name = entry
            .name()
            .map_err(|source| archive_error(archive, ArchiveError::Zip(source)))?
            .into_owned();
        let relative = entry.enclosed_name().filter(|_| is_safe_name(&name));
        let Some(relative) = relative else {
            return Err(archive_error(
                archive,
                ArchiveError::UnsafeEntry { entry: name },
            ));
        };

        let target = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|err| CoreError::from_io(&target, err))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|err| CoreError::from_io(parent, err))?;
            }
            let mut output =
                File::create(&target).map_err(|err| CoreError::from_io(&target, err))?;
            io::copy(&mut entry, &mut output).map_err(|err| CoreError::from_io(&target, err))?;
        }
    }

    Ok(())
}

fn extract_tar(archive: &Path, destination: &Path) -> Result<(), CoreError> {
    let file = File::open(archive).map_err(|err| CoreError::from_io(archive, err))?;
    let mut tar = tar::Archive::new(file);
    let entries = tar
        .entries()
        .map_err(|err| archive_error(archive, ArchiveError::Io(err)))?;

    for entry in entries {
        let mut entry = entry.map_err(|err| archive_error(archive, ArchiveError::Io(err)))?;
        let name = entry
            .path()
            .map_err(|err| archive_error(archive, ArchiveError::Io(err)))?
            .to_string_lossy()
            .into_owned();

        // `unpack_in` já sanitiza, mas validamos o nome e o retorno para
        // tratar qualquer entrada fora de `destination` como erro explícito.
        if !is_safe_name(&name) {
            return Err(archive_error(
                archive,
                ArchiveError::UnsafeEntry { entry: name },
            ));
        }
        let unpacked = entry
            .unpack_in(destination)
            .map_err(|err| archive_error(archive, ArchiveError::Io(err)))?;
        if !unpacked {
            return Err(archive_error(
                archive,
                ArchiveError::UnsafeEntry { entry: name },
            ));
        }
    }

    Ok(())
}

/// `true` quando o nome da entrada pode ser juntado com segurança a
/// `destination` (não é absoluto nem contém `..`).
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

/// Nome da origem dentro do arquivo (o próprio `file_name`).
fn entry_root(source: &Path) -> Result<String, CoreError> {
    source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| {
            archive_error(
                source,
                ArchiveError::UnsafeEntry {
                    entry: source.display().to_string(),
                },
            )
        })
}

fn add_to_zip<W: io::Write + io::Seek>(
    writer: &mut zip::ZipWriter<W>,
    source: &Path,
    entry: &str,
    options: zip::write::SimpleFileOptions,
) -> Result<(), ArchiveError> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        writer.add_directory(format!("{entry}/"), options)?;
        let mut children: Vec<PathBuf> = fs::read_dir(source)?
            .filter_map(Result::ok)
            .map(|child| child.path())
            .collect();
        children.sort();
        for child in children {
            let name = child
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            add_to_zip(writer, &child, &format!("{entry}/{name}"), options)?;
        }
    } else {
        let mut file = File::open(source)?;
        writer.start_file(entry, options)?;
        io::copy(&mut file, writer)?;
    }
    Ok(())
}

/// Normaliza o caminho para o reporte de erro.
fn archive_error(path: &Path, source: ArchiveError) -> CoreError {
    CoreError::Archive {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    use super::{ArchiveFormat, create_tar, create_zip, detect_format, extract};
    use tempfile::TempDir;

    fn tree() -> (TempDir, TempDir) {
        let work = tempfile::tempdir().expect("cria diretório temporário");
        let origin = work.path().join("origem");
        fs::create_dir_all(origin.join("nested")).expect("cria nested");
        fs::write(origin.join("a.txt"), b"aaa").expect("cria a.txt");
        fs::write(origin.join("nested/b.txt"), b"bbbbb").expect("cria b.txt");
        let dest = tempfile::tempdir().expect("cria destino");
        (work, dest)
    }

    #[test]
    fn detect_format_recognizes_supported_extensions() {
        assert_eq!(
            detect_format(Path::new("arquivo.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            detect_format(Path::new("arquivo.tar")),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(
            detect_format(Path::new("arquivo.tar.gz")),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(
            detect_format(Path::new("arquivo.tgz")),
            Some(ArchiveFormat::Tar)
        );
        assert_eq!(detect_format(Path::new("arquivo.7z")), None);
        assert_eq!(detect_format(Path::new("arquivo.rar")), None);
    }

    #[test]
    fn zip_roundtrip_preserves_files_and_nested_dirs() {
        let (work, dest) = tree();
        let archive = work.path().join("saida.zip");
        create_zip(&[work.path().join("origem")], &archive).expect("cria zip");

        extract(&archive, dest.path()).expect("extrai zip");
        assert_eq!(
            fs::read(dest.path().join("origem/a.txt")).expect("lê a"),
            b"aaa"
        );
        assert_eq!(
            fs::read(dest.path().join("origem/nested/b.txt")).expect("lê b"),
            b"bbbbb"
        );
    }

    #[test]
    fn zip_of_single_file_uses_file_name_as_entry() {
        let (work, dest) = tree();
        let archive = work.path().join("arquivo.zip");
        create_zip(&[work.path().join("origem/a.txt")], &archive).expect("cria zip");

        extract(&archive, dest.path()).expect("extrai zip");
        assert_eq!(fs::read(dest.path().join("a.txt")).expect("lê a"), b"aaa");
    }

    #[test]
    fn tar_roundtrip_preserves_files_and_nested_dirs() {
        let (work, dest) = tree();
        let archive = work.path().join("saida.tar");
        create_tar(&[work.path().join("origem")], &archive).expect("cria tar");

        extract(&archive, dest.path()).expect("extrai tar");
        assert_eq!(
            fs::read(dest.path().join("origem/a.txt")).expect("lê a"),
            b"aaa"
        );
        assert_eq!(
            fs::read(dest.path().join("origem/nested/b.txt")).expect("lê b"),
            b"bbbbb"
        );
    }

    #[test]
    fn zip_slip_entry_is_rejected() {
        let work = tempfile::tempdir().expect("cria diretório temporário");
        let dest = tempfile::tempdir().expect("cria destino");
        let archive = work.path().join("malicioso.zip");

        let file = fs::File::create(&archive).expect("cria zip");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file("../evil.txt", options)
            .expect("escreve entrada");
        writer.write_all(b"pwned").expect("escreve conteúdo");
        writer.finish().expect("finaliza zip");

        let error = extract(&archive, dest.path()).expect_err("zip-slip deve falhar");
        assert!(
            matches!(error, super::CoreError::Archive { .. }),
            "erro inesperado: {error:?}"
        );
        assert!(
            !work.path().join("evil.txt").exists(),
            "nada pode escapar do destino"
        );
    }

    #[test]
    fn missing_source_is_an_error() {
        let work = tempfile::tempdir().expect("cria diretório temporário");
        let missing = work.path().join("nao-existe.txt");
        let archive = work.path().join("saida.zip");
        assert!(create_zip(&[missing], &archive).is_err());
    }

    #[test]
    fn unsupported_extension_is_an_error() {
        let work = tempfile::tempdir().expect("cria diretório temporário");
        let archive = work.path().join("arquivo.7z");
        fs::write(&archive, b"nao importa").expect("cria arquivo");
        let dest = work.path().join("destino");
        assert!(extract(&archive, &dest).is_err(), "7z fica fora do escopo");
    }

    #[test]
    fn extract_creates_destination_when_missing() {
        let (work, _dest) = tree();
        let archive = work.path().join("saida.tar");
        create_tar(&[work.path().join("origem")], &archive).expect("cria tar");
        let novo = work.path().join("novo/destino");
        extract(&archive, &novo).expect("extrai criando destino");
        assert!(novo.join("origem/a.txt").is_file());
    }
}
