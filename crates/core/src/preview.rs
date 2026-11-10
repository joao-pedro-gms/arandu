//! Pré-visualização de arquivos (texto, imagens, Markdown, CSV, JSON, hex e
//! PDF).
//!
//! A decisão de *tipo* é barata (extensão e, para extensões desconhecidas, um
//! sniff de UTF-8 nos primeiros [`SNIFF_BYTES`] bytes). A leitura efetiva é
//! feita por [`load_preview`], que trunca textos em [`MAX_PREVIEW_BYTES`],
//! decodifica imagens em memória (`DynamicImage`) e rasteriza a primeira
//! página de PDFs com `pdfrum`.
//!
//! A UI é responsável por executar [`load_preview`] fora da thread de render e
//! por converter `DynamicImage`/`RgbaImage` em textura.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use image::{DynamicImage, ImageFormat, RgbaImage};
use pdfrum::{Deadline, Document, Limits, OpenOptions, RenderOptions, VelloCpuBackend};
use thiserror::Error;

/// Limite padrão de leitura para pré-visualização de texto (1 MiB).
pub const MAX_PREVIEW_BYTES: usize = 1024 * 1024;

/// Quantidade de bytes usada para detectar texto em extensões desconhecidas.
pub const SNIFF_BYTES: usize = 8 * 1024;

/// Imagens maiores que este limite não são decodificadas.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Teto de pixels após decodificar (evita explosão de memória).
const MAX_IMAGE_PIXELS: u64 = 100_000_000;

/// PDFs maiores que este limite não são abertos (o formato é lido inteiro).
const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;

/// Teto de pixels da página rasterizada de um PDF.
const MAX_PDF_PIXELS: u64 = 40_000_000;

/// Escala do render de PDF: 108 DPI (pontos PDF são 1/72 de polegada).
const PDF_RENDER_SCALE: f64 = 108.0 / 72.0;

/// Orçamento de tempo para abrir e rasterizar a primeira página de um PDF.
const PDF_RENDER_DEADLINE: Duration = Duration::from_secs(10);

/// Extensões tratadas como texto (as ~20 mais comuns em código/config).
pub const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "toml", "txt", "py", "js", "ts", "html", "css", "sh", "c", "cpp", "h", "java", "go",
    "yaml", "yml", "xml", "log", "lock",
];

/// Extensões renderizadas como Markdown.
pub const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown"];

/// Extensões exibidas como tabela CSV.
pub const CSV_EXTENSIONS: &[&str] = &["csv", "tsv"];

/// Extensões formatadas como JSON.
pub const JSON_EXTENSIONS: &[&str] = &["json"];

/// Extensões exibidas como hex dump.
pub const HEX_EXTENSIONS: &[&str] = &["bin"];

/// Extensões tratadas como imagem.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// Extensões rasterizadas como PDF.
pub const PDF_EXTENSIONS: &[&str] = &["pdf"];

