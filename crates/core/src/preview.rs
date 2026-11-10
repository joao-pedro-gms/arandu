//! Pré-visualização de arquivos (texto e imagens).
//!
//! A decisão de *tipo* é barata (extensão e, para extensões desconhecidas, um
//! sniff de UTF-8 nos primeiros [`SNIFF_BYTES`] bytes). A leitura efetiva é
//! feita por [`load_preview`], que trunca textos em [`MAX_PREVIEW_BYTES`] e
//! decodifica imagens em memória (`DynamicImage`).
//!
//! A UI é responsável por executar [`load_preview`] fora da thread de render e
//! por converter `DynamicImage` em textura.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat};
use thiserror::Error;

/// Limite padrão de leitura para pré-visualização de texto (1 MiB).
pub const MAX_PREVIEW_BYTES: usize = 1024 * 1024;

/// Quantidade de bytes usada para detectar texto em extensões desconhecidas.
pub const SNIFF_BYTES: usize = 8 * 1024;

/// Imagens maiores que este limite não são decodificadas.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Teto de pixels após decodificar (evita explosão de memória).
const MAX_IMAGE_PIXELS: u64 = 100_000_000;

/// Extensões tratadas como texto (as ~20 mais comuns em código/config).
pub const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "toml", "md", "txt", "json", "py", "js", "ts", "html", "css", "sh", "c", "cpp", "h",
    "java", "go", "yaml", "yml", "xml", "csv", "log", "lock",
];

/// Extensões tratadas como imagem.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Tipo de conteúdo detectado para um caminho.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// Arquivo de texto (com ou sem extensão conhecida).
    Text,
    /// Imagem suportada (`png`, `jpeg`, `gif`, `webp`).
    Image,
    /// Binário, diretório ou conteúdo que não sabemos exibir.
    Unsupported,
}

/// Motivo de um conteúdo não ser pré-visualizável.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// O caminho é um diretório.
    Directory,
    /// O conteúdo parece binário (NUL ou UTF-8 inválido).
    Binary,
    /// A imagem excede o limite de decodificação.
    TooLarge,
}

/// Pedido de pré-visualização.
///
/// `size_hint` permite que a UI mostre metadados (tamanho) mesmo antes de o
/// conteúdo terminar de carregar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewRequest {
    /// Caminho do arquivo.
    pub path: PathBuf,
    /// Tamanho conhecido do arquivo (0 quando indisponível).
    pub size_hint: u64,
}

impl PreviewRequest {
    /// Carrega o conteúdo com o limite padrão de [`MAX_PREVIEW_BYTES`].
    ///
    /// # Erros
    ///
    /// Propaga [`PreviewError`] em falhas de I/O ou decodificação.
    pub fn load(&self) -> Result<PreviewContent, PreviewError> {
        load_preview(&self.path, MAX_PREVIEW_BYTES)
    }
}

/// Conteúdo carregado de um arquivo.
#[derive(Debug)]
pub enum PreviewContent {
    /// Texto decodificado (possivelmente truncado).
    Text {
        /// Conteúdo em UTF-8 (bytes inválidos viram `U+FFFD`).
        text: String,
        /// Se a leitura parou em `max_bytes`.
        truncated: bool,
        /// Extensão usada para escolher a sintaxe (ex.: `rs`), se houver.
        syntax_name: Option<String>,
    },
    /// Imagem já decodificada.
    Image {
        /// Imagem decodificada (a UI converte para textura).
        image: DynamicImage,
        /// Formato original detectado pela extensão.
        format: ImageFormat,
    },
    /// Conteúdo não pré-visualizável (a UI mostra o card de metadados).
    Unsupported {
        /// Motivo da indisponibilidade.
        reason: UnsupportedReason,
    },
}

/// Erros ao carregar uma pré-visualização.
#[derive(Debug, Error)]
pub enum PreviewError {
    /// Falha ao ler o arquivo.
    #[error("erro de I/O ao ler {path}")]
    Io {
        /// Caminho lido.
        path: PathBuf,
        /// Erro original do sistema.
        #[source]
        source: std::io::Error,
    },

    /// Falha ao decodificar a imagem.
    #[error("erro ao decodificar imagem {path}")]
    ImageDecoding {
        /// Caminho decodificado.
        path: PathBuf,
        /// Erro original do decodificador.
        #[source]
        source: image::ImageError,
    },
}

