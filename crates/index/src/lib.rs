//! Indexação e busca global do Arandu — núcleo da fase 2.
//!
//! O crate expõe três peças independentes:
//!
//! - [`Indexer`] percorre a árvore de diretórios em background (walkdir, sem
//!   seguir symlinks e ignorando entradas ocultas);
//! - [`FuzzyMatcher`] pontua matches por subsequência no estilo do fzf;
//! - [`IndexStore`] mantém o índice em memória e devolve os melhores
//!   resultados para uma consulta.
//!
//! A persistência em disco e o watcher de mudanças ficam para PRs seguintes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;
use walkdir::WalkDir;

/// Limite padrão de entradas indexadas.
///
/// Um `$HOME` grande pode ter milhões de arquivos; o teto evita que o primeiro
/// scan consuma memória e CPU sem controle.
pub const DEFAULT_MAX_ENTRIES: usize = 200_000;

/// Intervalo de entradas entre callbacks de progresso do scan.
const PROGRESS_STEP: usize = 1_024;

/// Bônus aplicado quando a consulta casa com o nome do arquivo.
///
/// O caminho completo contém o nome no final, mas um match no nome é muito
/// mais relevante para quick-open do que um match em um diretório ancestral.
const NAME_BONUS: u32 = 1_000;

/// Erros do indexador.
#[derive(Debug, Error)]
pub enum IndexError {
    /// A raiz informada não existe.
    #[error("caminho raiz inválido: {path}")]
    InvalidRoot {
        /// Caminho informado.
        path: PathBuf,
    },

    /// A raiz existe, mas não é um diretório.
    #[error("caminho raiz não é um diretório: {path}")]
    NotDirectory {
        /// Caminho informado.
        path: PathBuf,
    },

    /// Falha de I/O ao inspecionar a raiz.
    #[error("falha ao inspecionar {path}")]
    Io {
        /// Caminho informado.
        path: PathBuf,
        /// Erro original do sistema.
        #[source]
        source: std::io::Error,
    },
}

/// Entrada do índice global.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Caminho completo.
    pub path: PathBuf,
    /// Nome do item (sem o caminho).
    pub name: String,
    /// Se o item é um diretório.
    pub is_dir: bool,
    /// Tamanho em bytes (0 quando indisponível).
    pub size: u64,
    /// Última modificação, ou [`UNIX_EPOCH`] quando indisponível.
    pub modified: SystemTime,
}

/// Indexador de disco com limite configurável.
#[derive(Debug, Clone)]
pub struct Indexer {
    max_entries: usize,
}

impl Default for Indexer {
    fn default() -> Self {
        Self::new()
    }
}

impl Indexer {
    /// Cria um indexador com o limite padrão.
    pub fn new() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }

    /// Cria um indexador com um limite explícito de entradas.
    pub fn with_limit(max_entries: usize) -> Self {
        Self { max_entries }
    }

    /// Percorre `root` e devolve as entradas encontradas.
    ///
    /// Erros de permissão em subdiretórios são ignorados silenciosamente, como
    /// manda a UX do quick-open: um diretório ilegível não deve interromper o
    /// índice inteiro. `cancel` é consultado a cada entrada.
    pub fn scan(&self, root: &Path, cancel: &AtomicBool) -> Vec<IndexEntry> {
        self.scan_result(root, cancel).unwrap_or_default()
    }

    /// Como [`Indexer::scan`], mas propaga erros da raiz.
    pub fn scan_result(
        &self,
        root: &Path,
        cancel: &AtomicBool,
    ) -> Result<Vec<IndexEntry>, IndexError> {
        let metadata = fs::metadata(root).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                IndexError::InvalidRoot {
                    path: root.to_path_buf(),
                }
            } else {
                IndexError::Io {
                    path: root.to_path_buf(),
                    source,
                }
            }
        })?;
        if !metadata.is_dir() {
            return Err(IndexError::NotDirectory {
                path: root.to_path_buf(),
            });
        }

        Ok(self.scan_with_progress(root, cancel, |_| {}))
    }

    /// Percorre `root` chamando `on_progress` a cada lote de entradas.
    ///
    /// O callback recebe o total acumulado e é útil para a UI mostrar
    /// "indexando…" com contagem enquanto o scan roda em background.
    pub fn scan_with_progress(
        &self,
        root: &Path,
        cancel: &AtomicBool,
        mut on_progress: impl FnMut(usize),
    ) -> Vec<IndexEntry> {
        let mut entries = Vec::new();
        on_progress(0);

        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_hidden(entry.file_name()));

        for item in walker {
            if cancel.load(Ordering::Relaxed) || entries.len() >= self.max_entries {
                break;
            }
            let Ok(item) = item else {
                // Erros de permissão/IO em subárvores são silenciosos.
                continue;
            };
            if item.depth() == 0 {
                continue;
            }

            let path = item.path();
            let Some(name) = item.file_name().to_str().map(str::to_owned) else {
                continue;
            };

            // `symlink_metadata` não segue o link, mantendo o scan dentro da
            // árvore mesmo quando o link aponta para fora.
            let Ok(metadata) = fs::symlink_metadata(path) else {
                continue;
            };

            entries.push(IndexEntry {
                path: path.to_path_buf(),
                name,
                is_dir: item.file_type().is_dir(),
                size: metadata.len(),
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            });

            if entries.len() % PROGRESS_STEP == 0 {
                on_progress(entries.len());
            }
        }

        on_progress(entries.len());
        entries
    }
}