/// Tipo de conteúdo detectado para um caminho.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    /// Arquivo de texto (com ou sem extensão conhecida).
    Text,
    /// Documento Markdown (o texto bruto é mantido; a UI renderiza).
    Markdown,
    /// Tabela CSV/TSV (texto bruto; a UI desenha a grade).
    Csv,
    /// JSON válido, formatado; inválido cai em [`PreviewKind::Text`].
    Json,
    /// Hex dump de binário pequeno (`.bin` ou desconhecido não-UTF-8).
    Hex,
    /// Imagem suportada (`png`, `jpeg`, `gif`, `webp`).
    Image,
    /// Documento PDF (primeira página rasterizada).
    Pdf,
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
    /// A imagem ou o PDF excede o limite de decodificação.
    TooLarge,
    /// O PDF não pôde ser aberto ou rasterizado.
    Pdf,
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
    /// Markdown com o texto bruto preservado (a renderização é da UI).
    Markdown {
        /// Conteúdo em UTF-8 (possivelmente truncado).
        text: String,
        /// Se a leitura parou em `max_bytes`.
        truncated: bool,
    },
    /// Binário pequeno para exibir como hex dump.
    Hex {
        /// Bytes lidos (possivelmente truncados em `max_bytes`).
        bytes: Vec<u8>,
        /// Caminho de origem (identifica o cache e a textura).
        path: PathBuf,
        /// Se a leitura parou em `max_bytes`.
        truncated: bool,
    },
    /// CSV/TSV com o texto bruto (a UI desenha a tabela).
    Csv {
        /// Conteúdo em UTF-8 (possivelmente truncado).
        text: String,
        /// Se a leitura parou em `max_bytes`.
        truncated: bool,
    },
    /// JSON válido já formatado com `serde_json::to_string_pretty`.
    Json {
        /// JSON formatado (possivelmente truncado na leitura original).
        text: String,
        /// Se a leitura parou em `max_bytes`.
        truncated: bool,
    },
    /// Primeira página rasterizada de um PDF.
    Pdf {
        /// Página 1 renderizada em RGBA.
        page: RgbaImage,
        /// Total de páginas do documento.
        page_count: u32,
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

    /// Falha ao abrir ou rasterizar o PDF.
    #[error("erro ao renderizar PDF {path}")]
    Pdf {
        /// Caminho do documento.
        path: PathBuf,
        /// Erro original do `pdfrum`.
        #[source]
        source: pdfrum::Error,
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
        Ok(_) => {
            // Binário sem extensão conhecida: mostramos hex dump quando cabe
            // no limite padrão; acima disso continua indisponível.
            match std::fs::metadata(path) {
                Ok(metadata) if metadata.len() <= MAX_PREVIEW_BYTES as u64 => PreviewKind::Hex,
                _ => PreviewKind::Unsupported,
            }
        }
        Err(_) => PreviewKind::Unsupported,
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

    match kind {
        Some(PreviewKind::Image) => load_image(path, metadata.len()),
        Some(PreviewKind::Pdf) => load_pdf(path, metadata.len()),
        Some(PreviewKind::Hex) => load_hex(path, max_bytes),
        _ => load_text(path, extension.as_deref(), kind, max_bytes, metadata.len()),
    }
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
    size: u64,
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

    let known_text = matches!(
        kind,
        Some(PreviewKind::Text | PreviewKind::Markdown | PreviewKind::Csv | PreviewKind::Json)
    );
    let sniff_len = buffer.len().min(SNIFF_BYTES);
    // Extensão textual conhecida aceita bytes inválidos (viram U+FFFD), mas
    // NUL continua sendo sinal forte de binário. Sem extensão conhecida,
    // exigimos UTF-8 plausível e, caso contrário, caímos em hex.
    let binary = buffer.contains(&0) || (!known_text && !looks_like_text(&buffer[..sniff_len]));

    if binary {
        if known_text {
            return Ok(PreviewContent::Unsupported {
                reason: UnsupportedReason::Binary,
            });
        }

        // Extensão desconhecida e conteúdo não-UTF-8: hex dump quando o
        // arquivo inteiro cabe no limite pedido.
        if size > max_bytes as u64 {
            return Ok(PreviewContent::Unsupported {
                reason: UnsupportedReason::TooLarge,
            });
        }
        return Ok(PreviewContent::Hex {
            bytes: buffer,
            path: path.to_path_buf(),
            truncated,
        });
    }

    // Ao truncar podemos cortar um caractere UTF-8 ao meio.
    let bytes = if truncated {
        trim_to_char_boundary(&buffer)
    } else {
        &buffer[..]
    };
    let text = String::from_utf8_lossy(bytes).into_owned();

    match kind {
        Some(PreviewKind::Markdown) => Ok(PreviewContent::Markdown { text, truncated }),
        Some(PreviewKind::Csv) => Ok(PreviewContent::Csv { text, truncated }),
        Some(PreviewKind::Json) => Ok(match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => PreviewContent::Json {
                text: serde_json::to_string_pretty(&value).unwrap_or(text),
                truncated,
            },
            // JSON inválido é apenas texto: melhor mostrar o bruto do que um
            // card de erro.
            Err(_) => PreviewContent::Text {
                text,
                truncated,
                syntax_name: extension.map(str::to_owned),
            },
        }),
        _ => Ok(PreviewContent::Text {
            text,
            truncated,
            syntax_name: extension.map(str::to_owned),
        }),
    }
}

