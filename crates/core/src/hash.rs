//! Hashes criptográficos de arquivos, lidos em streaming.
//!
//! O arquivo é lido em blocos de [`BUFFER_SIZE`] bytes, então o consumo de
//! memória é constante mesmo para arquivos grandes. Os hashes são devolvidos
//! em hexadecimal minúsculo (64 caracteres para SHA-256, 32 para MD5).

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use md5::Md5;
use sha2::{Digest, Sha256};

use crate::fs::CoreError;

/// Tamanho do bloco de leitura usado no streaming.
pub const BUFFER_SIZE: usize = 64 * 1024;

/// Calcula o SHA-256 de `path`, em hexadecimal minúsculo.
///
/// # Erros
///
/// Devolve [`CoreError`] quando o arquivo não existe ou não pode ser lido.
pub fn sha256_file(path: &Path) -> Result<String, CoreError> {
    hash_file::<Sha256>(path)
}

/// Calcula o MD5 de `path`, em hexadecimal minúsculo.
///
/// MD5 não é seguro para fins criptográficos; aqui ele existe apenas para
/// compatibilidade com checksums publicados por terceiros.
///
/// # Erros
///
/// Devolve [`CoreError`] quando o arquivo não existe ou não pode ser lido.
pub fn md5_file(path: &Path) -> Result<String, CoreError> {
    hash_file::<Md5>(path)
}

fn hash_file<D: Digest>(path: &Path) -> Result<String, CoreError> {
    let file = File::open(path).map_err(|err| CoreError::from_io(path, err))?;
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, file);
    let mut hasher = D::new();
    let mut buffer = vec![0_u8; BUFFER_SIZE];

    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| CoreError::from_io(path, err))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{md5_file, sha256_file};
    use crate::fs::CoreError;

    fn file_with(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = dir.path().join("arquivo.bin");
        fs::write(&path, contents).expect("cria arquivo");
        (dir, path)
    }

    #[test]
    fn sha256_matches_known_digest() {
        let (_dir, path) = file_with(b"abc");
        assert_eq!(
            sha256_file(&path).expect("calcula sha256"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_missing_file_is_not_found() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let error = sha256_file(&dir.path().join("nao-existe")).expect_err("arquivo não existe");
        assert!(matches!(error, CoreError::NotFound { .. }), "{error:?}");
    }

    #[test]
    fn md5_matches_known_digest() {
        let (_dir, path) = file_with(b"abc");
        assert_eq!(
            md5_file(&path).expect("calcula md5"),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }
}