/// Nome oculto no estilo Unix (começa com `.`).
fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// Matcher fuzzy determinístico, sem cache.
#[derive(Debug, Clone)]
pub struct FuzzyMatcher {
    name_bonus: u32,
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FuzzyMatcher {
    /// Matcher com o bônus de nome padrão.
    pub fn new() -> Self {
        Self {
            name_bonus: NAME_BONUS,
        }
    }

    /// Matcher com bônus de nome customizado (útil em testes).
    pub fn with_name_bonus(name_bonus: u32) -> Self {
        Self { name_bonus }
    }

    /// Pontua `query` contra a entrada, ou `None` se não houver subsequência.
    ///
    /// A consulta é case-insensitive. O match no nome do arquivo ganha um
    /// bônus fixo sobre o match no caminho completo; dentro de um texto, ganha
    /// mais pontos quem tem caracteres consecutivos e início de palavra
    /// (início do texto ou após `/`, `-`, `_` e `.`).
    pub fn matches(&self, query: &str, entry: &IndexEntry) -> Option<u32> {
        if query.is_empty() {
            return Some(0);
        }

        let name = score_text(query, &entry.name);
        let path = score_text(query, &entry.path.to_string_lossy());
        match (name, path) {
            (Some(name_score), _) => Some(name_score.saturating_add(self.name_bonus)),
            (None, Some(path_score)) => Some(path_score),
            (None, None) => None,
        }
    }
}

/// Pontua uma subsequência `query` dentro de `text`.
fn score_text(query: &str, text: &str) -> Option<u32> {
    let query: Vec<char> = query.to_lowercase().chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    let chars: Vec<char> = text.to_lowercase().chars().collect();

    let mut score = 0u32;
    let mut next = 0usize;
    let mut previous_match: Option<usize> = None;

    for (index, &character) in chars.iter().enumerate() {
        if next >= query.len() {
            break;
        }
        if character != query[next] {
            continue;
        }

        score = score.saturating_add(16);
        if previous_match.is_some_and(|previous| previous + 1 == index) {
            score = score.saturating_add(20);
        }
        let at_word_start =
            index == 0 || matches!(chars[index - 1], '/' | '\\' | '-' | '_' | '.' | ' ' | '\t');
        if at_word_start {
            score = score.saturating_add(12);
        }

        previous_match = Some(index);
        next += 1;
    }

    (next == query.len()).then_some(score)
}

/// Índice em memória compartilhável entre a UI e threads de background.
#[derive(Clone, Default)]
pub struct IndexStore {
    entries: Arc<Mutex<Vec<IndexEntry>>>,
}

impl std::fmt::Debug for IndexStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IndexStore")
            .field("len", &self.len())
            .finish()
    }
}

impl IndexStore {
    /// Cria um índice vazio.
    pub fn new() -> Self {
        Self::default()
    }