fn load_hex(path: &Path, max_bytes: usize) -> Result<PreviewContent, PreviewError> {
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

    Ok(PreviewContent::Hex {
        bytes: buffer,
        path: path.to_path_buf(),
        truncated,
    })
}

fn load_pdf(path: &Path, size: u64) -> Result<PreviewContent, PreviewError> {
    if size > MAX_PDF_BYTES {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::TooLarge,
        });
    }

    let limits = Limits {
        max_render_pixels: Some(MAX_PDF_PIXELS),
        deadline: Some(Deadline::after(PDF_RENDER_DEADLINE)),
        ..Limits::default()
    };
    let options = OpenOptions::builder().limits(limits).build();

    let document = Document::open_with(path, &options).map_err(|source| PreviewError::Pdf {
        path: path.to_path_buf(),
        source,
    })?;
    let page_count = document.page_count();
    if page_count == 0 {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::Pdf,
        });
    }

    let page = document.page(0).map_err(|source| PreviewError::Pdf {
        path: path.to_path_buf(),
        source,
    })?;
    let pixmap = page
        .render_with(VelloCpuBackend, &RenderOptions::scaled(PDF_RENDER_SCALE))
        .map_err(|source| PreviewError::Pdf {
            path: path.to_path_buf(),
            source,
        })?;

    let (width, height) = (pixmap.width(), pixmap.height());
    if width == 0 || height == 0 {
        return Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::Pdf,
        });
    }

    let rgba = pixmap.to_straight_rgba();
    match RgbaImage::from_raw(width, height, rgba) {
        Some(page) => Ok(PreviewContent::Pdf { page, page_count }),
        None => Ok(PreviewContent::Unsupported {
            reason: UnsupportedReason::Pdf,
        }),
    }
}