/// Classifica um caminho sem carregar o conteúdo inteiro.
///
/// Extensões conhecidas vencem; extensões desconhecidas (ou ausentes) passam
/// por um sniff de UTF-8 nos primeiros [`SNIFF_BYTES`] bytes. Falhas de leitura
/// são classificadas como [`PreviewKind::Unsupported`] — [`load_preview`] é
/// quem reporta o [`PreviewError::Io`] correspondente.
pub fn classify(path: impl AsRef<Path>) -> PreviewKind {
    let path = path.as_ref();

    if let Some(kind) = kind_from_extension(path) {
        return kind;
    }

    match sniff_prefix(path) {
        Ok(bytes) if looks_like_text(&bytes) => PreviewKind::Text,
        _ => PreviewKind::Unsupported,
    }
}

/// Carrega o conteúdo de `path` para pré-visualização.
///
/// Textos são lidos até `max_bytes` (com `truncated = true` quando há mais
/// conteúdo). Imagens acima de 20 MiB retornam
/// [`PreviewContent::Unsupported`] com [`UnsupportedReason::TooLarge`].
///
/// # Erros
///
/// Retorna [`PreviewError::Io`] para falhas de leitura e
/// [`PreviewError::ImageDecoding`] quando a extensão indica imagem mas o
/// conteúdo não pode ser decodificado.
pub fn load_preview(
    path: impl AsRef<Path>,
    max_bytes: usize,
) -> Result<PreviewContent, PreviewError> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path).map_err(|source| PreviewError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    if metadata.is_dir() {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::Directory,
        });
    }

    let extension = extension_of(path);
    let kind = extension.as_deref().and_then(kind_from_extension_str);

    if kind == Some(PreviewKind::Image) {
        return load_image(path, metadata.len());
    }

    load_text(path, extension.as_deref(), kind, max_bytes)
}

fn load_image(path: &Path, size: u64) -> Result<PreviewContent, PreviewError> {
    if size > MAX_IMAGE_BYTES {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::TooLarge,
        });
    }

    let format = ImageFormat::from_path(path).map_err(|source| PreviewError::ImageDecoding {
        path: path.to_path_buf(),
        source,
    })?;
    let image = image::open(path).map_err(|source| PreviewError::ImageDecoding {
        path: path.to_path_buf(),
        source,
    })?;

    // Uma imagem compactada pequena pode expandir para centenas de MiB. O teto
    // de pixels evita alocar uma textura absurda na UI.
    if u64::from(image.width()) * u64::from(image.height()) > MAX_IMAGE_PIXELS {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::TooLarge,
        });
    }

    Ok(PreviewContent::Image { image, format })
}

fn load_text(
    path: &Path,
    extension: Option<&str>,
    kind: Option<PreviewKind>,
    max_bytes: usize,
) -> Result<PreviewContent, PreviewError> {
    let mut file = File::open(path).map_err(|source| PreviewError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let limit = max_bytes.saturating_add(1);
    let mut buffer = Vec::with_capacity(limit.min(64 * 1024));
    file.by_ref()
        .take(limit as u64)
        .read_to_end(&mut buffer)
        .map_err(|source| PreviewError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    let truncated = buffer.len() > max_bytes;
    if truncated {
        buffer.truncate(max_bytes);
    }

    let known_text = kind == Some(PreviewKind::Text);
    let sniff_len = buffer.len().min(SNIFF_BYTES);

    if !known_text && !looks_like_text(&buffer[..sniff_len]) {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::Binary,
        });
    }

    // Mesmo com extensão textual, NUL é sinal forte de binário.
    if buffer.contains(&0) {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::Binary,
        });
    }

    // Ao truncar podemos cortar um caractere UTF-8 ao meio.
    let bytes = if truncated {
        trim_to_char_boundary(&buffer)
    } else {
        &buffer[..]
    };
    let text = String::from_utf8_lossy(bytes).into_owned();

    Ok(PreviewContent::Text {
        text,
        truncated,
        syntax_name: extension.map(str::to_owned),
    })
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
}

fn kind_from_extension(path: &Path) -> Option<PreviewKind> {
    extension_of(path)
        .as_deref()
        .and_then(kind_from_extension_str)
}

fn kind_from_extension_str(extension: &str) -> Option<PreviewKind> {
    if TEXT_EXTENSIONS.contains(&extension) {
        Some(PreviewKind::Text)
    } else if IMAGE_EXTENSIONS.contains(&extension) {
        Some(PreviewKind::Image)
    } else {
        None
    }
}

fn sniff_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buffer = Vec::with_capacity(SNIFF_BYTES);
    file.by_ref()
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// Texto plausível: sem NUL e UTF-8 válido (uma sequência incompleta no fim do
/// buffer é aceita, pois o corte pode ter partido um caractere ao meio).
fn looks_like_text(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return false;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none() && error.valid_up_to() > 0,
    }
}