    /// Substitui todo o conteúdo do índice.
    pub fn update(&self, entries: Vec<IndexEntry>) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = entries;
    }

    /// Quantidade de entradas indexadas.
    pub fn len(&self) -> usize {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.len()
    }

    /// Se o índice está vazio.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Devolve os `limit` melhores resultados para `query`.
    ///
    /// O resultado é ordenado por score decrescente; empates são desempatados
    /// pelo caminho para manter a lista determinística. As entradas são
    /// clonadas porque o índice pode ser atualizado por outra thread.
    pub fn search(&self, query: &str, limit: usize) -> Vec<(u32, IndexEntry)> {
        if limit == 0 {
            return Vec::new();
        }

        let matcher = FuzzyMatcher::new();
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut scored: Vec<(u32, &IndexEntry)> = guard
            .iter()
            .filter_map(|entry| matcher.matches(query, entry).map(|score| (score, entry)))
            .collect();

        scored.sort_by(|(score_a, entry_a), (score_b, entry_b)| {
            score_b
                .cmp(score_a)
                .then_with(|| entry_a.path.cmp(&entry_b.path))
        });
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|(score, entry)| (score, entry.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::{Duration, UNIX_EPOCH};

    fn entry(name: &str, path: &str) -> IndexEntry {
        IndexEntry {
            path: PathBuf::from(path),
            name: name.to_owned(),
            is_dir: false,
            size: 0,
            modified: UNIX_EPOCH + Duration::from_secs(1),
        }
    }

    fn scan_all(dir: &Path) -> Vec<IndexEntry> {
        Indexer::new().scan(dir, &AtomicBool::new(false))
    }

    #[test]
    fn scan_finds_files_and_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("arquivo.txt"), b"x").expect("arquivo");
        fs::create_dir(dir.path().join("pasta")).expect("pasta");
        fs::write(dir.path().join("pasta").join("neto.md"), b"y").expect("neto");

        let entries = scan_all(dir.path());
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();

        assert!(names.contains(&"arquivo.txt"));
        assert!(names.contains(&"pasta"));
        assert!(names.contains(&"neto.md"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "pasta" && entry.is_dir)
        );
    }

    #[test]
    fn scan_skips_hidden_files_and_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("visivel.txt"), b"x").expect("visivel");
        fs::write(dir.path().join(".oculto.txt"), b"x").expect("oculto");
        fs::create_dir(dir.path().join(".git")).expect("dir oculto");
        fs::write(dir.path().join(".git").join("config"), b"x").expect("config");

        let names: Vec<String> = scan_all(dir.path())
            .into_iter()
            .map(|entry| entry.name)
            .collect();

        assert_eq!(names, vec!["visivel.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn scan_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir externo");
        fs::write(outside.path().join("externo.txt"), b"x").expect("externo");
        symlink(outside.path(), dir.path().join("link")).expect("symlink");

        let entries = scan_all(dir.path());
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "link" && !entry.is_dir)
        );
        assert!(!entries.iter().any(|entry| entry.name == "externo.txt"));
    }

    #[test]
    fn scan_respects_entry_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        for index in 0..10 {
            fs::write(dir.path().join(format!("arquivo-{index}.txt")), b"x").expect("arquivo");
        }

        let entries = Indexer::with_limit(3).scan(dir.path(), &AtomicBool::new(false));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn scan_stops_when_cancelled() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("arquivo.txt"), b"x").expect("arquivo");
        let cancel = AtomicBool::new(true);

        assert!(Indexer::new().scan(dir.path(), &cancel).is_empty());
    }

    #[test]
    fn scan_invalid_root_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nao-existe");
        let cancel = AtomicBool::new(false);

        assert!(matches!(
            Indexer::new().scan_result(&missing, &cancel),
            Err(IndexError::InvalidRoot { .. })
        ));
        assert!(Indexer::new().scan(&missing, &cancel).is_empty());
    }

    #[test]
    fn consecutive_matches_score_higher_than_dispersed() {
        let matcher = FuzzyMatcher::new();
        let consecutive = entry("abc.txt", "/tmp/abc.txt");
        let dispersed = entry("a-b-c.txt", "/tmp/a-b-c.txt");

        let consecutive_score = matcher.matches("abc", &consecutive).expect("match");
        let dispersed_score = matcher.matches("abc", &dispersed).expect("match");

        assert!(consecutive_score > dispersed_score);
    }

    #[test]
    fn fuzzy_match_is_case_insensitive() {
        let matcher = FuzzyMatcher::new();
        let candidate = entry("README.md", "/tmp/README.md");

        assert!(matcher.matches("readme", &candidate).is_some());
        assert!(matcher.matches("ReAdMe", &candidate).is_some());
    }

    #[test]
    fn fuzzy_match_requires_subsequence() {
        let matcher = FuzzyMatcher::new();
        let candidate = entry("arquivo.txt", "/tmp/arquivo.txt");

        assert!(matcher.matches("arq", &candidate).is_some());
        assert!(matcher.matches("aqr", &candidate).is_none());
    }

    #[test]
    fn name_match_outweighs_path_match() {
        let matcher = FuzzyMatcher::new();
        let by_name = entry("relatorio.txt", "/tmp/diversos/relatorio.txt");
        let by_path = entry("x.txt", "/tmp/relatorio/x.txt");

        let name_score = matcher.matches("relatorio", &by_name).expect("nome");
        let path_score = matcher.matches("relatorio", &by_path).expect("caminho");

        assert!(name_score > path_score);
    }

    #[test]
    fn search_returns_top_results_ordered_and_limited() {
        let store = IndexStore::new();
        store.update(vec![
            entry("a-b-c.txt", "/tmp/a-b-c.txt"),
            entry("abc.txt", "/tmp/abc.txt"),
            entry("outro.txt", "/tmp/outro.txt"),
        ]);

        let results = store.search("abc", 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].1.name, "abc.txt");
        assert!(results[0].0 >= results[1].0);

        assert!(store.search("abc", 0).is_empty());
    }

    #[test]
    fn update_replaces_previous_entries() {
        let store = IndexStore::new();
        store.update(vec![
            entry("antigo.txt", "/tmp/antigo.txt"),
            entry("velho.txt", "/tmp/velho.txt"),
        ]);
        assert_eq!(store.len(), 2);

        store.update(vec![entry("novo.txt", "/tmp/novo.txt")]);
        assert_eq!(store.len(), 1);
        assert!(store.search("antigo", 10).is_empty());
        assert_eq!(store.search("novo", 10)[0].1.name, "novo.txt");
    }

    #[test]
    fn empty_query_returns_entries() {
        let store = IndexStore::new();
        store.update(vec![entry("a.txt", "/tmp/a.txt")]);

        let results = store.search("", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 0);
    }
}
