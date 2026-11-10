//! Operações de filesystem puras e reutilizáveis.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

/// Entrada de diretório exibida na listagem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Nome do item (sem o caminho).
    pub name: String,
    /// Caminho completo do item.
    pub path: PathBuf,
    /// Se o item é um diretório.
    pub is_dir: bool,
    /// Tamanho em bytes (0 quando o sistema não informa).
    pub size: u64,
    /// Última modificação, ou [`UNIX_EPOCH`] quando indisponível.
    pub modified: SystemTime,
}

/// Erros ao listar diretórios.
#[derive(Debug, Error)]
pub enum CoreError {
    /// O caminho não existe.
    #[error("diretório não encontrado: {path}")]
    NotFound {
        /// Caminho informado.
        path: PathBuf,
    },

    /// O processo não tem permissão para ler o diretório.
    #[error("permissão negada ao acessar: {path}")]
    PermissionDenied {
        /// Caminho informado.
        path: PathBuf,
    },

    /// Outro erro de I/O ao abrir o diretório.
    #[error("erro de I/O ao listar {path}")]
    Io {
        /// Caminho informado.
        path: PathBuf,
        /// Erro original do sistema.
        #[source]
        source: std::io::Error,
    },
}

impl CoreError {
    pub(crate) fn from_io(path: &Path, source: std::io::Error) -> Self {
        match source.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound {
                path: path.to_path_buf(),
            },
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied {
                path: path.to_path_buf(),
            },
            _ => Self::Io {
                path: path.to_path_buf(),
                source,
            },
        }
    }
}

/// Lista o conteúdo de um diretório.
///
/// Diretórios vêm primeiro, depois os arquivos, ambos em ordem alfabética
/// case-insensitive. Itens que não puderem ser lidos individualmente são
/// ignorados. Symlinks não são seguidos (`symlink_metadata`): um link para
/// diretório aparece como arquivo nesta etapa.
///
/// # Erros
///
/// Retorna [`CoreError`] quando o diretório em si não puder ser aberto
/// (inexistente, sem permissão, etc.).
pub fn list_dir(path: impl AsRef<Path>) -> Result<Vec<Entry>, CoreError> {
    let path = path.as_ref();
    let read_dir = fs::read_dir(path).map_err(|err| CoreError::from_io(path, err))?;

    let mut entries = Vec::new();
    for item in read_dir {
        let Ok(item) = item else {
            continue;
        };
        let item_path = item.path();
        // `symlink_metadata` não segue o link, evitando sair da árvore.
        let Ok(metadata) = fs::symlink_metadata(&item_path) else {
            continue;
        };
        let Some(name) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };

        entries.push(Entry {
            name,
            path: item_path,
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(entries)
}

/// Formata um tamanho em bytes para uma representação legível.
///
/// Usa potências de 1024 (KiB, MiB, ...), com uma casa decimal abaixo de 10
/// unidades e sem casas decimais a partir disso.
///
/// # Exemplos
///
/// ```
/// use arandu_core::fs::format_size;
///
/// assert_eq!(format_size(0), "0 B");
/// assert_eq!(format_size(1024), "1.0 KiB");
/// assert_eq!(format_size(1536), "1.5 KiB");
/// ```
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if value >= 10.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::UNIX_EPOCH;

    use super::{CoreError, format_size, list_dir};
    use tempfile::TempDir;

    fn fixture() -> TempDir {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        fs::create_dir(dir.path().join("beta_dir")).expect("cria beta_dir");
        fs::create_dir(dir.path().join("Alpha_dir")).expect("cria Alpha_dir");
        fs::write(dir.path().join("zeta.txt"), b"z").expect("cria zeta.txt");
        fs::write(dir.path().join("Alpha.txt"), b"alpha").expect("cria Alpha.txt");
        dir
    }

    #[test]
    fn lists_directories_first_then_files_case_insensitively() {
        let dir = fixture();
        let entries = list_dir(dir.path()).expect("lista diretório");

        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["Alpha_dir", "beta_dir", "Alpha.txt", "zeta.txt"]);
        assert!(entries[0].is_dir);
        assert!(!entries[2].is_dir);
    }

    #[test]
    fn fills_entry_metadata() {
        let dir = fixture();
        let entries = list_dir(dir.path()).expect("lista diretório");

        let alpha = entries
            .iter()
            .find(|entry| entry.name == "Alpha.txt")
            .expect("encontra Alpha.txt");
        assert_eq!(alpha.path, dir.path().join("Alpha.txt"));
        assert_eq!(alpha.size, 5);
        assert!(alpha.modified >= UNIX_EPOCH);
    }

    #[test]
    fn empty_directory_yields_empty_vec() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let entries = list_dir(dir.path()).expect("lista diretório vazio");
        assert!(entries.is_empty());
    }

    #[test]
    fn missing_directory_returns_error() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let missing = dir.path().join("nao-existe");

        let err = list_dir(&missing).expect_err("deve falhar");
        assert!(matches!(err, CoreError::NotFound { .. }));
    }

    #[test]
    fn formats_small_sizes_in_bytes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn formats_binary_units() {
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(10 * 1024 * 1024), "10 MiB");
    }
}