fn trim_to_char_boundary(bytes: &[u8]) -> &[u8] {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes,
        Err(error) if error.error_len().is_none() => &bytes[..error.valid_up_to()],
        Err(_) => bytes,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use image::{ImageFormat, Rgba, RgbaImage};
    use tempfile::TempDir;

    use super::{
        MAX_PREVIEW_BYTES, PreviewContent, PreviewError, PreviewKind, UnsupportedReason, classify,
        load_preview,
    };

    fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, bytes).expect("escreve fixture");
        path
    }

    #[test]
    fn classifies_known_text_extension() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "main.rs", b"fn main() {}\n");
        assert_eq!(classify(&path), PreviewKind::Text);
    }

    #[test]
    fn classifies_image_extension() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = dir.path().join("pixel.png");
        RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 255]))
            .save(&path)
            .expect("salva png 1x1");
        assert_eq!(classify(&path), PreviewKind::Image);
    }

    #[test]
    fn sniffs_utf8_for_unknown_extension() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "nota", "olá, mundo\n".as_bytes());

        assert_eq!(classify(&path), PreviewKind::Text);

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega texto");
        match content {
            PreviewContent::Text {
                text,
                truncated,
                syntax_name,
            } => {
                assert_eq!(text, "olá, mundo\n");
                assert!(!truncated);
                assert_eq!(syntax_name, None);
            }
            other => panic!("esperava texto, veio {other:?}"),
        }
    }

    #[test]
    fn binary_with_nul_is_unsupported() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "binario", &[0x00, 0x01, 0x02, 0xff]);

        assert_eq!(classify(&path), PreviewKind::Unsupported);

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega binário");
        assert!(matches!(
            content,
            PreviewContent::Unsupported {
                reason: UnsupportedReason::Binary
            }
        ));
    }

    #[test]
    fn truncates_text_at_limit() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "grande.txt", b"abcdefghij");

        let content = load_preview(&path, 5).expect("carrega texto");
        match content {
            PreviewContent::Text {
                text, truncated, ..
            } => {
                assert_eq!(text, "abcde");
                assert!(truncated);
            }
            other => panic!("esperava texto, veio {other:?}"),
        }
    }

    #[test]
    fn truncation_preserves_utf8_boundary() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        // "é" ocupa 2 bytes; cortar em 1 byte não pode gerar UTF-8 inválido.
        let path = write(&dir, "acento.txt", "é!".as_bytes());

        let content = load_preview(&path, 1).expect("carrega texto");
        match content {
            PreviewContent::Text {
                text, truncated, ..
            } => {
                assert_eq!(text, "");
                assert!(truncated);
            }
            other => panic!("esperava texto, veio {other:?}"),
        }
    }

    #[test]
    fn read_error_becomes_io_error() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let missing = dir.path().join("nao-existe.txt");

        let error = load_preview(&missing, MAX_PREVIEW_BYTES).expect_err("deve falhar");
        assert!(matches!(error, PreviewError::Io { .. }));
    }

    #[test]
    fn directory_is_unsupported() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let content = load_preview(dir.path(), MAX_PREVIEW_BYTES).expect("carrega diretório");
        assert!(matches!(
            content,
            PreviewContent::Unsupported {
                reason: UnsupportedReason::Directory
            }
        ));
    }

    #[test]
    fn loads_image_into_dynamic_image() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = dir.path().join("pixel.png");
        RgbaImage::from_pixel(3, 2, Rgba([0, 255, 0, 255]))
            .save(&path)
            .expect("salva png");

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega imagem");
        match content {
            PreviewContent::Image { image, format } => {
                assert_eq!(format, ImageFormat::Png);
                assert_eq!(image.width(), 3);
                assert_eq!(image.height(), 2);
            }
            other => panic!("esperava imagem, veio {other:?}"),
        }
    }

    #[test]
    fn image_extension_with_invalid_bytes_is_decoding_error() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "quebrada.png", b"isto nao e um png");

        let error = load_preview(&path, MAX_PREVIEW_BYTES).expect_err("deve falhar");
        assert!(matches!(error, PreviewError::ImageDecoding { .. }));
    }

    #[test]
    fn missing_file_classifies_as_unsupported_without_panicking() {
        let missing = Path::new("/caminho/que/nao/existe/sem/extensao");
        assert_eq!(classify(missing), PreviewKind::Unsupported);
    }
}