/// Formata uma linha de hex dump: offset, 16 bytes em hexa e o mesmo trecho
/// em ASCII (bytes não imprimíveis viram `.`).
///
/// A UI usa as três colunas separadamente; o helper vive aqui para que o
/// formato tenha teste unitário.
#[must_use]
pub fn hex_row(offset: usize, chunk: &[u8]) -> (String, String, String) {
    let offset = format!("{offset:08x}");
    let mut hex = String::with_capacity(47);
    for index in 0..16 {
        if index > 0 {
            hex.push(' ');
        }
        match chunk.get(index) {
            Some(byte) => hex.push_str(&format!("{byte:02x}")),
            None => hex.push_str("  "),
        }
    }
    let ascii: String = chunk
        .iter()
        .map(|&byte| {
            if (0x20..=0x7e).contains(&byte) {
                char::from(byte)
            } else {
                '.'
            }
        })
        .collect();
    (offset, hex, ascii)
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
    if MARKDOWN_EXTENSIONS.contains(&extension) {
        Some(PreviewKind::Markdown)
    } else if CSV_EXTENSIONS.contains(&extension) {
        Some(PreviewKind::Csv)
    } else if JSON_EXTENSIONS.contains(&extension) {
        Some(PreviewKind::Json)
    } else if HEX_EXTENSIONS.contains(&extension) {
        Some(PreviewKind::Hex)
    } else if PDF_EXTENSIONS.contains(&extension) {
        Some(PreviewKind::Pdf)
    } else if TEXT_EXTENSIONS.contains(&extension) {
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
        hex_row, load_preview,
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
    fn classifies_new_preview_extensions() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");

        let markdown = write(&dir, "README.md", b"# titulo\n");
        assert_eq!(classify(&markdown), PreviewKind::Markdown);

        let csv = write(&dir, "dados.csv", b"a,b\n1,2\n");
        assert_eq!(classify(&csv), PreviewKind::Csv);

        let json = write(&dir, "config.json", b"{}\n");
        assert_eq!(classify(&json), PreviewKind::Json);

        let bin = write(&dir, "firmware.bin", &[0x00, 0x01]);
        assert_eq!(classify(&bin), PreviewKind::Hex);

        let pdf = write(&dir, "doc.pdf", b"nao e um pdf");
        assert_eq!(classify(&pdf), PreviewKind::Pdf);
    }

    #[test]
    fn binary_with_nul_is_hex() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "binario", &[0x00, 0x01, 0x02, 0xff]);

        assert_eq!(classify(&path), PreviewKind::Hex);

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega binário");
        match content {
            PreviewContent::Hex {
                bytes,
                path: source,
                truncated,
            } => {
                assert_eq!(bytes, vec![0x00, 0x01, 0x02, 0xff]);
                assert_eq!(source, path);
                assert!(!truncated);
            }
            other => panic!("esperava hex, veio {other:?}"),
        }
    }

    #[test]
    fn hex_rows_show_offset_hex_and_ascii() {
        let (offset, hex, ascii) = hex_row(0x10, b"Hi\x00\xff");

        assert_eq!(offset, "00000010");
        assert_eq!(hex.trim_end(), "48 69 00 ff");
        assert_eq!(hex.len(), 47, "16 colunas com padding");
        assert_eq!(ascii, "Hi..");
    }

    #[test]
    fn markdown_keeps_raw_text() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "nota.md", b"# Titulo\n\n**forte**\n");

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega markdown");
        match content {
            PreviewContent::Markdown { text, truncated } => {
                assert_eq!(text, "# Titulo\n\n**forte**\n");
                assert!(!truncated);
            }
            other => panic!("esperava markdown, veio {other:?}"),
        }
    }

    #[test]
    fn csv_keeps_raw_text() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "dados.csv", b"nome,idade\nAna,30\n");

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega csv");
        match content {
            PreviewContent::Csv { text, truncated } => {
                assert_eq!(text, "nome,idade\nAna,30\n");
                assert!(!truncated);
            }
            other => panic!("esperava csv, veio {other:?}"),
        }
    }

    #[test]
    fn valid_json_is_pretty_printed() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "config.json", br#"{"b":2,"a":[1,true]}"#);

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega json");
        match content {
            PreviewContent::Json { text, truncated } => {
                assert_eq!(text, "{\n  \"a\": [\n    1,\n    true\n  ],\n  \"b\": 2\n}");
                assert!(!truncated);
            }
            other => panic!("esperava json, veio {other:?}"),
        }
    }

    #[test]
    fn invalid_json_falls_back_to_text() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "quebrado.json", b"{ nao e json }");

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega json inválido");
        match content {
            PreviewContent::Text {
                text, syntax_name, ..
            } => {
                assert_eq!(text, "{ nao e json }");
                assert_eq!(syntax_name.as_deref(), Some("json"));
            }
            other => panic!("esperava texto, veio {other:?}"),
        }
    }

    #[test]
    fn bin_extension_is_hex_even_for_text_bytes() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "dados.bin", b"texto");

        let content = load_preview(&path, MAX_PREVIEW_BYTES).expect("carrega bin");
        assert!(matches!(content, PreviewContent::Hex { .. }));
    }

    #[test]
    fn invalid_pdf_returns_pdf_error() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let path = write(&dir, "quebrado.pdf", b"isto nao e um pdf");

        let error = load_preview(&path, MAX_PREVIEW_BYTES).expect_err("deve falhar");
        assert!(matches!(error, PreviewError::Pdf { .. }));
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
