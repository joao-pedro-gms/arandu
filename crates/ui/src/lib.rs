//! Interface gráfica do Arandu, construída com egui/eframe.
//!
//! Os textos são carregados dos catálogos em `locales/` via `rust-i18n`, que
//! suporta TOML nativamente — por isso mantemos os catálogos em TOML, o mesmo
//! formato usado pela configuração da aplicação.
//!
//! # Navegação dual pane
//!
//! A tela segue `docs/spec-dual-pane.md`: dois painéis lado a lado, splitter
//! arrastável, breadcrumb por painel e status bar. Todas as cores vêm de
//! `ui.visuals()` (o accent configurável é aplicado uma única vez no setup),
//! para que temas claros/escuros e o TOML de tema continuem funcionando.

rust_i18n::i18n!("locales", fallback = "en");

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arandu_config::{
    Action, Config, KeyBinding, Keymap, Modifiers as KeyModifiers, TagsStore, Theme, extension_key,
};
use arandu_core::archive::{self, ArchiveFormat};
use arandu_core::batch::apply_pattern;
use arandu_core::fs::{CoreError, Entry, format_size, list_dir};
use arandu_core::hash::{md5_file, sha256_file};
use arandu_core::ops::{self, Op, OpKind, OpsError, Progress};
use arandu_core::preview::{
    PreviewContent, PreviewError, PreviewRequest, UnsupportedReason, hex_row,
};
use arandu_core::queue::{OpQueue, QueueEvent, UndoStack};
use arandu_git::{
    BlameLine, BranchInfo, CommitInfo, DEFAULT_BLAME_LINES, DEFAULT_LOG_LIMIT, FileStatus,
    GitStatus, Repo, StashInfo, render_graph,
};
use arandu_index::{IndexEntry, IndexStore, Indexer};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, Label, Layout, Modifiers, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, TextFormat, TextStyle, Ui, UiBuilder, pos2, vec2,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// Nome exibido da aplicação.
pub const APP_NAME: &str = "Arandu";

/// Altura de cada linha da lista (spec §2).
const ROW_H: f32 = 22.0;
/// Altura do header de cada painel.
const HEADER_H: f32 = 28.0;
/// Altura do cabeçalho de colunas.
const COLUMN_HEADER_H: f32 = 22.0;
/// Altura da toolbar superior.
const TOOLBAR_H: f32 = 32.0;
/// Altura da status bar.
const STATUS_H: f32 = 24.0;
/// Altura da barra de operações (acima da status bar).
const OPS_H: f32 = 28.0;
/// Área de hit do splitter (linha desenhada tem 1pt).
const SPLITTER_HIT: f32 = 6.0;
/// Largura mínima de cada painel.
const MIN_PANEL_W: f32 = 240.0;
/// Largura da coluna de tamanho.
const SIZE_COL_W: f32 = 72.0;
/// Largura da coluna de modificação.
const MODIFIED_COL_W: f32 = 140.0;
/// Cor de fundo da seleção: accent a ~25% de alpha.
const SELECTION_ALPHA: u8 = 64;
/// Fração inicial da área útil ocupada pelo painel de preview.
const PREVIEW_FRACTION: f32 = 0.4;
/// Largura mínima do painel de preview.
const PREVIEW_MIN_W: f32 = 200.0;
/// Fração máxima da área útil ocupada pelo painel de preview.
const PREVIEW_MAX_FRACTION: f32 = 0.6;
/// Entradas mantidas no cache de preview (LRU simples).
const PREVIEW_CACHE_CAP: usize = 20;
/// Fração inicial da área útil ocupada pelo painel Git.
const GIT_FRACTION: f32 = 0.34;
/// Largura mínima do painel Git.
const GIT_MIN_W: f32 = 220.0;
/// Fração máxima da área útil ocupada pelo painel Git.
const GIT_MAX_FRACTION: f32 = 0.5;
/// Intervalo do refresh automático do status Git.
const GIT_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// Altura reservada ao formulário de commit no painel Git.
const GIT_COMMIT_H: f32 = 96.0;
/// Bytes por linha no hex dump.
const HEX_ROW_BYTES: usize = 16;
/// Máximo de linhas de dados exibidas na tabela CSV.
const CSV_MAX_ROWS: usize = 1000;
/// Teto de linhas renderizadas com syntax highlight.
///
/// Um arquivo de 1 MiB pode ter dezenas de milhares de linhas; construir e
/// dispor um `LayoutJob` completo a cada frame travaria a UI. O texto inteiro
/// continua disponível no cache, apenas a renderização é limitada.
const MAX_HIGHLIGHT_LINES: usize = 2000;
/// Largura de uma célula no modo ícones.
const CELL_W: f32 = 96.0;
/// Altura de uma linha de células no modo ícones.
const GRID_ROW_H: f32 = 100.0;
/// Espaçamento horizontal entre células no modo ícones.
const CELL_GAP: f32 = 8.0;
/// Tamanho do glifo do ícone no modo ícones.
const ICON_SIZE: f32 = 28.0;
/// Altura reservada para o nome (duas linhas) no modo ícones.
const GRID_NAME_H: f32 = 40.0;
/// Máximo de resultados exibidos no quick-open (Ctrl+P).
const QUICK_OPEN_MAX_RESULTS: usize = 20;
/// Altura de uma linha de resultado no quick-open.
const QUICK_OPEN_ROW_H: f32 = 22.0;
/// Intervalo mínimo entre rescans automáticos do índice ao reabrir.
const QUICK_OPEN_REFRESH: Duration = Duration::from_secs(300);

/// Ações listadas na status bar, na ordem em que aparecem (as principais).
const STATUS_HINTS: [Action; 7] = [
    Action::TogglePreview,
    Action::ToggleViewMode,
    Action::Copy,
    Action::Move,
    Action::Rename,
    Action::Delete,
    Action::Undo,
];

/// Painel esquerdo ou direito.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PanelSide {
    Left,
    Right,
}

impl PanelSide {
    fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// Ordenação visual da lista (a interação chega em etapa posterior).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    /// Nome ascendente, diretórios primeiro.
    #[default]
    NameAscending,
}

/// Modo de visualização de um painel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// Lista detalhada (Nome/Tamanho/Modificado).
    #[default]
    List,
    /// Grade de ícones.
    Icons,
}

impl ViewMode {
    /// Nome curto do modo, traduzido (exibido na toolbar).
    fn label(self) -> String {
        match self {
            Self::List => rust_i18n::t!("view_mode_list").into_owned(),
            Self::Icons => rust_i18n::t!("view_mode_icons").into_owned(),
        }
    }

    fn toggled(self) -> Self {
        match self {
            Self::List => Self::Icons,
            Self::Icons => Self::List,
        }
    }
}

/// Estado de um painel.
pub struct PanelState {
    /// Diretório atual do painel.
    pub cwd: PathBuf,
    /// Entradas do diretório atual (já ordenadas por `list_dir`).
    pub entries: Vec<Entry>,
    /// Índice da seleção primária.
    pub selected: Option<usize>,
    /// Índice do item focado pelo teclado.
    pub focus: usize,
    /// Ordenação visual (sem interação nesta etapa).
    pub sort: SortOrder,
    /// Modo de visualização do painel.
    pub view_mode: ViewMode,
    /// Colunas da grade no último frame (usado pela navegação por teclado).
    grid_cols: usize,

    /// Conjunto de itens selecionados (inclui `selected`).
    multi: BTreeSet<usize>,
    /// Erro ao abrir o diretório atual.
    error: Option<CoreError>,
    /// Último offset de scroll conhecido.
    scroll_offset: f32,
    /// Altura visível da lista no último frame.
    viewport_h: f32,
    /// Offset solicitado para o próximo frame (auto-scroll do foco).
    pending_scroll: Option<f32>,
}

impl PanelState {
    fn new(cwd: PathBuf) -> Self {
        let mut panel = Self {
            cwd,
            entries: Vec::new(),
            selected: None,
            focus: 0,
            sort: SortOrder::default(),
            view_mode: ViewMode::default(),
            grid_cols: 1,
            multi: BTreeSet::new(),
            error: None,
            scroll_offset: 0.0,
            viewport_h: 0.0,
            pending_scroll: None,
        };
        panel.reload();
        panel
    }

    /// Recarrega a listagem do diretório atual.
    fn reload(&mut self) {
        match list_dir(&self.cwd) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
                self.selected = None;
                self.multi.clear();
                if self.focus >= self.entries.len() {
                    self.focus = self.entries.len().saturating_sub(1);
                }
                self.pending_scroll = Some(0.0);
            }
            Err(err) => {
                self.entries.clear();
                self.error = Some(err);
                self.selected = None;
                self.multi.clear();
                self.focus = 0;
            }
        }
    }

    /// Navega para um diretório, recarregando a listagem.
    fn navigate_to(&mut self, path: PathBuf) {
        self.cwd = path;
        self.reload();
        self.ensure_focus_visible();
    }

    /// Sobe para o diretório pai, se existir.
    fn go_up(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            let parent = parent.to_path_buf();
            self.navigate_to(parent);
        }
    }

    /// Navega para o diretório pai de `path` e foca a entrada, se presente.
    ///
    /// Usado pelo quick-open: o item vem do índice global e pode estar em
    /// qualquer lugar da árvore.
    fn reveal_path(&mut self, path: &Path) {
        if let Some(parent) = path.parent()
            && parent != self.cwd
        {
            self.navigate_to(parent.to_path_buf());
        }
        if let Some(index) = self.entries.iter().position(|entry| entry.path == path) {
            self.select(index, false);
            self.ensure_focus_visible();
        }
    }

    /// Aplica um clique (com Ctrl = toggle) na linha `index`.
    fn select(&mut self, index: usize, ctrl: bool) {
        if index >= self.entries.len() {
            return;
        }
        if ctrl {
            if self.multi.remove(&index) {
                if self.selected == Some(index) {
                    self.selected = self.multi.iter().next().copied();
                }
            } else {
                self.multi.insert(index);
                self.selected = Some(index);
            }
        } else {
            self.multi.clear();
            self.multi.insert(index);
            self.selected = Some(index);
        }
        self.focus = index;
    }

    /// Abre a entrada `index`: diretórios são navegados, arquivos selecionados.
    fn open(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if entry.is_dir {
            let path = entry.path.clone();
            self.navigate_to(path);
        } else {
            self.select(index, false);
        }
    }

    /// Abre o item focado (Enter): diretórios navegam, arquivos são selecionados.
    ///
    /// As setas alteram apenas o foco (como no spec), então o Enter usa o foco
    /// — que acompanha a seleção quando ela vem do mouse.
    fn open_focused(&mut self) {
        self.open(self.focus);
    }

    /// Move o foco do teclado e garante que a linha fique visível.
    fn move_focus(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.focus = 0;
            return;
        }
        let last = self.entries.len() - 1;
        let target = (self.focus as isize + delta).clamp(0, last as isize) as usize;
        self.focus = target;
        self.ensure_focus_visible();
    }

    /// Move o foco uma linha inteira da grade (para cima/baixo no modo ícones).
    fn move_focus_row(&mut self, delta: isize) {
        let cols = self.grid_cols.max(1) as isize;
        self.move_focus(delta * cols);
    }

    /// Move o foco uma célula na grade (para os lados no modo ícones).
    fn move_focus_column(&mut self, delta: isize) {
        self.move_focus(delta);
    }

    /// Alterna entre lista e ícones, reposicionando o scroll para o foco.
    pub fn toggle_view_mode(&mut self) {
        self.view_mode = self.view_mode.toggled();
        self.ensure_focus_visible();
    }

    /// Pede ao próximo frame que ajuste o scroll para mostrar o foco.
    fn ensure_focus_visible(&mut self) {
        let (row_h, cols) = match self.view_mode {
            ViewMode::List => (ROW_H, 1),
            ViewMode::Icons => (GRID_ROW_H, self.grid_cols.max(1)),
        };
        let row = self.focus / cols;
        let top = row as f32 * row_h;
        let bottom = top + row_h;
        let view_h = self.viewport_h.max(row_h);
        let mut offset = self.scroll_offset;
        if top < offset {
            offset = top;
        } else if bottom > offset + view_h {
            offset = bottom - view_h;
        }
        self.pending_scroll = Some(offset.max(0.0));
    }

    /// Quantidade de itens selecionados.
    fn selected_count(&self) -> usize {
        self.multi.len()
    }

    /// Caminhos selecionados; sem seleção explícita, usa o item focado.
    fn selected_paths(&self) -> Vec<PathBuf> {
        let indices: Vec<usize> = if self.multi.is_empty() {
            if self.entries.is_empty() {
                Vec::new()
            } else {
                vec![self.focus]
            }
        } else {
            self.multi.iter().copied().collect()
        };
        indices
            .into_iter()
            .filter_map(|index| self.entries.get(index))
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// Diretório atual e listagem, para a status bar.
    fn item_count(&self) -> usize {
        self.entries.len()
    }
}

/// Operação em destaque na barra de progresso.
struct CurrentOp {
    kind: OpKind,
    name: String,
    progress: Option<Progress>,
}

/// Mensagem efêmera exibida na status bar.
struct Notice {
    text: String,
    error: bool,
}

impl Notice {
    fn info(text: String) -> Self {
        Self { text, error: false }
    }

    fn error(text: String) -> Self {
        Self { text, error: true }
    }
}

/// Estado da barra de operações.
#[derive(Default)]
struct OpsState {
    current: Option<CurrentOp>,
    notice: Option<Notice>,
}

/// Chave de cache do preview: caminho + mtime.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PreviewKey {
    path: PathBuf,
    modified: SystemTime,
}

impl PreviewKey {
    fn for_entry(entry: &Entry) -> Self {
        Self {
            path: entry.path.clone(),
            modified: entry.modified,
        }
    }
}

/// Resultado cacheado de um preview (conteúdo ou erro).
#[derive(Clone)]
enum PreviewEntry {
    Ready(Arc<PreviewContent>),
    Failed(Arc<String>),
}

/// Cache LRU simples (mapa + ordem de inserção/uso).
#[derive(Default)]
struct PreviewCache {
    map: HashMap<PreviewKey, PreviewEntry>,
    order: Vec<PreviewKey>,
}

impl PreviewCache {
    fn get(&self, key: &PreviewKey) -> Option<PreviewEntry> {
        self.map.get(key).cloned()
    }

    /// Marca a chave como usada recentemente (move para o fim da ordem).
    fn touch(&mut self, key: &PreviewKey) {
        if !self.map.contains_key(key) {
            return;
        }
        self.order.retain(|item| item != key);
        self.order.push(key.clone());
    }

    fn insert(&mut self, key: PreviewKey, entry: PreviewEntry) {
        self.touch(&key);
        self.map.insert(key, entry);
        while self.order.len() > PREVIEW_CACHE_CAP {
            let oldest = self.order.remove(0);
            self.map.remove(&oldest);
        }
    }
}

/// Estado do painel de preview (F3).
#[derive(Default)]
struct PreviewState {
    /// Painel visível (F3 alterna).
    visible: bool,
    cache: PreviewCache,
    /// Chave cuja leitura está em andamento.
    pending: Option<PreviewKey>,
    /// Canal da leitura em background (uma por vez).
    receiver: Option<Receiver<(PreviewKey, Result<PreviewContent, PreviewError>)>>,
    /// Job de highlight do preview de texto atual.
    job: Option<(PreviewKey, bool, Arc<egui::text::LayoutJob>)>,
    /// Textura da imagem/PDF atual.
    texture: Option<(PreviewKey, egui::TextureHandle)>,
    /// Cache de layout/renderização do Markdown.
    markdown_cache: egui_commonmark::CommonMarkCache,
}

/// Conjunto padrão de sintaxes (carregado uma única vez).
fn syntax_set() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Temas padrão do syntect (carregados uma única vez).
fn theme_set() -> &'static ThemeSet {
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    THEMES.get_or_init(ThemeSet::load_defaults)
}

/// Diálogo de renomeação.
struct RenameDialog {
    path: PathBuf,
    name: String,
    error: Option<String>,
}

/// Editor de tags (F4).
struct TagEditorState {
    /// Caminhos que receberão a tag digitada.
    paths: Vec<PathBuf>,
    /// Tag em digitação.
    input: String,
    /// Mensagem de validação da última tentativa.
    error: Option<String>,
}

/// Hashes calculados para um arquivo.
#[derive(Clone)]
struct FileHashes {
    sha256: String,
    md5: String,
}

/// Diálogo de checksums (F9).
///
/// A thread de cálculo é independente do diálogo: fechar com Esc apenas
/// descarta o resultado; o trabalho em background termina sozinho (o `send`
/// falha quando o canal é descartado e a thread encerra).
struct ChecksumsDialog {
    paths: Vec<PathBuf>,
    /// Resultado por arquivo, na mesma ordem de `paths`.
    results: Vec<Option<Result<FileHashes, String>>>,
    receiver: Option<Receiver<(usize, Result<FileHashes, String>)>>,
}

/// Diálogo de renomeação em lote (F7).
struct BatchRenameDialog {
    paths: Vec<PathBuf>,
    pattern: String,
}

/// Diálogo de compressão (F8).
///
/// O nome digitado decide o formato pela extensão; o arquivo é criado no
/// diretório do painel oposto, como em cópia/movimento.
struct CompressDialog {
    sources: Vec<PathBuf>,
    destination_dir: PathBuf,
    name: String,
}

/// Diálogo "Abrir com" para arquivos sem app configurado.
struct OpenWithDialog {
    /// Alvos da abertura.
    paths: Vec<PathBuf>,
    /// Comando digitado (sem shell; placeholders `{path}`/`{paths}`).
    command: String,
    /// Erro de validação do último envio.
    error: Option<String>,
}

/// Executa a abertura de arquivos em aplicativos externos.
///
/// A trait permite substituir o spawn real por um executor de teste, sem
/// lançar processos durante os testes headless.
trait OpenExecutor: Send + Sync {
    /// Abre um caminho com o aplicativo padrão do sistema operacional.
    fn open_default(&self, path: &Path) -> Result<(), String>;
    /// Executa um comando já dividido em argv (sem shell).
    fn run_command(&self, program: &str, args: &[String]) -> Result<(), String>;
}

/// Executor real: `open` para o app padrão e `Command` para comandos.
struct SystemOpenExecutor;

impl OpenExecutor for SystemOpenExecutor {
    fn open_default(&self, path: &Path) -> Result<(), String> {
        open::that(path).map_err(|error| error.to_string())
    }

    fn run_command(&self, program: &str, args: &[String]) -> Result<(), String> {
        std::process::Command::new(program)
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// Trabalho de compressão/extração em background.
///
/// A thread é independente da UI: o resultado chega pelo canal e é drenado em
/// [`AranduApp::pump_archive_jobs`]. Fechar o diálogo não cancela o trabalho.
struct ArchiveJob {
    receiver: Receiver<(PathBuf, Result<(), String>)>,
    /// Diretório a recarregar quando o trabalho terminar.
    reload_dir: PathBuf,
}

/// Resultado de uma atualização de status Git em background.
enum GitRefresh {
    /// O diretório não pertence a nenhum repositório.
    NotRepository,
    /// Status carregado com sucesso.
    Status { root: PathBuf, status: GitStatus },
    /// Falha ao consultar o repositório.
    Failed(String),
}

/// Resultado de uma consulta de log: diretório consultado + commits.
type LogRefresh = (PathBuf, Result<Vec<CommitInfo>, String>);
/// Resultado de uma consulta de blame: arquivo consultado + linhas.
type BlameRefresh = (PathBuf, Result<Vec<BlameLine>, String>);
/// Resultado de uma consulta de branches: diretório consultado + branches.
type BranchesRefresh = (PathBuf, Result<Vec<BranchInfo>, String>);
/// Resultado de uma consulta de stashes: diretório consultado + entradas.
type StashesRefresh = (PathBuf, Result<Vec<StashInfo>, String>);

/// Diálogo de criação de branch (N na aba Branches).
struct BranchDialog {
    name: String,
    error: Option<String>,
}

/// Diálogo de stash push (S na aba Stashes).
struct StashDialog {
    message: String,
    error: Option<String>,
}

/// Confirmação de uma ação destrutiva no painel Git.
#[derive(Clone)]
enum GitConfirm {
    DeleteBranch(String),
    PopStash(String),
}

/// Aba visível no painel Git.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GitTab {
    #[default]
    Status,
    Log,
    Blame,
    Branches,
    Stashes,
}

impl GitTab {
    /// Próxima aba, em ciclo (usada por Shift+Tab).
    fn next(self) -> Self {
        match self {
            Self::Status => Self::Log,
            Self::Log => Self::Blame,
            Self::Blame => Self::Branches,
            Self::Branches => Self::Stashes,
            Self::Stashes => Self::Status,
        }
    }
}

/// Estado do painel Git.
#[derive(Default)]
struct GitState {
    /// Painel visível (Ctrl+G alterna).
    visible: bool,
    /// Aba ativa (Status, Log ou Blame).
    tab: GitTab,
    /// Diretório da última consulta (para detectar troca de painel).
    requested_dir: Option<PathBuf>,
    /// Raiz do repositório descoberto.
    root: Option<PathBuf>,
    /// Último status válido.
    status: Option<GitStatus>,
    /// O diretório atual não é um repositório.
    not_repo: bool,
    /// Erro da última consulta.
    error: Option<String>,
    /// Erro da última ação (stage/unstage/commit).
    action_error: Option<String>,
    /// Consulta em andamento.
    receiver: Option<Receiver<(PathBuf, GitRefresh)>>,
    /// Commits carregados para a aba Log.
    log: Vec<CommitInfo>,
    /// Grafo ASCII correspondente a `log`.
    log_graph: Vec<String>,
    /// Diretório dos dados de log atuais (ou da requisição em voo).
    log_dir: Option<PathBuf>,
    /// Erro ao carregar o log.
    log_error: Option<String>,
    /// Requisição de log em andamento.
    log_receiver: Option<Receiver<LogRefresh>>,
    /// Linhas de blame carregadas.
    blame: Vec<BlameLine>,
    /// Arquivo dos dados de blame atuais (ou da requisição em voo).
    blame_path: Option<PathBuf>,
    /// Erro ao carregar o blame.
    blame_error: Option<String>,
    /// Requisição de blame em andamento.
    blame_receiver: Option<Receiver<BlameRefresh>>,
    /// O blame atingiu o limite de linhas.
    blame_truncated: bool,
    /// Branches carregadas para a aba Branches.
    branches: Vec<BranchInfo>,
    /// Diretório dos dados de branches atuais (ou da requisição em voo).
    branches_dir: Option<PathBuf>,
    /// Erro ao carregar as branches.
    branches_error: Option<String>,
    /// Requisição de branches em andamento.
    branches_receiver: Option<Receiver<BranchesRefresh>>,
    /// Stashes carregados para a aba Stashes.
    stashes: Vec<StashInfo>,
    /// Diretório dos dados de stashes atuais (ou da requisição em voo).
    stashes_dir: Option<PathBuf>,
    /// Erro ao carregar os stashes.
    stashes_error: Option<String>,
    /// Requisição de stashes em andamento.
    stashes_receiver: Option<Receiver<StashesRefresh>>,
    /// Diálogo de criação de branch.
    branch_dialog: Option<BranchDialog>,
    /// Diálogo de stash push.
    stash_dialog: Option<StashDialog>,
    /// Confirmação pendente (excluir branch / aplicar stash).
    confirm: Option<GitConfirm>,
    /// Aviso modal de uma ação do painel Git.
    notice: Option<String>,
    /// Mensagem de commit em edição.
    commit_message: String,
    /// Linha focada na lista (staged + modified + untracked, ou log).
    focus: usize,
    /// O teclado está focado no painel Git.
    focused: bool,
    /// O campo de mensagem está com foco (suspende atalhos).
    commit_focused: bool,
    /// Início da última consulta, para o refresh periódico.
    last_loaded: Option<Instant>,
}

/// Seção de uma linha do painel Git.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GitSection {
    Staged,
    Modified,
    Untracked,
}

/// Evento enviado pela thread de indexação.
enum IndexScanEvent {
    /// Total de entradas já percorridas.
    Progress(usize),
    /// Scan concluído com o índice completo.
    Done(Vec<IndexEntry>),
}

/// Estado do scan do índice global.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum IndexScanStatus {
    /// Nenhum scan iniciado nesta sessão.
    #[default]
    Idle,
    /// Scan em andamento em background.
    Scanning,
    /// Índice pronto para consultas.
    Ready,
}

/// Overlay de quick-open (Ctrl+P).
struct QuickOpenState {
    open: bool,
    query: String,
    focus: usize,
    results: Vec<(u32, IndexEntry)>,
    /// Última consulta usada para preencher `results`.
    last_query: String,
    /// Pede recomputação dos resultados no próximo frame.
    dirty: bool,
    store: IndexStore,
    receiver: Option<Receiver<IndexScanEvent>>,
    /// Cancelamento compartilhado com a thread de scan.
    cancel: Arc<AtomicBool>,
    status: IndexScanStatus,
    /// Entradas já percorridas no scan atual.
    scanned: usize,
    /// Fim do último scan bem-sucedido (para o refresh automático).
    last_scan: Option<Instant>,
    /// Pede foco no campo de busca no próximo frame.
    needs_focus: bool,
}

impl Default for QuickOpenState {
    fn default() -> Self {
        Self {
            open: false,
            query: String::new(),
            focus: 0,
            results: Vec::new(),
            last_query: String::new(),
            dirty: false,
            store: IndexStore::new(),
            receiver: None,
            cancel: Arc::new(AtomicBool::new(false)),
            status: IndexScanStatus::Idle,
            scanned: 0,
            last_scan: None,
            needs_focus: false,
        }
    }
}

/// Aplicação principal do Arandu.
pub struct AranduApp {
    config: Config,
    left: PanelState,
    right: PanelState,
    active: PanelSide,
    /// Fração da largura útil ocupada pelo painel esquerdo (0.0..=1.0).
    split: f32,
    initialized: bool,
    /// Fila sequencial em background.
    queue: OpQueue,
    /// Operações desfazíveis (últimas 50).
    undo: UndoStack,
    /// Estado da barra de operações e avisos.
    ops: OpsState,
    /// Exclusões aguardando confirmação.
    delete_confirm: Option<Vec<PathBuf>>,
    /// Renomeação em edição.
    rename: Option<RenameDialog>,
    /// Editor de tags aberto (F4).
    tag_editor: Option<TagEditorState>,
    /// Diálogo de checksums aberto (F9).
    checksums: Option<ChecksumsDialog>,
    /// Diálogo de renomeação em lote aberto (F7).
    batch_rename: Option<BatchRenameDialog>,
    /// Diálogo de compressão aberto (F8).
    compress: Option<CompressDialog>,
    /// Compressão em andamento (background).
    compress_job: Option<ArchiveJob>,
    /// Extração em andamento (background).
    extract_job: Option<ArchiveJob>,
    /// Diálogo "Abrir com" aberto (F11 sem app configurado).
    open_with_dialog: Option<OpenWithDialog>,
    /// Executor de abertura externa (substituível em testes).
    open_executor: Arc<dyn OpenExecutor>,
    /// Tags dos arquivos, persistidas em `tags.toml`.
    tags: TagsStore,
    /// Painel de pré-visualização (F3).
    preview: PreviewState,
    /// Painel Git (Ctrl+G).
    git: GitState,
    /// Tema resolvido (config + arquivo de tema), aplicado no primeiro frame.
    theme: Theme,
    /// Atalhos redefiníveis (preset + overrides).
    keymap: Keymap,
    /// Cheatsheet de atalhos visível (F1).
    show_hints: bool,
    /// Quick-open global (Ctrl+P).
    quick_open: QuickOpenState,
    /// Congela a drenagem da fila em testes determinísticos de progresso.
    #[cfg(test)]
    injected_ops: bool,
}

impl AranduApp {
    /// Cria a aplicação aplicando o locale da configuração.
    pub fn new(config: Config) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        Self::new_at(config, cwd)
    }

    /// Cria a aplicação com um diretório inicial explícito (útil em testes).
    pub fn new_at(config: Config, dir: impl Into<PathBuf>) -> Self {
        #[cfg(test)]
        let keymap = Keymap::default();
        #[cfg(not(test))]
        let keymap = Keymap::load().unwrap_or_else(|error| {
            eprintln!("arandu: keymap inválido, usando o preset padrão: {error}");
            Keymap::default()
        });
        #[cfg(test)]
        let tags = TagsStore::default();
        #[cfg(not(test))]
        let tags = TagsStore::load().unwrap_or_else(|error| {
            eprintln!("arandu: tags inválidas, começando vazio: {error}");
            TagsStore::default()
        });

        Self::with_state(config, dir, keymap, tags)
    }

    /// Variante determinística para testes headless (ex.: snapshots de tela).
    ///
    /// Usa o keymap e o armazenamento de tags padrão em vez de ler a
    /// configuração do usuário, mantendo a renderização reprodutível.
    #[doc(hidden)]
    pub fn new_at_isolated(config: Config, dir: impl Into<PathBuf>) -> Self {
        Self::with_state(config, dir, Keymap::default(), TagsStore::default())
    }

    fn with_state(
        config: Config,
        dir: impl Into<PathBuf>,
        keymap: Keymap,
        tags: TagsStore,
    ) -> Self {
        rust_i18n::set_locale(&config.locale);
        let dir = dir.into();
        let theme = config.resolved_theme();
        Self {
            config,
            left: PanelState::new(dir.clone()),
            right: PanelState::new(dir),
            active: PanelSide::Left,
            split: 0.5,
            initialized: false,
            queue: OpQueue::new(),
            undo: UndoStack::default(),
            ops: OpsState::default(),
            delete_confirm: None,
            rename: None,
            tag_editor: None,
            checksums: None,
            batch_rename: None,
            compress: None,
            compress_job: None,
            extract_job: None,
            open_with_dialog: None,
            open_executor: Arc::new(SystemOpenExecutor),
            tags,
            preview: PreviewState::default(),
            git: GitState::default(),
            theme,
            keymap,
            show_hints: false,
            quick_open: QuickOpenState::default(),
            #[cfg(test)]
            injected_ops: false,
        }
    }

    /// Configuração carregada na inicialização.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Desenha a aplicação inteira em `ui`.
    ///
    /// Separado de [`eframe::App::ui`] para permitir o harness headless de
    /// testes chamar a mesma renderização sem um [`eframe::Frame`].
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.initialized {
            self.apply_theme(&ctx);
            self.initialized = true;
        }

        self.pump_queue();
        self.pump_preview();
        self.pump_checksums();
        self.pump_archive_jobs();
        self.pump_git();
        self.request_git_refresh(false);
        self.pump_index_scan(&ctx);
        if self.compress_job.is_some()
            || self.extract_job.is_some()
            || self.git.receiver.is_some()
            || self.git.log_receiver.is_some()
            || self.git.blame_receiver.is_some()
            || self.git.branches_receiver.is_some()
            || self.git.stashes_receiver.is_some()
        {
            ctx.request_repaint();
        }
        self.handle_keyboard(&ctx);

        // O `Ui` entregue pelo eframe 0.37 não tem margem nem fundo: pintamos o
        // fundo do painel e dividimos a área em toolbar / conteúdo / fila /
        // status.
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, ui.visuals().panel_fill);

        let toolbar_rect = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR_H));
        let ops_h = if self.ops.current.is_some() {
            OPS_H
        } else {
            0.0
        };
        let status_rect = Rect::from_min_max(
            pos2(full.left(), full.bottom() - STATUS_H),
            pos2(full.right(), full.bottom()),
        );
        let ops_rect = Rect::from_min_max(
            pos2(full.left(), full.bottom() - STATUS_H - ops_h),
            pos2(full.right(), full.bottom() - STATUS_H),
        );
        let content = Rect::from_min_max(
            pos2(full.left(), full.top() + TOOLBAR_H),
            pos2(full.right(), full.bottom() - STATUS_H - ops_h),
        );

        self.draw_toolbar(ui, toolbar_rect);
        self.draw_content(ui, content);
        if ops_h > 0.0 {
            self.draw_ops_bar(ui, ops_rect);
        }
        self.draw_status_bar(ui, status_rect);
        self.draw_hints_overlay(&ctx);
        self.draw_quick_open(&ctx);
        self.draw_dialogs(&ctx);
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let theme = &self.theme;
        let egui_theme = if theme.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(egui_theme);

        let mut style = (*ctx.style_of(egui_theme)).clone();
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.5));

        let mut visuals = if theme.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        let color = |value: &Option<String>| value.as_deref().and_then(parse_hex_color);
        let background = color(&theme.background);
        let panel = color(&theme.panel);
        let text = color(&theme.text);
        let text_muted = color(&theme.text_muted);
        let border = color(&theme.border);

        // Fundos: a janela e as barras usam `background`; os painéis, `panel`.
        if let Some(fill) = background.or(panel) {
            visuals.window_fill = fill;
            visuals.faint_bg_color = fill;
        }
        if let Some(fill) = panel.or(background) {
            visuals.panel_fill = fill;
        }

        if let Some(text) = text {
            visuals.override_text_color = Some(text);
            visuals.widgets.noninteractive.fg_stroke.color = text;
            visuals.widgets.inactive.fg_stroke.color = text;
        }
        if let Some(muted) = text_muted {
            visuals.weak_text_color = Some(muted);
        }

        if let Some(accent) = parse_hex_color(&theme.accent) {
            visuals.selection.bg_fill = with_alpha(accent, SELECTION_ALPHA);
            visuals.selection.stroke = Stroke::new(1.0, accent);
            visuals.hyperlink_color = accent;
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, accent);
        }

        if let Some(border) = border {
            let stroke = Stroke::new(1.0, border);
            visuals.widgets.noninteractive.bg_stroke = stroke;
            visuals.window_stroke = stroke;
        }

        // Superfícies de widgets derivadas do fundo com contraste previsível,
        // em vez de reaproveitar os cinzas fixos do egui.
        let base = panel.or(background);
        let towards = text.unwrap_or(visuals.text_color());
        if let Some(base) = base {
            let tint = |amount: f32| mix_color(base, towards, amount);
            visuals.widgets.noninteractive.bg_fill = base;
            visuals.widgets.noninteractive.weak_bg_fill = base;
            visuals.widgets.inactive.bg_fill = tint(0.14);
            visuals.widgets.inactive.weak_bg_fill = tint(0.10);
            visuals.widgets.hovered.bg_fill = tint(0.20);
            visuals.widgets.hovered.weak_bg_fill = tint(0.16);
            visuals.widgets.active.bg_fill = tint(0.28);
            visuals.widgets.active.weak_bg_fill = tint(0.24);
        }

        // O estilo é por tema no egui 0.37; um único ponto de configuração
        // concentra tema + tipografia + paleta.
        style.visuals = visuals;
        ctx.set_style_of(egui_theme, style);
    }

    /// Alterna para o próximo tema disponível e persiste a escolha.
    fn cycle_theme(&mut self, ctx: &egui::Context) {
        let available = Theme::available();
        if available.is_empty() {
            return;
        }
        let current = available
            .iter()
            .position(|theme| theme.name == self.config.theme.name)
            .map_or(0, |index| (index + 1) % available.len());
        self.config.theme = available[current].clone();
        self.theme = self.config.theme.clone();
        self.apply_theme(ctx);
        #[cfg(not(test))]
        if let Err(error) = self.config.save() {
            eprintln!("arandu: não foi possível salvar o tema: {error}");
        }
    }

    /// Alguma tecla pressionada no frame casa com os bindings da ação?
    fn action_pressed(&self, action: Action, pressed: &[KeyBinding]) -> bool {
        self.keymap
            .effective(action)
            .iter()
            .any(|binding| pressed.iter().any(|key| binding.matches(key)))
    }

    fn active_panel(&self) -> &PanelState {
        match self.active {
            PanelSide::Left => &self.left,
            PanelSide::Right => &self.right,
        }
    }

    fn active_panel_mut(&mut self) -> &mut PanelState {
        match self.active {
            PanelSide::Left => &mut self.left,
            PanelSide::Right => &mut self.right,
        }
    }

    /// Tags das entradas de um painel, resolvidas uma vez por frame.
    fn tag_map_for(&self, panel: &PanelState) -> HashMap<String, Vec<String>> {
        let mut map = HashMap::new();
        for entry in &panel.entries {
            let tags = self.tags.tags_of(&entry.path);
            if !tags.is_empty() {
                map.insert(entry.path.to_string_lossy().into_owned(), tags);
            }
        }
        map
    }

    /// Trata atalhos globais antes de desenhar os widgets, casando as teclas
    /// pressionadas no frame contra o `Keymap` efetivo.
    ///
    /// Com um diálogo de confirmação aberto, Enter/Escape continuam fixos.
    /// Com a renomeação aberta, a digitação vai para o `TextEdit`. Com o
    /// cheatsheet aberto (F1), os demais atalhos ficam suspensos; fechá-lo com
    /// F1/Escape é tratado aqui e o clique fora no próprio overlay. O diálogo
    /// de confirmação tem prioridade: o cheatsheet não abre enquanto ele
    /// estiver ativo (e os dois nunca ficam abertos ao mesmo tempo).
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        if self.delete_confirm.is_some() {
            let (enter, escape) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Enter),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            if enter {
                self.confirm_delete();
            } else if escape {
                self.delete_confirm = None;
            }
            return;
        }

        // O `TextEdit` dos diálogos de renomeação/edição de tags/"Abrir com"
        // recebe a digitação; os atalhos globais ficam suspensos.
        if self.rename.is_some() || self.tag_editor.is_some() || self.open_with_dialog.is_some() {
            return;
        }

        // Quick-open: o campo de busca captura a digitação. Setas percorrem a
        // lista, Enter ativa o item focado e Esc/Ctrl+P fecham o overlay.
        if self.quick_open.open {
            let pressed = pressed_keys(ctx);
            let toggle = self.action_pressed(Action::ToggleQuickOpen, &pressed);
            let (escape, up, down, enter) = ctx.input_mut(|input| {
                (
                    input.consume_key(Modifiers::NONE, Key::Escape),
                    input.consume_key(Modifiers::NONE, Key::ArrowUp),
                    input.consume_key(Modifiers::NONE, Key::ArrowDown),
                    input.consume_key(Modifiers::NONE, Key::Enter),
                )
            });

            if toggle || escape {
                self.quick_open.open = false;
                return;
            }
            if !self.quick_open.results.is_empty() {
                let last = self.quick_open.results.len() - 1;
                if up {
                    self.quick_open.focus = self.quick_open.focus.saturating_sub(1);
                }
                if down {
                    self.quick_open.focus = (self.quick_open.focus + 1).min(last);
                }
            }
            if enter {
                self.activate_quick_open();
            }
            return;
        }

        // Diálogos das ferramentas: Esc (e, no caso dos checksums, a própria
        // ação) fecham; o restante da digitação/atalhos fica suspenso. O
        // cálculo em background pode terminar sozinho e o resultado é
        // descartado ao fechar.
        if self.checksums.is_some() {
            let pressed = pressed_keys(ctx);
            let escape = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            if escape || self.action_pressed(Action::Checksums, &pressed) {
                self.checksums = None;
            }
            return;
        }
        if self.batch_rename.is_some() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.batch_rename = None;
            }
            return;
        }
        if self.compress.is_some() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.compress = None;
            }
            return;
        }

        // A digitação da mensagem de commit tem prioridade sobre os atalhos.
        if self.git.visible && self.git.commit_focused {
            return;
        }
        // Diálogos do painel Git tratam Esc/Enter por conta própria.
        if self.git.visible
            && (self.git.branch_dialog.is_some()
                || self.git.stash_dialog.is_some()
                || self.git.confirm.is_some()
                || self.git.notice.is_some())
        {
            return;
        }

        let pressed = pressed_keys(ctx);

        let toggle_hints = self.action_pressed(Action::ShowHints, &pressed);
        if self.show_hints {
            let escape = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            if toggle_hints || escape {
                self.show_hints = false;
            }
            return;
        }
        if toggle_hints {
            self.show_hints = true;
            return;
        }

        if self.action_pressed(Action::ToggleQuickOpen, &pressed) {
            self.open_quick_open();
            return;
        }

        let toggle_preview = self.action_pressed(Action::TogglePreview, &pressed);
        // Ctrl+Shift+G também satisfaz o binding de Ctrl+G (o matching ignora
        // Shift em letras), então o log tem precedência quando Shift está ativo.
        let toggle_git_log = pressed.iter().any(|key| {
            key.mods.shift
                && self
                    .keymap
                    .effective(Action::ToggleGitLog)
                    .iter()
                    .any(|binding| binding.matches(key))
        });
        let toggle_git = !toggle_git_log && self.action_pressed(Action::ToggleGitPanel, &pressed);
        let toggle_view_mode = self.action_pressed(Action::ToggleViewMode, &pressed);
        let toggle_panel = self.action_pressed(Action::TogglePanel, &pressed);
        let up = self.action_pressed(Action::NavigateUp, &pressed);
        let down = self.action_pressed(Action::NavigateDown, &pressed);
        let enter = self.action_pressed(Action::Enter, &pressed);
        let back = self.action_pressed(Action::GoBack, &pressed);
        let copy = self.action_pressed(Action::Copy, &pressed);
        let moving = self.action_pressed(Action::Move, &pressed);
        let rename = self.action_pressed(Action::Rename, &pressed);
        let delete = self.action_pressed(Action::Delete, &pressed);
        let undo = self.action_pressed(Action::Undo, &pressed);
        let cycle_theme = self.action_pressed(Action::CycleTheme, &pressed);
        let quit = self.action_pressed(Action::Quit, &pressed);
        let tag_files = self.action_pressed(Action::TagFiles, &pressed);
        let checksums = self.action_pressed(Action::Checksums, &pressed);
        let batch_rename = self.action_pressed(Action::BatchRename, &pressed);
        let compress = self.action_pressed(Action::Compress, &pressed);
        let extract = self.action_pressed(Action::Extract, &pressed);
        let open_with_default = self.action_pressed(Action::OpenWithDefault, &pressed);
        let open_with = self.action_pressed(Action::OpenWith, &pressed);

        if toggle_preview {
            self.preview.visible = !self.preview.visible;
        }
        if toggle_git {
            self.toggle_git_panel();
        }
        if toggle_git_log {
            self.open_git_log();
        }
        if toggle_view_mode {
            self.active_panel_mut().toggle_view_mode();
        }
        if toggle_panel {
            self.active = self.active.other();
        }

        let mut git_consumed_delete = false;
        {
            let view_mode = self.active_panel().view_mode;
            // No modo ícones, as setas horizontais percorrem a grade. Como não
            // há ações dedicadas para elas (o keymap permanece enxuto), são
            // consumidas diretamente do input neste frame.
            let (grid_left, grid_right) = if view_mode == ViewMode::Icons {
                ctx.input_mut(|input| {
                    (
                        input.consume_key(Modifiers::NONE, Key::ArrowLeft),
                        input.consume_key(Modifiers::NONE, Key::ArrowRight),
                    )
                })
            } else {
                (false, false)
            };

            let mut git_navigated = false;
            if self.git.visible && self.git.focused {
                if ctx.input_mut(|input| input.consume_key(Modifiers::SHIFT, Key::Tab)) {
                    self.git.tab = self.git.tab.next();
                    self.git.focus = 0;
                    git_navigated = true;
                }
                match self.git.tab {
                    GitTab::Status => {
                        let rows = self.git_rows();
                        if !rows.is_empty() {
                            if up {
                                self.git.focus = self.git.focus.saturating_sub(1);
                            }
                            if down {
                                self.git.focus = (self.git.focus + 1).min(rows.len() - 1);
                            }
                            if enter {
                                let (staged, path) =
                                    rows[self.git.focus.min(rows.len() - 1)].clone();
                                self.git_toggle(path, !staged);
                            }
                            git_navigated = up || down || enter;
                        }
                    }
                    GitTab::Log => {
                        if !self.git.log.is_empty() {
                            if up {
                                self.git.focus = self.git.focus.saturating_sub(1);
                            }
                            if down {
                                self.git.focus = (self.git.focus + 1).min(self.git.log.len() - 1);
                            }
                            // Enter é reservado para ações futuras sobre commits.
                            git_navigated = up || down || enter;
                        }
                    }
                    GitTab::Blame => {}
                    GitTab::Branches => {
                        // Delete é do painel Git nesta aba, nunca dos arquivos.
                        git_consumed_delete |= delete;
                        if !self.git.branches.is_empty() {
                            if up {
                                self.git.focus = self.git.focus.saturating_sub(1);
                            }
                            if down {
                                self.git.focus =
                                    (self.git.focus + 1).min(self.git.branches.len() - 1);
                            }
                            let selected = self
                                .git
                                .branches
                                .get(self.git.focus.min(self.git.branches.len() - 1))
                                .cloned();
                            if enter && let Some(branch) = &selected {
                                self.git_checkout(&branch.name);
                            }
                            if let Some(branch) = &selected {
                                if delete && !branch.current {
                                    self.git.confirm =
                                        Some(GitConfirm::DeleteBranch(branch.name.clone()));
                                    git_consumed_delete = true;
                                }
                                let new_branch = ctx
                                    .input_mut(|input| input.consume_key(Modifiers::NONE, Key::N));
                                if new_branch {
                                    self.git.branch_dialog = Some(BranchDialog {
                                        name: String::new(),
                                        error: None,
                                    });
                                }
                                git_navigated = up || down || enter || delete || new_branch;
                            }
                        }
                    }
                    GitTab::Stashes => {
                        // Delete não exclui arquivos quando a aba Stashes está ativa.
                        git_consumed_delete = delete;
                        git_navigated = delete;
                        if !self.git.stashes.is_empty() {
                            if up {
                                self.git.focus = self.git.focus.saturating_sub(1);
                            }
                            if down {
                                self.git.focus =
                                    (self.git.focus + 1).min(self.git.stashes.len() - 1);
                            }
                            let selected = self
                                .git
                                .stashes
                                .get(self.git.focus.min(self.git.stashes.len() - 1))
                                .cloned();
                            if enter && let Some(stash) = &selected {
                                self.git.confirm = Some(GitConfirm::PopStash(stash.id.clone()));
                            }
                            let stash_push =
                                ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::S));
                            if stash_push {
                                self.git.stash_dialog = Some(StashDialog {
                                    message: String::new(),
                                    error: None,
                                });
                            }
                            git_navigated = up || down || enter || stash_push || delete;
                        }
                    }
                }
            }

            if !git_navigated {
                let panel = self.active_panel_mut();
                if up {
                    if view_mode == ViewMode::Icons {
                        panel.move_focus_row(-1);
                    } else {
                        panel.move_focus(-1);
                    }
                }
                if down {
                    if view_mode == ViewMode::Icons {
                        panel.move_focus_row(1);
                    } else {
                        panel.move_focus(1);
                    }
                }
                if grid_left {
                    panel.move_focus_column(-1);
                }
                if grid_right {
                    panel.move_focus_column(1);
                }
                if enter {
                    panel.open_focused();
                }
                if back {
                    panel.go_up();
                }
            }
        }
        if copy {
            self.start_transfer(OpKind::Copy);
        }
        if moving {
            self.start_transfer(OpKind::Move);
        }
        if rename {
            self.start_rename();
        }
        if tag_files {
            self.start_tag_editor();
        }
        if checksums {
            self.start_checksums();
        }
        if batch_rename {
            self.start_batch_rename();
        }
        if compress {
            self.start_compress();
        }
        if extract {
            self.start_extract();
        }
        if open_with_default {
            self.open_with_default();
        } else if open_with {
            self.start_open_with();
        }
        if delete && !git_consumed_delete {
            self.request_delete();
        }
        if undo {
            self.undo_last();
        }
        if cycle_theme {
            self.cycle_theme(ctx);
        }
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Drena eventos da fila sem bloquear a UI.
    fn pump_queue(&mut self) {
        #[cfg(test)]
        if self.injected_ops {
            return;
        }

        while let Some(event) = self.queue.try_recv() {
            match event {
                QueueEvent::Started(op) => {
                    self.ops.current = Some(CurrentOp {
                        kind: op.kind,
                        name: op
                            .params
                            .src
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| op.params.src.display().to_string()),
                        progress: None,
                    });
                    self.ops.notice = None;
                }
                QueueEvent::Progress(progress) => {
                    if let Some(current) = &mut self.ops.current {
                        current.progress = Some(progress);
                    }
                }
                QueueEvent::Done(result) => match result.outcome {
                    Ok(completed) => {
                        if completed.kind.undoable() {
                            self.undo.push(completed);
                        }
                        self.reload_after(&result.op);
                    }
                    Err(OpsError::Cancelled) => {}
                    Err(err) => {
                        self.ops.notice = Some(Notice::error(
                            rust_i18n::t!("op_failed", error = err.to_string()).to_string(),
                        ));
                    }
                },
                QueueEvent::FinishedQueue => {
                    self.ops.current = None;
                }
                QueueEvent::Cancelled => {
                    self.ops.current = None;
                    self.ops.notice = Some(Notice::info(rust_i18n::t!("op_cancelled").to_string()));
                }
            }
        }
    }

    /// Drena o resultado da leitura de preview em background.
    fn pump_preview(&mut self) {
        let received = self
            .preview
            .receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some((key, result)) = received else {
            return;
        };
        self.preview.receiver = None;
        if self.preview.pending.as_ref() == Some(&key) {
            self.preview.pending = None;
        }
        let entry = match result {
            Ok(content) => PreviewEntry::Ready(Arc::new(content)),
            Err(error) => PreviewEntry::Failed(Arc::new(error.to_string())),
        };
        self.preview.cache.insert(key, entry);
    }

    /// Dispara (se necessário) a leitura do item focado.
    ///
    /// Diretórios não são lidos: o painel mostra apenas o card de metadados.
    fn ensure_preview(&mut self, entry: Option<&Entry>) {
        let Some(entry) = entry else {
            return;
        };
        if entry.is_dir {
            return;
        }

        let key = PreviewKey::for_entry(entry);
        if self.preview.cache.get(&key).is_some() {
            self.preview.cache.touch(&key);
            return;
        }
        if self.preview.pending.as_ref() == Some(&key) {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        self.preview.receiver = Some(receiver);
        self.preview.pending = Some(key.clone());

        let request = PreviewRequest {
            path: entry.path.clone(),
            size_hint: entry.size,
        };
        // Preview é leitura pura: não passa pela fila de operações.
        std::thread::spawn(move || {
            let result = request.load();
            let _ = sender.send((key, result));
        });
    }

    /// Recarrega os painéis cujo diretório foi afetado pela operação.
    fn reload_after(&mut self, op: &Op) {
        let mut affected = Vec::new();
        if let Some(parent) = op.params.src.parent() {
            affected.push(parent.to_path_buf());
        }
        if matches!(op.kind, OpKind::Copy | OpKind::Move)
            && let Some(dest) = &op.params.dest
        {
            affected.push(dest.clone());
        }
        for panel in [&mut self.left, &mut self.right] {
            if affected.contains(&panel.cwd) {
                panel.reload();
            }
        }
    }

    /// Diretório do painel oposto ao ativo.
    fn other_cwd(&self) -> PathBuf {
        match self.active {
            PanelSide::Left => self.right.cwd.clone(),
            PanelSide::Right => self.left.cwd.clone(),
        }
    }

    /// Enfileira cópia/movimento da seleção ativa para o painel oposto.
    fn start_transfer(&mut self, kind: OpKind) {
        let paths = self.active_panel().selected_paths();
        if paths.is_empty() {
            return;
        }
        let target = self.other_cwd();
        for path in paths {
            let _ = self
                .queue
                .enqueue(Op::new(kind, path, Some(target.clone())));
        }
        self.ops.notice = None;
    }

    /// Abre o diálogo de renomeação para o primeiro item selecionado.
    fn start_rename(&mut self) {
        let Some(path) = self.active_panel().selected_paths().into_iter().next() else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.rename = Some(RenameDialog {
            path,
            name,
            error: None,
        });
    }

    /// Abre o editor de tags para a seleção do painel ativo (ou o item focado).
    fn start_tag_editor(&mut self) {
        let paths = self.active_panel().selected_paths();
        if paths.is_empty() {
            return;
        }
        self.tag_editor = Some(TagEditorState {
            paths,
            input: String::new(),
            error: None,
        });
    }

    /// Abre o diálogo de checksums e dispara o cálculo em background.
    ///
    /// Apenas arquivos regulares entram na lista (diretórios não têm conteúdo
    /// para resumir). O cálculo roda em `std::thread` e envia cada resultado
    /// pelo canal; a UI drena sem bloquear em [`Self::pump_checksums`].
    fn start_checksums(&mut self) {
        let paths: Vec<PathBuf> = self
            .active_panel()
            .selected_paths()
            .into_iter()
            .filter(|path| path.is_file())
            .collect();
        if paths.is_empty() {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let thread_paths = paths.clone();
        std::thread::spawn(move || {
            for (index, path) in thread_paths.into_iter().enumerate() {
                let result = sha256_file(&path)
                    .and_then(|sha256| md5_file(&path).map(|md5| FileHashes { sha256, md5 }))
                    .map_err(|error| error.to_string());
                // Canal descartado = diálogo fechado; a thread termina sozinha.
                if sender.send((index, result)).is_err() {
                    break;
                }
            }
        });

        let count = paths.len();
        self.checksums = Some(ChecksumsDialog {
            paths,
            results: vec![None; count],
            receiver: Some(receiver),
        });
        self.ops.notice = None;
    }

    /// Drena os resultados do cálculo de checksums sem bloquear.
    fn pump_checksums(&mut self) {
        let Some(dialog) = self.checksums.as_mut() else {
            return;
        };
        let received: Vec<(usize, Result<FileHashes, String>)> = match &dialog.receiver {
            Some(receiver) => receiver.try_iter().collect(),
            None => return,
        };
        for (index, result) in received {
            if let Some(slot) = dialog.results.get_mut(index) {
                *slot = Some(result);
            }
        }
        if dialog.results.iter().all(Option::is_some) {
            dialog.receiver = None;
        }
    }

    /// Abre o diálogo de renomeação em lote para a seleção do painel ativo.
    ///
    /// O padrão inicial preserva o nome atual (`{name}{ext}`), então confirmar
    /// sem editar o padrão não altera nada.
    fn start_batch_rename(&mut self) {
        let paths: Vec<PathBuf> = self
            .active_panel()
            .selected_paths()
            .into_iter()
            .filter(|path| path.is_file())
            .collect();
        if paths.is_empty() {
            self.ops.notice = Some(Notice::error(
                rust_i18n::t!("batch_rename_no_selection").to_string(),
            ));
            return;
        }
        self.batch_rename = Some(BatchRenameDialog {
            paths,
            pattern: "{name}{ext}".to_owned(),
        });
        self.ops.notice = None;
    }

    /// Executa as renomeações confirmadas, agregando sucessos e falhas.
    fn apply_batch_rename(&mut self, renames: Vec<(PathBuf, String)>) {
        let mut succeeded = 0_usize;
        let mut failed = 0_usize;
        let mut touched: Vec<PathBuf> = Vec::new();

        for (path, new_name) in renames {
            let Some(parent) = path.parent() else {
                failed += 1;
                continue;
            };
            let target = parent.join(new_name.trim());
            if target == path {
                succeeded += 1;
                continue;
            }
            match std::fs::rename(&path, &target) {
                Ok(()) => {
                    succeeded += 1;
                    if !touched.iter().any(|dir| dir == parent) {
                        touched.push(parent.to_path_buf());
                    }
                }
                Err(_) => failed += 1,
            }
        }

        for dir in touched {
            self.reload_dir(&dir);
        }
        let text = rust_i18n::t!("batch_rename_done", ok = succeeded, errors = failed).to_string();
        self.ops.notice = Some(if failed == 0 {
            Notice::info(text)
        } else {
            Notice::error(text)
        });
        self.batch_rename = None;
    }

    /// Abre o diálogo de compressão para a seleção do painel ativo.
    ///
    /// O nome padrão é o do primeiro alvo com extensão `.zip`; o diretório de
    /// destino é o painel oposto (mesmo padrão de cópia/movimento).
    fn start_compress(&mut self) {
        let sources = self.active_panel().selected_paths();
        if sources.is_empty() {
            self.ops.notice = Some(Notice::error(
                rust_i18n::t!("compress_no_selection").to_string(),
            ));
            return;
        }
        let base = sources[0]
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive".to_owned());
        self.compress = Some(CompressDialog {
            sources,
            destination_dir: self.other_cwd(),
            name: format!("{base}.zip"),
        });
        self.ops.notice = None;
    }

    /// Inicia a compressão em background e fecha o diálogo.
    fn begin_compress(&mut self, dialog: CompressDialog, name: String) {
        let Some(format) = archive::detect_format(Path::new(&name)) else {
            return;
        };
        let destination = dialog.destination_dir.join(&name);
        let (sender, receiver) = mpsc::channel();
        let sources = dialog.sources;
        let thread_destination = destination.clone();
        std::thread::spawn(move || {
            let result = match format {
                ArchiveFormat::Zip => archive::create_zip(&sources, &thread_destination),
                ArchiveFormat::Tar => archive::create_tar(&sources, &thread_destination),
            }
            .map_err(|error| error.to_string());
            // Canal descartado = app encerrado; a thread termina sozinha.
            let _ = sender.send((thread_destination, result));
        });
        self.compress_job = Some(ArchiveJob {
            receiver,
            reload_dir: dialog.destination_dir,
        });
        self.ops.notice = Some(Notice::info(rust_i18n::t!("compressing").to_string()));
    }

    /// Extrai o item focado se for um arquivo suportado, em background.
    ///
    /// O destino é `<nome-sem-extensão>/` no mesmo diretório, com sufixo
    /// numérico quando já existir.
    fn start_extract(&mut self) {
        if self.extract_job.is_some() {
            return;
        }
        let (path, cwd, is_dir) = {
            let panel = self.active_panel();
            let Some(entry) = panel.entries.get(panel.focus) else {
                return;
            };
            (entry.path.clone(), panel.cwd.clone(), entry.is_dir)
        };
        if is_dir {
            self.ops.notice = Some(Notice::error(
                rust_i18n::t!("extract_not_archive").to_string(),
            ));
            return;
        }
        if archive::detect_format(&path).is_none() {
            self.ops.notice = Some(Notice::error(
                rust_i18n::t!("extract_not_archive").to_string(),
            ));
            return;
        }

        let stem = archive_stem(&path);
        let destination = ops::unique_destination(&cwd, &stem);
        let (sender, receiver) = mpsc::channel();
        let thread_archive = path.clone();
        let thread_destination = destination.clone();
        std::thread::spawn(move || {
            let result = archive::extract(&thread_archive, &thread_destination)
                .map_err(|error| error.to_string());
            // Canal descartado = app encerrado; a thread termina sozinha.
            let _ = sender.send((thread_destination, result));
        });
        self.extract_job = Some(ArchiveJob {
            receiver,
            reload_dir: cwd,
        });
        self.ops.notice = Some(Notice::info(rust_i18n::t!("extracting").to_string()));
    }

    /// Abre a seleção no app configurado ou mostra o diálogo "Abrir com".
    ///
    /// Quando a extensão do primeiro alvo tem comando em `[open_with]`, ele é
    /// executado direto. Caso contrário abre o diálogo para escolher o app
    /// padrão do sistema, reusar um comando existente ou digitar um novo.
    fn start_open_with(&mut self) {
        let paths = self.active_panel().selected_paths();
        if paths.is_empty() {
            self.ops.notice = Some(Notice::error(
                rust_i18n::t!("open_with_no_selection").to_string(),
            ));
            return;
        }
        if let Some(command) = self.config().open_with_for(&paths[0]) {
            let command = command.to_owned();
            self.execute_open_command(&command, &paths);
            return;
        }
        self.open_with_dialog = Some(OpenWithDialog {
            paths,
            command: String::new(),
            error: None,
        });
        self.ops.notice = None;
    }

    /// Abre a seleção no aplicativo padrão do sistema operacional.
    fn open_with_default(&mut self) {
        let paths = self.active_panel().selected_paths();
        if paths.is_empty() {
            self.ops.notice = Some(Notice::error(
                rust_i18n::t!("open_with_no_selection").to_string(),
            ));
            return;
        }
        self.open_paths_default(&paths);
    }

    /// Abre cada caminho com o app padrão, reportando o primeiro erro.
    fn open_paths_default(&mut self, paths: &[PathBuf]) {
        for path in paths {
            if let Err(error) = self.open_executor.open_default(path) {
                self.ops.notice = Some(Notice::error(format!(
                    "{}: {error}",
                    rust_i18n::t!("open_failed")
                )));
                return;
            }
        }
        self.ops.notice = Some(Notice::info(
            rust_i18n::t!("open_with_opened", count = paths.len()).to_string(),
        ));
    }

    /// Executa um comando de `[open_with]` sobre os caminhos dados.
    ///
    /// O comando é dividido em argv por espaços em branco e executado sem
    /// shell. `{path}` gera uma invocação por arquivo; `{paths}` expande todos
    /// os arquivos na mesma invocação.
    fn execute_open_command(&mut self, command: &str, paths: &[PathBuf]) {
        let invocations = match build_open_invocations(command, paths) {
            Ok(invocations) => invocations,
            Err(error) => {
                self.ops.notice = Some(Notice::error(error));
                return;
            }
        };
        for (program, args) in invocations {
            if let Err(error) = self.open_executor.run_command(&program, &args) {
                self.ops.notice = Some(Notice::error(format!(
                    "{}: {error}",
                    rust_i18n::t!("open_failed")
                )));
                return;
            }
        }
        self.ops.notice = Some(Notice::info(
            rust_i18n::t!("open_with_opened", count = paths.len()).to_string(),
        ));
    }

    /// Persiste o comando digitado para a extensão do primeiro alvo.
    fn remember_open_command(&mut self, path: &Path, command: &str) {
        let Some(extension) = extension_key(path) else {
            return;
        };
        self.config.open_with.insert(extension, command.to_owned());
        #[cfg(not(test))]
        if let Err(error) = self.config().save() {
            self.ops.notice = Some(Notice::error(format!(
                "{}: {error}",
                rust_i18n::t!("config_save_failed")
            )));
        }
    }

    /// Drena os trabalhos de compressão/extração sem bloquear.
    fn pump_archive_jobs(&mut self) {
        if let Some(job) = &self.compress_job
            && let Ok((destination, result)) = job.receiver.try_recv()
        {
            let reload = job.reload_dir.clone();
            self.compress_job = None;
            self.ops.notice = Some(archive_notice(&destination, result, true));
            self.reload_dir(&reload);
        }
        if let Some(job) = &self.extract_job
            && let Ok((destination, result)) = job.receiver.try_recv()
        {
            let reload = job.reload_dir.clone();
            self.extract_job = None;
            self.ops.notice = Some(archive_notice(&destination, result, false));
            self.reload_dir(&reload);
        }
    }

    /// Alterna a visibilidade do painel Git.
    fn toggle_git_panel(&mut self) {
        self.git.visible = !self.git.visible;
        self.git.focused = false;
        if self.git.visible {
            self.request_git_refresh(true);
        }
    }

    /// Diretório usado para descobrir o repositório: o do painel ativo.
    fn git_target_dir(&self) -> PathBuf {
        self.active_panel().cwd.clone()
    }

    /// Arquivo focado no painel ativo, se for um arquivo (não diretório).
    fn git_blame_target(&self) -> Option<PathBuf> {
        let panel = self.active_panel();
        let entry = panel.entries.get(panel.focus)?;
        (!entry.is_dir).then(|| entry.path.clone())
    }

    /// Abre o painel Git já na aba Log.
    fn open_git_log(&mut self) {
        self.git.visible = true;
        self.git.tab = GitTab::Log;
        self.git.focus = 0;
        self.git.focused = false;
        self.request_git_refresh(true);
    }

    /// Dispara a consulta do log se a aba Log estiver ativa e os dados forem
    /// de outro diretório.
    fn request_git_log(&mut self, force: bool) {
        if self.git.tab != GitTab::Log {
            return;
        }
        let dir = self.git_target_dir();
        if force {
            self.git.log_receiver = None;
            self.git.log_dir = None;
            self.git.log_error = None;
        }
        if self.git.log_receiver.is_some() {
            return;
        }
        if !force && self.git.log_dir.as_deref() == Some(dir.as_path()) {
            return;
        }

        self.git.log_dir = Some(dir.clone());
        let (sender, receiver) = mpsc::channel();
        self.git.log_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = match Repo::discover(&dir) {
                None => Err("not a git repository".to_string()),
                Some(repo) => repo
                    .log(DEFAULT_LOG_LIMIT)
                    .map_err(|error| error.to_string()),
            };
            let _ = sender.send((dir, result));
        });
    }

    /// Dispara o blame do arquivo focado quando a aba Blame está ativa.
    fn request_git_blame(&mut self, force: bool) {
        if self.git.tab != GitTab::Blame {
            return;
        }
        let Some(path) = self.git_blame_target() else {
            if self.git.blame_receiver.is_none() {
                self.git.blame.clear();
                self.git.blame_path = None;
                self.git.blame_error = None;
                self.git.blame_truncated = false;
            }
            return;
        };
        let dir = self.git_target_dir();
        if force {
            self.git.blame_receiver = None;
            self.git.blame_path = None;
            self.git.blame_error = None;
        }
        if self.git.blame_receiver.is_some() {
            return;
        }
        if !force && self.git.blame_path.as_deref() == Some(path.as_path()) {
            return;
        }

        self.git.blame_path = Some(path.clone());
        let (sender, receiver) = mpsc::channel();
        self.git.blame_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = match Repo::discover(&dir) {
                None => Err("not a git repository".to_string()),
                Some(repo) => repo
                    .blame_file(&path, DEFAULT_BLAME_LINES)
                    .map_err(|error| error.to_string()),
            };
            let _ = sender.send((path, result));
        });
    }

    /// Dispara a consulta de branches se a aba Branches estiver ativa.
    fn request_git_branches(&mut self, force: bool) {
        if self.git.tab != GitTab::Branches {
            return;
        }
        let dir = self.git_target_dir();
        if force {
            self.git.branches_receiver = None;
            self.git.branches_dir = None;
            self.git.branches_error = None;
        }
        if self.git.branches_receiver.is_some() {
            return;
        }
        if !force && self.git.branches_dir.as_deref() == Some(dir.as_path()) {
            return;
        }

        self.git.branches_dir = Some(dir.clone());
        let (sender, receiver) = mpsc::channel();
        self.git.branches_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = match Repo::discover(&dir) {
                None => Err("not a git repository".to_string()),
                Some(repo) => repo.branches().map_err(|error| error.to_string()),
            };
            let _ = sender.send((dir, result));
        });
    }

    /// Dispara a consulta de stashes se a aba Stashes estiver ativa.
    fn request_git_stashes(&mut self, force: bool) {
        if self.git.tab != GitTab::Stashes {
            return;
        }
        let dir = self.git_target_dir();
        if force {
            self.git.stashes_receiver = None;
            self.git.stashes_dir = None;
            self.git.stashes_error = None;
        }
        if self.git.stashes_receiver.is_some() {
            return;
        }
        if !force && self.git.stashes_dir.as_deref() == Some(dir.as_path()) {
            return;
        }

        self.git.stashes_dir = Some(dir.clone());
        let (sender, receiver) = mpsc::channel();
        self.git.stashes_receiver = Some(receiver);
        std::thread::spawn(move || {
            let result = match Repo::discover(&dir) {
                None => Err("not a git repository".to_string()),
                Some(repo) => repo.stashes().map_err(|error| error.to_string()),
            };
            let _ = sender.send((dir, result));
        });
    }

    /// Dispara (se necessário) uma consulta de status em background.
    ///
    /// `force` ignora o intervalo de 10 s e reinicia uma consulta em voo —
    /// usado após abrir o painel e depois de stage/unstage/commit.
    fn request_git_refresh(&mut self, force: bool) {
        if !self.git.visible {
            return;
        }
        self.request_git_log(force);
        self.request_git_blame(force);
        self.request_git_branches(force);
        self.request_git_stashes(force);
        let dir = self.git_target_dir();
        if force {
            // A thread antiga termina sozinha; o `send` falha no canal órfão.
            self.git.receiver = None;
        } else if self.git.receiver.is_some() {
            return;
        }

        let dir_changed = self.git.requested_dir.as_deref() != Some(dir.as_path());
        let due = self
            .git
            .last_loaded
            .is_none_or(|loaded| loaded.elapsed() >= GIT_REFRESH_INTERVAL);
        if !force && !dir_changed && !due {
            return;
        }

        self.git.requested_dir = Some(dir.clone());
        self.git.last_loaded = Some(Instant::now());
        let (sender, receiver) = mpsc::channel();
        self.git.receiver = Some(receiver);
        std::thread::spawn(move || {
            let refresh = match Repo::discover(&dir) {
                None => GitRefresh::NotRepository,
                Some(repo) => match repo.status() {
                    Ok(status) => GitRefresh::Status {
                        root: repo.root().to_path_buf(),
                        status,
                    },
                    Err(error) => GitRefresh::Failed(error.to_string()),
                },
            };
            let _ = sender.send((dir, refresh));
        });
    }

    /// Drena o resultado da consulta de status sem bloquear a UI.
    fn pump_git(&mut self) {
        self.pump_git_log();
        self.pump_git_blame();
        self.pump_git_branches();
        self.pump_git_stashes();
        let Some(receiver) = self.git.receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.git.receiver = None;
                return;
            }
        };
        self.git.receiver = None;
        let (dir, refresh) = result;
        // Descarta resultados de um diretório que não é mais o alvo.
        if self.git.requested_dir.as_deref() != Some(dir.as_path()) {
            return;
        }
        match refresh {
            GitRefresh::NotRepository => {
                self.git.root = None;
                self.git.status = None;
                self.git.not_repo = true;
                self.git.error = None;
                self.git.action_error = None;
            }
            GitRefresh::Status { root, status } => {
                let rows = status.staged.len() + status.modified.len() + status.untracked.len();
                self.git.root = Some(root);
                self.git.status = Some(status);
                self.git.not_repo = false;
                self.git.error = None;
                self.git.focus = self.git.focus.min(rows.saturating_sub(1));
            }
            GitRefresh::Failed(error) => {
                self.git.error = Some(error);
                self.git.not_repo = false;
            }
        }
    }

    /// Drena o resultado do log em background.
    fn pump_git_log(&mut self) {
        let Some(receiver) = self.git.log_receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.git.log_receiver = None;
                return;
            }
        };
        self.git.log_receiver = None;
        let (dir, result) = result;
        if self.git_target_dir() != dir {
            return;
        }
        self.git.log_dir = Some(dir);
        match result {
            Ok(commits) => {
                self.git.log_graph = render_graph(&commits);
                self.git.log = commits;
                self.git.log_error = None;
                self.git.focus = self.git.focus.min(self.git.log.len().saturating_sub(1));
            }
            Err(error) => {
                self.git.log.clear();
                self.git.log_graph.clear();
                self.git.log_error = Some(error);
            }
        }
    }

    /// Drena o resultado do blame em background.
    fn pump_git_blame(&mut self) {
        let Some(receiver) = self.git.blame_receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.git.blame_receiver = None;
                return;
            }
        };
        self.git.blame_receiver = None;
        let (path, result) = result;
        // Descarta resultados de um arquivo que já não está focado.
        if self.git_blame_target().as_deref() != Some(path.as_path()) {
            return;
        }
        self.git.blame_path = Some(path);
        match result {
            Ok(lines) => {
                self.git.blame_truncated = lines.len() >= DEFAULT_BLAME_LINES;
                self.git.blame = lines;
                self.git.blame_error = None;
            }
            Err(error) => {
                self.git.blame.clear();
                self.git.blame_truncated = false;
                self.git.blame_error = Some(error);
            }
        }
    }

    /// Drena o resultado da consulta de branches em background.
    fn pump_git_branches(&mut self) {
        let Some(receiver) = self.git.branches_receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.git.branches_receiver = None;
                return;
            }
        };
        self.git.branches_receiver = None;
        let (dir, result) = result;
        if self.git_target_dir() != dir {
            return;
        }
        self.git.branches_dir = Some(dir);
        match result {
            Ok(branches) => {
                self.git.focus = self.git.focus.min(branches.len().saturating_sub(1));
                self.git.branches = branches;
                self.git.branches_error = None;
            }
            Err(error) => {
                self.git.branches.clear();
                self.git.branches_error = Some(error);
            }
        }
    }

    /// Drena o resultado da consulta de stashes em background.
    fn pump_git_stashes(&mut self) {
        let Some(receiver) = self.git.stashes_receiver.as_ref() else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.git.stashes_receiver = None;
                return;
            }
        };
        self.git.stashes_receiver = None;
        let (dir, result) = result;
        if self.git_target_dir() != dir {
            return;
        }
        self.git.stashes_dir = Some(dir);
        match result {
            Ok(stashes) => {
                self.git.focus = self.git.focus.min(stashes.len().saturating_sub(1));
                self.git.stashes = stashes;
                self.git.stashes_error = None;
            }
            Err(error) => {
                self.git.stashes.clear();
                self.git.stashes_error = Some(error);
            }
        }
    }

    /// Linhas do painel Git na ordem exibida: `(staged?, caminho absoluto)`.
    fn git_rows(&self) -> Vec<(bool, PathBuf)> {
        let (Some(status), Some(root)) = (&self.git.status, &self.git.root) else {
            return Vec::new();
        };
        status
            .staged
            .iter()
            .map(|entry| (true, root.join(&entry.path)))
            .chain(
                status
                    .modified
                    .iter()
                    .map(|entry| (false, root.join(&entry.path))),
            )
            .chain(
                status
                    .untracked
                    .iter()
                    .map(|entry| (false, root.join(&entry.path))),
            )
            .collect()
    }

    /// Executa stage (`stage = true`) ou unstage de um caminho e atualiza.
    fn git_toggle(&mut self, path: PathBuf, stage: bool) {
        let dir = self.git_target_dir();
        let Some(repo) = Repo::discover(&dir) else {
            self.git.not_repo = true;
            self.git.status = None;
            return;
        };
        let paths = [path];
        let outcome = if stage {
            repo.stage(&paths)
        } else {
            repo.unstage(&paths)
        };
        self.git.action_error = outcome.err().map(|error| error.to_string());
        self.request_git_refresh(true);
    }

    /// Cria um commit com a mensagem digitada e o index atual.
    fn git_commit(&mut self) {
        let message = self.git.commit_message.trim().to_owned();
        if message.is_empty() {
            return;
        }
        let dir = self.git_target_dir();
        let Some(repo) = Repo::discover(&dir) else {
            self.git.not_repo = true;
            self.git.status = None;
            return;
        };
        match repo.commit(&message) {
            Ok(_) => {
                self.git.commit_message.clear();
                self.git.action_error = None;
                self.request_git_refresh(true);
            }
            Err(error) => self.git.action_error = Some(error.to_string()),
        }
    }

    /// Faz checkout da branch selecionada e recarrega o painel.
    fn git_checkout(&mut self, name: &str) {
        let dir = self.git_target_dir();
        let Some(repo) = Repo::discover(&dir) else {
            self.git.not_repo = true;
            self.git.status = None;
            return;
        };
        match repo.checkout(name) {
            Ok(()) => {
                self.git.notice = None;
                self.reload_dir(&dir);
                self.request_git_refresh(true);
            }
            Err(error) => self.git.notice = Some(error.to_string()),
        }
    }

    /// Cria uma branch no HEAD atual. Retorna o erro para o diálogo exibir.
    fn git_create_branch(&mut self, name: &str) -> Result<(), String> {
        let dir = self.git_target_dir();
        let Some(repo) = Repo::discover(&dir) else {
            self.git.not_repo = true;
            self.git.status = None;
            return Err("not a git repository".to_string());
        };
        repo.create_branch(name)
            .map_err(|error| error.to_string())?;
        self.request_git_refresh(true);
        Ok(())
    }

    /// Exclui uma branch e recarrega a lista.
    fn git_delete_branch(&mut self, name: &str) -> Result<(), String> {
        let dir = self.git_target_dir();
        let Some(repo) = Repo::discover(&dir) else {
            self.git.not_repo = true;
            self.git.status = None;
            return Err("not a git repository".to_string());
        };
        repo.delete_branch(name)
            .map_err(|error| error.to_string())?;
        self.request_git_refresh(true);
        Ok(())
    }

    /// Guarda as mudanças atuais num stash.
    fn git_stash_push(&mut self, message: &str) {
        let dir = self.git_target_dir();
        let Some(repo) = Repo::discover(&dir) else {
            self.git.not_repo = true;
            self.git.status = None;
            return;
        };
        match repo.stash_push(message) {
            Ok(()) => {
                self.git.notice = None;
                self.reload_dir(&dir);
                self.request_git_refresh(true);
            }
            Err(error) => self.git.notice = Some(error.to_string()),
        }
    }

    /// Reaplica o stash indicado e o remove da pilha.
    fn git_stash_pop(&mut self, id: &str) {
        let dir = self.git_target_dir();
        let Some(repo) = Repo::discover(&dir) else {
            self.git.not_repo = true;
            self.git.status = None;
            return;
        };
        match repo.stash_pop(id) {
            Ok(()) => {
                self.git.notice = None;
                self.reload_dir(&dir);
                self.request_git_refresh(true);
            }
            Err(error) => self.git.notice = Some(error.to_string()),
        }
    }

    /// Recarrega os painéis que estão exibindo `dir`.
    fn reload_dir(&mut self, dir: &Path) {
        for panel in [&mut self.left, &mut self.right] {
            if panel.cwd == dir {
                panel.reload();
            }
        }
    }

    /// Abre o quick-open, iniciando o scan do `$HOME` no primeiro uso.
    ///
    /// Reabrir reaproveita o índice em memória; um rescans só acontece se o
    /// último scan tiver mais de [`QUICK_OPEN_REFRESH`].
    fn open_quick_open(&mut self) {
        self.quick_open.open = true;
        self.quick_open.needs_focus = true;
        self.quick_open.query.clear();
        self.quick_open.focus = 0;
        self.quick_open.results.clear();
        self.quick_open.last_query.clear();
        self.quick_open.dirty = true;

        let needs_scan = match self.quick_open.status {
            IndexScanStatus::Idle => true,
            IndexScanStatus::Scanning => false,
            IndexScanStatus::Ready => self
                .quick_open
                .last_scan
                .is_none_or(|last| last.elapsed() >= QUICK_OPEN_REFRESH),
        };
        if needs_scan {
            self.start_index_scan();
        }
    }

    /// Inicia o scan do diretório home em uma thread própria.
    fn start_index_scan(&mut self) {
        if self.quick_open.receiver.is_some() {
            return;
        }

        let root = home_directory();
        let cancel = Arc::clone(&self.quick_open.cancel);
        cancel.store(false, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.quick_open.receiver = Some(receiver);
        self.quick_open.status = IndexScanStatus::Scanning;
        self.quick_open.scanned = 0;

        std::thread::spawn(move || {
            let progress_sender = sender.clone();
            let entries = Indexer::new().scan_with_progress(&root, &cancel, |count| {
                let _ = progress_sender.send(IndexScanEvent::Progress(count));
            });
            // Canal descartado = app encerrado; a thread termina sozinha.
            let _ = sender.send(IndexScanEvent::Done(entries));
        });
    }

    /// Drena eventos do scan de índice sem bloquear a UI.
    fn pump_index_scan(&mut self, ctx: &egui::Context) {
        let mut progress = None;
        let mut finished = None;

        if let Some(receiver) = &self.quick_open.receiver {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    IndexScanEvent::Progress(count) => progress = Some(count),
                    IndexScanEvent::Done(entries) => finished = Some(entries),
                }
            }
        }

        if let Some(count) = progress {
            self.quick_open.scanned = count;
        }

        if let Some(entries) = finished {
            self.quick_open.store.update(entries);
            self.quick_open.status = IndexScanStatus::Ready;
            self.quick_open.scanned = self.quick_open.store.len();
            self.quick_open.receiver = None;
            self.quick_open.last_scan = Some(Instant::now());
            self.quick_open.dirty = true;
        }

        if self.quick_open.receiver.is_some() {
            ctx.request_repaint();
        }
    }

    /// Recalcula os resultados quando a consulta/índice mudou.
    fn refresh_quick_open_results(&mut self) {
        if !self.quick_open.dirty && self.quick_open.query == self.quick_open.last_query {
            return;
        }

        self.quick_open
            .last_query
            .clone_from(&self.quick_open.query);
        self.quick_open.results = self
            .quick_open
            .store
            .search(&self.quick_open.query, QUICK_OPEN_MAX_RESULTS);
        self.quick_open.dirty = false;

        if self.quick_open.results.is_empty() {
            self.quick_open.focus = 0;
        } else {
            self.quick_open.focus = self.quick_open.focus.min(self.quick_open.results.len() - 1);
        }
    }

    /// Ativa o resultado focado: navega o painel ativo e foca a entrada.
    fn activate_quick_open(&mut self) {
        let Some((_, entry)) = self.quick_open.results.get(self.quick_open.focus).cloned() else {
            return;
        };
        self.quick_open.open = false;
        self.active_panel_mut().reveal_path(&entry.path);
    }

    /// Overlay central de quick-open (Ctrl+P).
    fn draw_quick_open(&mut self, ctx: &egui::Context) {
        if !self.quick_open.open {
            return;
        }
        self.refresh_quick_open_results();

        let mut activate = false;
        let mut clicked_row = None;
        let quick = &mut self.quick_open;
        let scanning = quick.status == IndexScanStatus::Scanning;
        let scanned = quick.scanned;

        let modal = egui::Modal::new(egui::Id::unique("quick-open")).show(ctx, |ui| {
            ui.set_min_width(640.0);
            ui.set_max_width(640.0);

            let response = ui.add(
                egui::TextEdit::singleline(&mut quick.query)
                    .hint_text(rust_i18n::t!("quick_open_placeholder"))
                    .desired_width(f32::INFINITY),
            );
            if quick.needs_focus {
                response.request_focus();
                quick.needs_focus = false;
            }
            ui.add_space(6.0);

            if quick.results.is_empty() {
                let message = if scanning {
                    rust_i18n::t!("quick_open_indexing", count => scanned).into_owned()
                } else if quick.query.is_empty() {
                    rust_i18n::t!("quick_open_start").into_owned()
                } else {
                    rust_i18n::t!("quick_open_empty").into_owned()
                };
                ui.add_space(12.0);
                ui.label(
                    RichText::new(message)
                        .color(ui.visuals().weak_text_color())
                        .italics(),
                );
                ui.add_space(12.0);
            } else {
                let visuals = ui.visuals().clone();
                let weak = visuals.weak_text_color();
                for (index, (_, entry)) in quick.results.iter().enumerate() {
                    let selected = index == quick.focus;
                    let (rect, row) = ui.allocate_exact_size(
                        vec2(ui.available_width(), QUICK_OPEN_ROW_H),
                        Sense::click(),
                    );
                    if selected {
                        ui.painter()
                            .rect_filled(rect, 3.0, visuals.selection.bg_fill);
                    }
                    let text_color = if selected {
                        visuals.strong_text_color()
                    } else {
                        visuals.text_color()
                    };
                    let mut row_ui = ui.new_child(
                        UiBuilder::new()
                            .max_rect(rect.shrink2(vec2(8.0, 2.0)))
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    row_ui.spacing_mut().item_spacing.x = 10.0;
                    row_ui.label(RichText::new(&entry.name).strong().color(text_color));
                    row_ui.add(
                        Label::new(
                            RichText::new(entry.path.display().to_string())
                                .small()
                                .color(weak),
                        )
                        .truncate(),
                    );

                    if row.clicked() {
                        clicked_row = Some(index);
                    }
                    if row.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            }

            ui.add_space(4.0);
            let footer = if scanning {
                rust_i18n::t!("quick_open_indexing", count => scanned).into_owned()
            } else {
                rust_i18n::t!("quick_open_footer", count => quick.store.len()).into_owned()
            };
            ui.label(
                RichText::new(footer)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        });

        if modal.should_close() {
            self.quick_open.open = false;
        }
        if let Some(index) = clicked_row {
            self.quick_open.focus = index;
            activate = true;
        }
        if activate {
            self.activate_quick_open();
        }
    }

    /// Persiste as tags (em testes, não toca no disco real).
    fn persist_tags(&self) {
        #[cfg(not(test))]
        if let Err(error) = self.tags.save() {
            eprintln!("arandu: não foi possível salvar as tags: {error}");
        }
    }

    /// Pede confirmação para enviar a seleção à lixeira.
    fn request_delete(&mut self) {
        let paths = self.active_panel().selected_paths();
        if !paths.is_empty() {
            self.delete_confirm = Some(paths);
        }
    }

    /// Confirma a exclusão pendente (não desfazível).
    fn confirm_delete(&mut self) {
        if let Some(paths) = self.delete_confirm.take() {
            for path in paths {
                let _ = self.queue.enqueue(Op::new(OpKind::Delete, path, None));
            }
        }
    }

    /// Desfaz a última operação desfazível, se houver.
    fn undo_last(&mut self) {
        let Some(completed) = self.undo.pop() else {
            return;
        };
        if let Some(op) = ops::undo(&completed) {
            let _ = self.queue.enqueue(op);
            self.ops.notice = Some(Notice::info(rust_i18n::t!("undo_done").to_string()));
        }
    }

    fn draw_toolbar(&mut self, ui: &mut Ui, rect: Rect) {
        let visuals = ui.visuals().clone();
        ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            visuals.widgets.noninteractive.bg_stroke,
        );

        ui.scope_builder(
            UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 0.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    let menu = ui
                        .add(
                            Label::new(RichText::new(rust_i18n::t!("menu")).size(14.0))
                                .sense(Sense::CLICK),
                        )
                        .on_hover_text(rust_i18n::t!("menu"));
                    egui::Popup::menu(&menu).show(|ui| {
                        ui.label(format!("{APP_NAME} v{}", env!("CARGO_PKG_VERSION")));
                    });

                    ui.separator();
                    ui.label(RichText::new(APP_NAME).strong());

                    let git_hint = self
                        .keymap
                        .effective(Action::ToggleGitPanel)
                        .into_iter()
                        .next()
                        .map(|binding| {
                            format!("{binding} {}", action_description(Action::ToggleGitPanel))
                        })
                        .unwrap_or_default();
                    let git_button = ui
                        .add(
                            Label::new(RichText::new(rust_i18n::t!("git_title")).size(14.0))
                                .sense(Sense::CLICK),
                        )
                        .on_hover_text(git_hint);
                    if git_button.clicked() {
                        self.toggle_git_panel();
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let theme_button = ui
                            .add(
                                Label::new(
                                    RichText::new(self.config.theme.name.clone())
                                        .size(12.5)
                                        .color(ui.visuals().weak_text_color()),
                                )
                                .sense(Sense::CLICK),
                            )
                            .on_hover_text(rust_i18n::t!("toggle_theme"));
                        if theme_button.clicked() {
                            self.cycle_theme(ui.ctx());
                        }

                        let view_mode = self.active_panel().view_mode;
                        let view_hint = self
                            .keymap
                            .effective(Action::ToggleViewMode)
                            .into_iter()
                            .next()
                            .map(|binding| {
                                format!("{binding} {}", action_description(Action::ToggleViewMode))
                            })
                            .unwrap_or_else(|| action_description(Action::ToggleViewMode));
                        let view_button = ui
                            .add(
                                Label::new(
                                    RichText::new(view_mode.label())
                                        .size(12.5)
                                        .color(ui.visuals().weak_text_color()),
                                )
                                .sense(Sense::CLICK),
                            )
                            .on_hover_text(view_hint);
                        if view_button.clicked() {
                            self.active_panel_mut().toggle_view_mode();
                        }
                        let settings = ui
                            .add(Label::new(rust_i18n::t!("settings")).sense(Sense::CLICK))
                            .on_hover_text(rust_i18n::t!("settings"));
                        let _ = settings;
                        let search = ui
                            .add(Label::new(rust_i18n::t!("search")).sense(Sense::CLICK))
                            .on_hover_text(rust_i18n::t!("search"));
                        let _ = search;
                    });
                });
            },
        );
    }

    fn draw_content(&mut self, ui: &mut Ui, content: Rect) {
        let visuals = ui.visuals().clone();
        let accent = accent_color(&self.theme, &visuals);

        // Preview e painel Git ficam fixos à direita; os dois painéis dividem
        // o restante. O Git ocupa a borda direita e o preview vem antes dele.
        let git_w = if self.git.visible {
            let max_w = content.width() * GIT_MAX_FRACTION;
            (content.width() * GIT_FRACTION)
                .max(GIT_MIN_W.min(max_w))
                .min(max_w)
        } else {
            0.0
        };
        let preview_space = (content.width() - git_w).max(1.0);
        let preview_w = if self.preview.visible {
            let max_w = preview_space * PREVIEW_MAX_FRACTION;
            (preview_space * PREVIEW_FRACTION)
                .max(PREVIEW_MIN_W.min(max_w))
                .min(max_w)
        } else {
            0.0
        };
        let panes_rect = Rect::from_min_max(
            content.min,
            pos2(
                (content.right() - git_w - preview_w).max(content.left() + 1.0),
                content.bottom(),
            ),
        );

        let avail = (panes_rect.width() - SPLITTER_HIT).max(1.0);
        let min_w = MIN_PANEL_W.min(avail / 2.0);
        let left_w = (self.split * avail).clamp(min_w, avail - min_w);

        let left_rect = Rect::from_min_max(
            panes_rect.min,
            pos2(panes_rect.left() + left_w, panes_rect.bottom()),
        );
        let splitter_rect = Rect::from_min_max(
            pos2(left_rect.right(), panes_rect.top()),
            pos2(left_rect.right() + SPLITTER_HIT, panes_rect.bottom()),
        );
        let right_rect = Rect::from_min_max(
            pos2(splitter_rect.right(), panes_rect.top()),
            panes_rect.max,
        );

        let splitter = ui
            .interact(
                splitter_rect,
                ui.scope_id().with("splitter"),
                Sense::CLICK | Sense::DRAG,
            )
            .on_hover_text(rust_i18n::t!("splitter_hint"));
        if splitter.double_clicked() {
            self.split = 0.5;
        }
        if splitter.dragged() {
            if let Some(pos) = ui.ctx().pointer_interact_pos() {
                let min_w = MIN_PANEL_W.min(avail / 2.0);
                let new_left = (pos.x - panes_rect.left()).clamp(min_w, avail - min_w);
                self.split = new_left / avail;
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        let left_tags = self.tag_map_for(&self.left);
        let right_tags = self.tag_map_for(&self.right);

        let left_interacted = draw_panel(
            ui,
            left_rect,
            &mut self.left,
            PanelSide::Left,
            self.active == PanelSide::Left,
            accent,
            &left_tags,
        );
        let right_interacted = draw_panel(
            ui,
            right_rect,
            &mut self.right,
            PanelSide::Right,
            self.active == PanelSide::Right,
            accent,
            &right_tags,
        );

        if left_interacted {
            self.active = PanelSide::Left;
            self.git.focused = false;
        } else if right_interacted {
            self.active = PanelSide::Right;
            self.git.focused = false;
        }

        // Linha do splitter por cima das bordas dos painéis.
        let splitter_color = if splitter.hovered() || splitter.dragged() {
            accent
        } else {
            visuals.widgets.noninteractive.bg_stroke.color
        };
        ui.painter().vline(
            splitter_rect.center().x,
            panes_rect.y_range(),
            Stroke::new(1.0, splitter_color),
        );

        if self.preview.visible {
            let preview_rect = Rect::from_min_max(
                pos2(panes_rect.right(), content.top()),
                pos2(content.right() - git_w, content.bottom()),
            );
            let focused = self
                .active_panel()
                .entries
                .get(self.active_panel().focus)
                .cloned();
            self.ensure_preview(focused.as_ref());
            self.draw_preview(ui, preview_rect, focused.as_ref(), &visuals);
        }

        if self.git.visible {
            let git_rect =
                Rect::from_min_max(pos2(content.right() - git_w, content.top()), content.max);
            self.draw_git_panel(ui, git_rect, &visuals);
        }
    }

    /// Desenha o painel Git completo (cabeçalho, listas e formulário).
    fn draw_git_panel(&mut self, ui: &mut Ui, rect: Rect, visuals: &egui::Visuals) {
        ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);

        let inner = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.top()),
            pos2(rect.right() - 8.0, rect.bottom()),
        );
        let toggle_hint = self
            .keymap
            .effective(Action::ToggleGitPanel)
            .into_iter()
            .next()
            .map(|binding| format!("{binding} {}", action_description(Action::ToggleGitPanel)));
        let branch_info = self.git.status.as_ref().map(|status| {
            let branch = status
                .branch
                .clone()
                .unwrap_or_else(|| rust_i18n::t!("git_detached").into_owned());
            let upstream = status
                .upstream
                .as_ref()
                .map(|upstream| format!("{upstream} ↑{} ↓{}", status.ahead, status.behind));
            (branch, upstream)
        });
        let mut next_tab: Option<GitTab> = None;

        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let header = ui.allocate_ui_with_layout(
                vec2(ui.available_width(), HEADER_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(RichText::new(rust_i18n::t!("git_title")).strong());
                    let tabs = [
                        (GitTab::Status, rust_i18n::t!("git_tab_status").into_owned()),
                        (GitTab::Log, rust_i18n::t!("git_tab_log").into_owned()),
                        (GitTab::Blame, rust_i18n::t!("git_tab_blame").into_owned()),
                        (
                            GitTab::Branches,
                            rust_i18n::t!("git_tab_branches").into_owned(),
                        ),
                        (
                            GitTab::Stashes,
                            rust_i18n::t!("git_tab_stashes").into_owned(),
                        ),
                    ];
                    for (tab, label) in tabs {
                        let response = ui
                            .selectable_label(self.git.tab == tab, RichText::new(label).size(11.5));
                        if response.clicked() {
                            next_tab = Some(tab);
                        }
                    }
                    if let Some((branch, upstream)) = &branch_info {
                        ui.label(
                            RichText::new(branch)
                                .size(12.0)
                                .color(visuals.weak_text_color()),
                        );
                        if let Some(upstream) = upstream {
                            ui.label(
                                RichText::new(upstream)
                                    .size(11.0)
                                    .color(visuals.weak_text_color()),
                            );
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(hint) = &toggle_hint {
                            ui.label(
                                RichText::new(hint)
                                    .size(12.0)
                                    .color(visuals.weak_text_color()),
                            );
                        }
                    });
                },
            );
            ui.painter().hline(
                header.response.rect.x_range(),
                header.response.rect.bottom(),
                visuals.widgets.noninteractive.bg_stroke,
            );

            let body =
                Rect::from_min_max(pos2(inner.left(), header.response.rect.bottom()), inner.max);
            self.draw_git_body(ui, body, visuals);
        });

        if let Some(tab) = next_tab {
            self.git.tab = tab;
            self.git.focus = 0;
        }

        ui.painter().vline(
            rect.left(),
            rect.y_range(),
            visuals.widgets.noninteractive.bg_stroke,
        );
    }

    fn draw_git_body(&mut self, ui: &mut Ui, body: Rect, visuals: &egui::Visuals) {
        let background = ui.interact(body, ui.scope_id().with("git-panel-bg"), Sense::click());
        if background.clicked() {
            self.git.focused = true;
        }

        if self.git.not_repo && self.git.status.is_none() {
            draw_preview_centered(ui, body, rust_i18n::t!("git_not_repository"), visuals);
            return;
        }
        if let Some(error) = self.git.error.clone() {
            draw_preview_centered(ui, body, error, visuals);
            return;
        }

        match self.git.tab {
            GitTab::Status => self.draw_git_status_body(ui, body, visuals),
            GitTab::Log => self.draw_git_log_body(ui, body, visuals),
            GitTab::Blame => self.draw_git_blame_body(ui, body, visuals),
            GitTab::Branches => self.draw_git_branches_body(ui, body, visuals),
            GitTab::Stashes => self.draw_git_stashes_body(ui, body, visuals),
        }
    }

    /// Aba Status: seções Staged/Modified/Untracked e formulário de commit.
    fn draw_git_status_body(&mut self, ui: &mut Ui, body: Rect, visuals: &egui::Visuals) {
        let Some(status) = self.git.status.clone() else {
            draw_preview_centered(ui, body, rust_i18n::t!("git_loading"), visuals);
            return;
        };
        let root = self.git.root.clone().unwrap_or_default();

        let footer_h = GIT_COMMIT_H.min(body.height() * 0.5);
        let list_rect = Rect::from_min_max(
            body.min,
            pos2(
                body.right(),
                (body.bottom() - footer_h).max(body.top() + 1.0),
            ),
        );
        let footer_rect = Rect::from_min_max(pos2(body.left(), list_rect.bottom()), body.max);

        let focus = self.git.focus;
        let focused_panel = self.git.focused;
        let mut clicked: Option<(usize, bool, PathBuf)> = None;
        let mut commit_clicked = false;
        let mut row_index = 0usize;

        ui.scope_builder(
            UiBuilder::new().max_rect(list_rect.shrink2(vec2(6.0, 4.0))),
            |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let sections = [
                            (
                                GitSection::Staged,
                                rust_i18n::t!("git_staged").into_owned(),
                                status.staged.clone(),
                                false,
                            ),
                            (
                                GitSection::Modified,
                                rust_i18n::t!("git_modified").into_owned(),
                                status.modified.clone(),
                                true,
                            ),
                            (
                                GitSection::Untracked,
                                rust_i18n::t!("git_untracked").into_owned(),
                                status.untracked.clone(),
                                true,
                            ),
                        ];

                        for (_section, title, entries, stage) in sections {
                            if entries.is_empty() {
                                continue;
                            }
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(format!("{title} ({})", entries.len()))
                                    .small()
                                    .strong()
                                    .color(visuals.weak_text_color()),
                            );
                            for entry in entries {
                                let focused = focused_panel && row_index == focus;
                                let text = format!(
                                    "{}  {}",
                                    status_glyph(entry.status),
                                    entry.path.display()
                                );
                                let mut rich = RichText::new(text).size(13.0);
                                if focused {
                                    rich = rich.strong().color(visuals.strong_text_color());
                                }
                                let response = ui
                                    .add(Label::new(rich).sense(Sense::click()))
                                    .on_hover_text(entry.path.display().to_string());
                                if response.clicked() {
                                    clicked = Some((row_index, stage, root.join(&entry.path)));
                                }
                                row_index += 1;
                            }
                        }

                        if row_index == 0 {
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(rust_i18n::t!("git_clean"))
                                    .color(visuals.weak_text_color()),
                            );
                        }
                    });
            },
        );

        ui.scope_builder(
            UiBuilder::new().max_rect(footer_rect.shrink2(vec2(6.0, 2.0))),
            |ui| {
                ui.separator();
                let editor = egui::TextEdit::multiline(&mut self.git.commit_message)
                    .hint_text(rust_i18n::t!("git_commit_placeholder"))
                    .desired_rows(2)
                    .desired_width(f32::INFINITY);
                let response = ui.add(editor);
                self.git.commit_focused = response.has_focus();

                let enabled =
                    !status.staged.is_empty() && !self.git.commit_message.trim().is_empty();
                let button =
                    ui.add_enabled(enabled, egui::Button::new(rust_i18n::t!("git_commit")));
                if button.clicked() {
                    commit_clicked = true;
                }
                if let Some(error) = &self.git.action_error {
                    ui.label(
                        RichText::new(error)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                }
            },
        );

        if let Some((index, stage, path)) = clicked {
            self.git.focus = index;
            self.git.focused = true;
            self.git_toggle(path, stage);
        }
        if commit_clicked {
            self.git_commit();
        }
    }

    /// Aba Log: lista virtualizada de commits com grafo ASCII.
    fn draw_git_log_body(&mut self, ui: &mut Ui, body: Rect, visuals: &egui::Visuals) {
        if let Some(error) = self.git.log_error.clone() {
            draw_preview_centered(ui, body, error, visuals);
            return;
        }
        if self.git.log.is_empty() {
            let message = if self.git.log_receiver.is_some() {
                rust_i18n::t!("git_loading")
            } else {
                rust_i18n::t!("git_log_empty")
            };
            draw_preview_centered(ui, body, message, visuals);
            return;
        }

        const LOG_ROW_H: f32 = 20.0;
        let rows = self.git.log.len();
        let focus = self.git.focus;
        let focused_panel = self.git.focused;
        let mut clicked = None;

        ui.scope_builder(
            UiBuilder::new().max_rect(body.shrink2(vec2(6.0, 4.0))),
            |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, LOG_ROW_H, rows, |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for index in range {
                            let commit = &self.git.log[index];
                            let graph = self
                                .git
                                .log_graph
                                .get(index)
                                .map(String::as_str)
                                .unwrap_or("");
                            let text = format!(
                                "{graph:<6} {} {:<44} {:<12} {}",
                                commit.id, commit.message, commit.author, commit.time
                            );
                            let mut rich = RichText::new(text).monospace().size(11.5);
                            if focused_panel && index == focus {
                                rich = rich.strong().color(visuals.strong_text_color());
                            }
                            let response =
                                ui.add(Label::new(rich).truncate().sense(Sense::click()));
                            if response.clicked() {
                                clicked = Some(index);
                            }
                        }
                    });
            },
        );

        if let Some(index) = clicked {
            self.git.focus = index;
            self.git.focused = true;
        }
    }

    /// Aba Branches: branches locais, com a atual marcada.
    fn draw_git_branches_body(&mut self, ui: &mut Ui, body: Rect, visuals: &egui::Visuals) {
        if let Some(error) = self.git.branches_error.clone() {
            draw_preview_centered(ui, body, error, visuals);
            return;
        }
        if self.git.branches.is_empty() {
            let message = if self.git.branches_receiver.is_some() {
                rust_i18n::t!("git_loading")
            } else {
                rust_i18n::t!("git_branches_empty")
            };
            draw_preview_centered(ui, body, message, visuals);
            return;
        }

        const BRANCH_ROW_H: f32 = 20.0;
        let rows = self.git.branches.len();
        let focus = self.git.focus;
        let focused_panel = self.git.focused;
        let mut clicked = None;

        ui.scope_builder(
            UiBuilder::new().max_rect(body.shrink2(vec2(6.0, 4.0))),
            |ui| {
                ui.label(
                    RichText::new(rust_i18n::t!("git_branches_hint"))
                        .small()
                        .color(visuals.weak_text_color()),
                );
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, BRANCH_ROW_H, rows, |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for index in range {
                            let branch = &self.git.branches[index];
                            let marker = if branch.current { "●" } else { " " };
                            let text = format!("{marker} {:<28} {}", branch.name, branch.commit);
                            let mut rich = RichText::new(text).monospace().size(11.5);
                            if focused_panel && index == focus {
                                rich = rich.strong().color(visuals.strong_text_color());
                            }
                            let response =
                                ui.add(Label::new(rich).truncate().sense(Sense::click()));
                            if response.clicked() {
                                clicked = Some(index);
                            }
                        }
                    });
            },
        );

        if let Some(index) = clicked {
            self.git.focus = index;
            self.git.focused = true;
        }
    }

    /// Aba Stashes: pilha de stashes, do mais recente para o mais antigo.
    fn draw_git_stashes_body(&mut self, ui: &mut Ui, body: Rect, visuals: &egui::Visuals) {
        if let Some(error) = self.git.stashes_error.clone() {
            draw_preview_centered(ui, body, error, visuals);
            return;
        }
        if self.git.stashes.is_empty() {
            let message = if self.git.stashes_receiver.is_some() {
                rust_i18n::t!("git_loading")
            } else {
                rust_i18n::t!("git_stashes_empty")
            };
            draw_preview_centered(ui, body, message, visuals);
            return;
        }

        const STASH_ROW_H: f32 = 20.0;
        let rows = self.git.stashes.len();
        let focus = self.git.focus;
        let focused_panel = self.git.focused;
        let mut clicked = None;

        ui.scope_builder(
            UiBuilder::new().max_rect(body.shrink2(vec2(6.0, 4.0))),
            |ui| {
                ui.label(
                    RichText::new(rust_i18n::t!("git_stashes_hint"))
                        .small()
                        .color(visuals.weak_text_color()),
                );
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, STASH_ROW_H, rows, |ui, range| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for index in range {
                            let stash = &self.git.stashes[index];
                            let text = format!("{} {:<40} {}", stash.id, stash.message, stash.time);
                            let mut rich = RichText::new(text).monospace().size(11.5);
                            if focused_panel && index == focus {
                                rich = rich.strong().color(visuals.strong_text_color());
                            }
                            let response =
                                ui.add(Label::new(rich).truncate().sense(Sense::click()));
                            if response.clicked() {
                                clicked = Some(index);
                            }
                        }
                    });
            },
        );

        if let Some(index) = clicked {
            self.git.focus = index;
            self.git.focused = true;
        }
    }

    /// Aba Blame: metadados por linha + conteúdo do arquivo focado.
    fn draw_git_blame_body(&mut self, ui: &mut Ui, body: Rect, visuals: &egui::Visuals) {
        if self.git_blame_target().is_none() {
            draw_preview_centered(ui, body, rust_i18n::t!("git_blame_need_file"), visuals);
            return;
        }
        if let Some(error) = self.git.blame_error.clone() {
            draw_preview_centered(ui, body, error, visuals);
            return;
        }
        if self.git.blame.is_empty() {
            let message = if self.git.blame_receiver.is_some() {
                rust_i18n::t!("git_loading")
            } else {
                rust_i18n::t!("git_blame_empty")
            };
            draw_preview_centered(ui, body, message, visuals);
            return;
        }

        const BLAME_ROW_H: f32 = 20.0;
        let rows = self.git.blame.len();
        let truncated = self.git.blame_truncated;

        ui.scope_builder(
            UiBuilder::new().max_rect(body.shrink2(vec2(6.0, 4.0))),
            |ui| {
                if truncated {
                    ui.label(
                        RichText::new(rust_i18n::t!(
                            "git_blame_truncated",
                            max = DEFAULT_BLAME_LINES
                        ))
                        .small()
                        .color(visuals.weak_text_color()),
                    );
                }
                let meta_width = (ui.available_width() * 0.55).clamp(120.0, 280.0);
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, BLAME_ROW_H, rows, |ui, range| {
                        ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
                        for index in range {
                            let line = &self.git.blame[index];
                            let author: String = line.author.chars().take(10).collect();
                            let meta = format!("{} {:<10} {}", line.commit, author, line.time);
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [meta_width, BLAME_ROW_H - 2.0],
                                    Label::new(
                                        RichText::new(meta)
                                            .monospace()
                                            .size(11.0)
                                            .color(visuals.weak_text_color()),
                                    )
                                    .truncate(),
                                );
                                ui.add(
                                    Label::new(RichText::new(&line.content).monospace().size(11.5))
                                        .truncate(),
                                );
                            });
                        }
                    });
            },
        );
    }

    /// Desenha o painel de preview completo (header + corpo).
    fn draw_preview(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        entry: Option<&Entry>,
        visuals: &egui::Visuals,
    ) {
        ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);

        let inner = Rect::from_min_max(
            pos2(rect.left() + 8.0, rect.top()),
            pos2(rect.right() - 8.0, rect.bottom()),
        );
        // Hint do painel: binding efetivo (respeita preset/overrides) + descrição.
        let toggle_hint = self
            .keymap
            .effective(Action::TogglePreview)
            .into_iter()
            .next()
            .map(|binding| format!("{binding} {}", action_description(Action::TogglePreview)));
        let kind_label = entry.and_then(|entry| self.preview_kind_label(entry));
        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let header = ui.allocate_ui_with_layout(
                vec2(ui.available_width(), HEADER_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(RichText::new(rust_i18n::t!("preview_title")).strong());
                    if let Some(kind) = &kind_label {
                        ui.label(
                            RichText::new("·")
                                .size(12.5)
                                .color(visuals.weak_text_color()),
                        );
                        ui.label(
                            RichText::new(kind)
                                .size(12.5)
                                .color(visuals.weak_text_color()),
                        );
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if let Some(hint) = &toggle_hint {
                            ui.label(
                                RichText::new(hint)
                                    .size(12.5)
                                    .color(visuals.weak_text_color()),
                            );
                        }
                    });
                },
            );
            ui.painter().hline(
                header.response.rect.x_range(),
                header.response.rect.bottom(),
                visuals.widgets.noninteractive.bg_stroke,
            );

            let body =
                Rect::from_min_max(pos2(inner.left(), header.response.rect.bottom()), inner.max);
            self.draw_preview_body(ui, body, entry, visuals);
        });

        ui.painter().vline(
            rect.left(),
            rect.y_range(),
            visuals.widgets.noninteractive.bg_stroke,
        );
    }

    fn draw_preview_body(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        entry: Option<&Entry>,
        visuals: &egui::Visuals,
    ) {
        let Some(entry) = entry else {
            draw_preview_centered(ui, body, rust_i18n::t!("preview_empty"), visuals);
            return;
        };

        // Diretórios: apenas metadados (tamanho indisponível).
        if entry.is_dir {
            ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                draw_preview_metadata(ui, entry);
            });
            return;
        }

        let key = PreviewKey::for_entry(entry);
        match self.preview.cache.get(&key) {
            Some(PreviewEntry::Ready(content)) => match &*content {
                PreviewContent::Text {
                    text,
                    truncated,
                    syntax_name,
                } => {
                    self.draw_preview_text(
                        ui,
                        body,
                        &key,
                        text,
                        *truncated,
                        syntax_name.as_deref(),
                    );
                }
                PreviewContent::Markdown { text, truncated } => {
                    self.draw_preview_markdown(ui, body, text, *truncated);
                }
                PreviewContent::Hex {
                    bytes, truncated, ..
                } => {
                    self.draw_preview_hex(ui, body, bytes, *truncated);
                }
                PreviewContent::Csv { text, truncated } => {
                    self.draw_preview_csv(ui, body, text, *truncated);
                }
                PreviewContent::Json { text, truncated } => {
                    self.draw_preview_text(ui, body, &key, text, *truncated, Some("json"));
                }
                PreviewContent::Image { image, .. } => {
                    self.draw_preview_image(ui, body, &key, image, visuals);
                }
                PreviewContent::Pdf { page, .. } => {
                    self.draw_preview_rgba(ui, body, &key, page, visuals);
                }
                PreviewContent::Unsupported { reason } => {
                    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                        draw_preview_metadata(ui, entry);
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(rust_i18n::t!("preview_unavailable"))
                                .color(visuals.warn_fg_color),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(preview_reason(*reason)).color(visuals.weak_text_color()),
                        );
                    });
                }
            },
            Some(PreviewEntry::Failed(error)) => {
                ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                    draw_preview_metadata(ui, entry);
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(rust_i18n::t!("preview_unavailable"))
                            .color(visuals.warn_fg_color),
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new(error.as_str()).color(visuals.weak_text_color()));
                });
            }
            None => {
                draw_preview_centered(ui, body, rust_i18n::t!("preview_loading"), visuals);
            }
        }
    }

    fn draw_preview_text(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        key: &PreviewKey,
        text: &str,
        truncated: bool,
        syntax_name: Option<&str>,
    ) {
        let dark = ui.visuals().dark_mode;
        let rebuild = match &self.preview.job {
            Some((cached_key, cached_dark, _)) => cached_key != key || *cached_dark != dark,
            None => true,
        };
        if rebuild {
            self.preview.job = Some((
                key.clone(),
                dark,
                Arc::new(build_text_job(text, syntax_name, dark)),
            ));
        }
        let job = self
            .preview
            .job
            .as_ref()
            .map(|(_, _, job)| Arc::clone(job))
            .expect("job recém-construído");

        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            if truncated {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(rust_i18n::t!("preview_truncated"))
                        .size(12.5)
                        .color(ui.visuals().warn_fg_color),
                );
                ui.add_space(4.0);
            }
            ScrollArea::both()
                .id_salt("preview-text")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.add(Label::new(job).selectable(false));
                });
        });
    }

    /// Rótulo do tipo detectado exibido no header do painel.
    fn preview_kind_label(&self, entry: &Entry) -> Option<String> {
        if entry.is_dir {
            return Some(rust_i18n::t!("preview_kind_directory").to_string());
        }
        let key = PreviewKey::for_entry(entry);
        let PreviewEntry::Ready(content) = self.preview.cache.get(&key)? else {
            return None;
        };
        Some(match &*content {
            PreviewContent::Text { .. } => rust_i18n::t!("preview_kind_text").to_string(),
            PreviewContent::Markdown { .. } => rust_i18n::t!("preview_kind_markdown").to_string(),
            PreviewContent::Hex { .. } => rust_i18n::t!("preview_kind_hex").to_string(),
            PreviewContent::Csv { .. } => rust_i18n::t!("preview_kind_csv").to_string(),
            PreviewContent::Json { .. } => rust_i18n::t!("preview_kind_json").to_string(),
            PreviewContent::Image { .. } => rust_i18n::t!("preview_kind_image").to_string(),
            PreviewContent::Pdf { page_count, .. } => format!(
                "{} — {}",
                rust_i18n::t!("preview_kind_pdf"),
                rust_i18n::t!("preview_pdf_pages", count => *page_count)
            ),
            PreviewContent::Unsupported { .. } => return None,
        })
    }

    fn draw_preview_markdown(&mut self, ui: &mut Ui, body: Rect, text: &str, truncated: bool) {
        let cache = &mut self.preview.markdown_cache;
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            if truncated {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(rust_i18n::t!("preview_truncated"))
                        .size(12.5)
                        .color(ui.visuals().warn_fg_color),
                );
            }
            ScrollArea::vertical()
                .id_salt("preview-markdown")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    egui_commonmark::CommonMarkViewer::new().show(ui, cache, text);
                    ui.add_space(8.0);
                });
        });
    }

    fn draw_preview_hex(&mut self, ui: &mut Ui, body: Rect, bytes: &[u8], truncated: bool) {
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            if truncated {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(rust_i18n::t!("preview_truncated"))
                        .size(12.5)
                        .color(ui.visuals().warn_fg_color),
                );
            }
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
            let rows = bytes.len().div_ceil(HEX_ROW_BYTES);
            let visuals = ui.visuals().clone();
            ScrollArea::vertical()
                .id_salt("preview-hex")
                .auto_shrink([false, false])
                .show_rows(ui, row_height, rows, |ui, row_range| {
                    for row in row_range {
                        let offset = row * HEX_ROW_BYTES;
                        let end = (offset + HEX_ROW_BYTES).min(bytes.len());
                        let (offset_text, hex_text, ascii_text) =
                            hex_row(offset, &bytes[offset..end]);
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 12.0;
                            ui.label(
                                RichText::new(offset_text)
                                    .monospace()
                                    .color(visuals.weak_text_color()),
                            );
                            ui.label(RichText::new(hex_text).monospace());
                            ui.label(
                                RichText::new(ascii_text)
                                    .monospace()
                                    .color(visuals.weak_text_color()),
                            );
                        });
                    }
                });
        });
    }

    fn draw_preview_csv(&mut self, ui: &mut Ui, body: Rect, text: &str, truncated: bool) {
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            if truncated {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(rust_i18n::t!("preview_truncated"))
                        .size(12.5)
                        .color(ui.visuals().warn_fg_color),
                );
            }
            let mut lines = text.lines();
            let Some(header) = lines.next() else {
                return;
            };
            let delimiter = if header.contains('\t') && !header.contains(',') {
                '\t'
            } else {
                ','
            };
            let headers = parse_csv_row(header, delimiter);
            let rows: Vec<Vec<String>> = lines
                .take(CSV_MAX_ROWS + 1)
                .map(|line| parse_csv_row(line, delimiter))
                .collect();
            let capped = rows.len() > CSV_MAX_ROWS;
            ScrollArea::both()
                .id_salt("preview-csv")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("preview-csv-grid")
                        .striped(true)
                        .min_col_width(32.0)
                        .show(ui, |ui| {
                            for field in &headers {
                                ui.label(RichText::new(field.as_str()).strong());
                            }
                            ui.end_row();
                            for row in rows.iter().take(CSV_MAX_ROWS) {
                                for field in row {
                                    ui.label(if field.is_empty() { " " } else { field });
                                }
                                ui.end_row();
                            }
                        });
                    if capped {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(rust_i18n::t!(
                                "preview_csv_limit",
                                count => CSV_MAX_ROWS
                            ))
                            .size(12.5)
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                });
        });
    }

    fn draw_preview_image(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        key: &PreviewKey,
        image: &image::DynamicImage,
        visuals: &egui::Visuals,
    ) {
        let rgba = image.to_rgba8();
        self.draw_preview_rgba(ui, body, key, &rgba, visuals);
    }

    fn draw_preview_rgba(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        key: &PreviewKey,
        rgba: &image::RgbaImage,
        _visuals: &egui::Visuals,
    ) {
        let stale = self
            .preview
            .texture
            .as_ref()
            .map(|(cached_key, _)| cached_key != key)
            .unwrap_or(true);
        if stale {
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            );
            let texture = ui.ctx().load_texture(
                format!("preview:{}", key.path.display()),
                color,
                egui::TextureOptions::LINEAR,
            );
            self.preview.texture = Some((key.clone(), texture));
        }

        let Some((_, texture)) = &self.preview.texture else {
            return;
        };
        let texture = texture.clone();

        let available = body.size() - vec2(0.0, 8.0);
        let width = rgba.width() as f32;
        let height = rgba.height() as f32;
        let scale = (available.x / width)
            .min(available.y / height)
            .clamp(0.01, 1.0);
        let size = vec2(width * scale, height * scale);

        ui.scope_builder(
            UiBuilder::new()
                .max_rect(body)
                .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
            |ui| {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    texture.id(),
                    size,
                )));
            },
        );
    }

    fn draw_ops_bar(&mut self, ui: &mut Ui, rect: Rect) {
        let visuals = ui.visuals().clone();
        ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            visuals.widgets.noninteractive.bg_stroke,
        );

        let Some(current) = &self.ops.current else {
            return;
        };
        let kind = current.kind;
        let name = current.name.clone();
        let progress = current.progress.clone();
        let percent = progress
            .as_ref()
            .map(|progress| (progress.fraction() * 100.0).round() as u32)
            .unwrap_or(0);
        let fraction = progress
            .as_ref()
            .map(Progress::fraction)
            .unwrap_or_default();
        let verb = match kind {
            OpKind::Copy => rust_i18n::t!("copying"),
            OpKind::Move => rust_i18n::t!("moving"),
            OpKind::Delete => rust_i18n::t!("deleting"),
            OpKind::Rename => rust_i18n::t!("renaming"),
        };
        let label = format!("{verb} {name} — {percent}%");
        let mut cancel = false;

        ui.scope_builder(
            UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 0.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add(Label::new(RichText::new(label).size(13.0)).truncate());
                    ui.add_space(8.0);
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(200.0)
                            .text(format!("{percent}%")),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .button(rust_i18n::t!("cancel_operation"))
                            .on_hover_text(rust_i18n::t!("cancel_operation"))
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
            },
        );

        if cancel {
            self.queue.cancel();
        }
    }

    fn draw_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(paths) = self.delete_confirm.clone() {
            let mut confirm = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("delete-confirm")).show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.heading(rust_i18n::t!("delete_confirm_title"));
                ui.add_space(6.0);
                let body = if paths.len() == 1 {
                    let name = paths[0]
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| paths[0].display().to_string());
                    rust_i18n::t!("delete_confirm_body", name = name)
                } else {
                    rust_i18n::t!("delete_confirm_body_many", count = paths.len())
                };
                ui.label(body);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("delete_confirm_ok")).clicked() {
                        confirm = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });
            if confirm {
                self.confirm_delete();
            } else if cancel {
                self.delete_confirm = None;
            }
        }

        if let Some(mut dialog) = self.rename.take() {
            let mut submit = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("rename-dialog")).show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.heading(rust_i18n::t!("rename_title"));
                ui.add_space(6.0);
                ui.label(rust_i18n::t!("rename_label"));
                let response = ui
                    .add(egui::TextEdit::singleline(&mut dialog.name).desired_width(f32::INFINITY));
                if !response.has_focus() {
                    response.request_focus();
                }
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("rename_ok")).clicked() {
                        submit = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            if submit {
                let name = dialog.name.trim().to_owned();
                if name.is_empty() {
                    dialog.error = Some(rust_i18n::t!("rename_empty").to_string());
                    self.rename = Some(dialog);
                } else {
                    let _ = self.queue.enqueue(Op::new(
                        OpKind::Rename,
                        dialog.path,
                        Some(PathBuf::from(name)),
                    ));
                    self.ops.notice = None;
                }
            } else if !cancel {
                self.rename = Some(dialog);
            }
        }

        if let Some(mut dialog) = self.tag_editor.take() {
            // União das tags dos alvos, usada para exibir e remover.
            let mut union = std::collections::BTreeSet::new();
            for path in &dialog.paths {
                union.extend(self.tags.tags_of(path));
            }
            let existing: Vec<String> = union.into_iter().collect();

            let mut submit = false;
            let mut cancel = false;
            let mut remove: Option<String> = None;

            egui::Modal::new(egui::Id::unique("tag-editor-dialog")).show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.heading(rust_i18n::t!("tags_title"));
                ui.add_space(6.0);
                ui.label(rust_i18n::t!("tags_targets", count = dialog.paths.len()));
                ui.add_space(6.0);

                if existing.is_empty() {
                    ui.label(
                        RichText::new(rust_i18n::t!("tags_empty"))
                            .color(ui.visuals().weak_text_color()),
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for tag in &existing {
                            if ui.button(format!("{tag} ×")).clicked() {
                                remove = Some(tag.clone());
                            }
                        }
                    });
                }

                ui.add_space(8.0);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.input)
                        .hint_text(rust_i18n::t!("tags_input_placeholder"))
                        .desired_width(f32::INFINITY),
                );
                if !response.has_focus() {
                    response.request_focus();
                }
                if let Some(error) = dialog.error.take() {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            if let Some(tag) = remove {
                for path in &dialog.paths {
                    self.tags.remove(path, &tag);
                }
                self.persist_tags();
            }

            if submit {
                let tag = dialog.input.trim().to_owned();
                if tag.is_empty() {
                    dialog.error = Some(rust_i18n::t!("tags_invalid").to_string());
                    self.tag_editor = Some(dialog);
                } else {
                    let added = dialog
                        .paths
                        .iter()
                        .try_for_each(|path| self.tags.add(path, &tag));
                    if added.is_ok() {
                        self.persist_tags();
                        dialog.input.clear();
                        dialog.error = None;
                    } else {
                        dialog.error = Some(rust_i18n::t!("tags_invalid").to_string());
                    }
                    self.tag_editor = Some(dialog);
                }
            } else if !cancel {
                self.tag_editor = Some(dialog);
            }
        }

        if let Some(mut dialog) = self.compress.take() {
            let mut submit = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("compress-dialog")).show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.heading(rust_i18n::t!("compress_title"));
                ui.add_space(6.0);
                ui.label(rust_i18n::t!("compress_output_label"));
                let response = ui
                    .add(egui::TextEdit::singleline(&mut dialog.name).desired_width(f32::INFINITY));
                if !response.has_focus() {
                    response.request_focus();
                }
                let valid = archive::detect_format(Path::new(dialog.name.trim())).is_some();
                if !valid {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        rust_i18n::t!("compress_invalid_extension"),
                    );
                }
                ui.label(
                    RichText::new(rust_i18n::t!(
                        "compress_destination",
                        dir = dialog.destination_dir.display().to_string()
                    ))
                    .color(ui.visuals().weak_text_color()),
                );
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(valid, egui::Button::new(rust_i18n::t!("compress_ok")))
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            let name = dialog.name.trim().to_owned();
            if submit && archive::detect_format(Path::new(&name)).is_some() {
                self.begin_compress(dialog, name);
            } else if !cancel {
                self.compress = Some(dialog);
            }
        }

        if let Some(mut dialog) = self.open_with_dialog.take() {
            let configured: Vec<(String, String)> = self
                .config
                .open_with
                .iter()
                .map(|(extension, command)| (extension.clone(), command.clone()))
                .collect();
            let mut submit = false;
            let mut use_default = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("open-with-dialog")).show(ctx, |ui| {
                ui.set_min_width(460.0);
                ui.heading(rust_i18n::t!("open_with_title"));
                ui.add_space(6.0);
                ui.label(rust_i18n::t!(
                    "open_with_targets",
                    count = dialog.paths.len()
                ));
                if let Some(first) = dialog.paths.first() {
                    let name = first
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| first.display().to_string());
                    ui.label(RichText::new(name).color(ui.visuals().weak_text_color()));
                }
                ui.add_space(8.0);

                if !configured.is_empty() {
                    ui.label(rust_i18n::t!("open_with_configured_label"));
                    ui.horizontal_wrapped(|ui| {
                        for (extension, command) in &configured {
                            if ui
                                .button(format!(".{extension} — {command}"))
                                .on_hover_text(command)
                                .clicked()
                            {
                                dialog.command = command.clone();
                            }
                        }
                    });
                    ui.add_space(8.0);
                }

                ui.label(rust_i18n::t!("open_with_command_label"));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.command)
                        .hint_text(rust_i18n::t!("open_with_command_hint"))
                        .desired_width(f32::INFINITY),
                );
                if !response.has_focus() && dialog.error.is_none() {
                    response.request_focus();
                }
                ui.label(
                    RichText::new(rust_i18n::t!("open_with_command_help"))
                        .color(ui.visuals().weak_text_color()),
                );
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("open_with_ok")).clicked() {
                        submit = true;
                    }
                    if ui
                        .button(rust_i18n::t!("open_with_system_default"))
                        .clicked()
                    {
                        use_default = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            if use_default {
                self.open_paths_default(&dialog.paths);
            } else if submit {
                let command = dialog.command.trim().to_owned();
                if command.is_empty() {
                    dialog.error = Some(rust_i18n::t!("open_with_empty_command").to_string());
                    self.open_with_dialog = Some(dialog);
                } else if let Some(first) = dialog.paths.first() {
                    self.remember_open_command(first, &command);
                    self.execute_open_command(&command, &dialog.paths);
                }
            } else if !cancel {
                self.open_with_dialog = Some(dialog);
            }
        }

        if let Some(dialog) = self.checksums.take() {
            let mut close = false;
            egui::Modal::new(egui::Id::unique("checksums-dialog")).show(ctx, |ui| {
                ui.set_min_width(640.0);
                ui.heading(rust_i18n::t!("checksums_title"));
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .show(ui, |ui| {
                        for (index, path) in dialog.paths.iter().enumerate() {
                            let name = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.display().to_string());
                            ui.label(RichText::new(name).strong());
                            match &dialog.results[index] {
                                None => {
                                    ui.label(
                                        RichText::new(rust_i18n::t!("checksums_computing"))
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                }
                                Some(Ok(hashes)) => {
                                    hash_row(
                                        ui,
                                        &rust_i18n::t!("checksums_sha256"),
                                        &hashes.sha256,
                                    );
                                    hash_row(ui, &rust_i18n::t!("checksums_md5"), &hashes.md5);
                                }
                                Some(Err(error)) => {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        rust_i18n::t!("checksums_failed", error = error.as_str()),
                                    );
                                }
                            }
                            ui.add_space(6.0);
                        }
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("checksums_close")).clicked() {
                        close = true;
                    }
                });
            });
            if !close {
                self.checksums = Some(dialog);
            }
        }

        if let Some(mut dialog) = self.batch_rename.take() {
            let renames = batch_renames(&dialog.paths, &dialog.pattern);
            let validation = validate_batch_renames(&renames);
            let mut submit = false;
            let mut cancel = false;

            egui::Modal::new(egui::Id::unique("batch-rename-dialog")).show(ctx, |ui| {
                ui.set_min_width(580.0);
                ui.heading(rust_i18n::t!("batch_rename_title"));
                ui.add_space(6.0);
                ui.label(rust_i18n::t!("batch_rename_pattern"));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.pattern)
                        .hint_text("{name}{ext}")
                        .desired_width(f32::INFINITY),
                );
                if !response.has_focus() {
                    response.request_focus();
                }
                ui.label(
                    RichText::new(rust_i18n::t!("batch_rename_hint"))
                        .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for (_, current, new_name) in renames.iter().take(8) {
                            let line = format!("{current} → {}", new_name.trim());
                            ui.label(
                                RichText::new(truncate_for_display(&line, 72))
                                    .text_style(TextStyle::Monospace),
                            );
                        }
                        if renames.len() > 8 {
                            ui.label("…");
                        }
                    });
                if let Some(error) = &validation {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            validation.is_none(),
                            egui::Button::new(rust_i18n::t!("batch_rename_ok")),
                        )
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            if submit && validation.is_none() {
                let pairs = renames
                    .into_iter()
                    .map(|(path, _, new_name)| (path, new_name))
                    .collect();
                self.apply_batch_rename(pairs);
            } else if !cancel {
                self.batch_rename = Some(dialog);
            }
        }

        if let Some(mut dialog) = self.git.branch_dialog.take() {
            let mut submit = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("git-branch-dialog")).show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.heading(rust_i18n::t!("git_branch_new_title"));
                ui.add_space(6.0);
                ui.label(rust_i18n::t!("git_branch_new_label"));
                let response = ui
                    .add(egui::TextEdit::singleline(&mut dialog.name).desired_width(f32::INFINITY));
                if !response.has_focus() {
                    response.request_focus();
                }
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("git_branch_create")).clicked() {
                        submit = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            if submit {
                let name = dialog.name.trim().to_owned();
                if name.is_empty() {
                    dialog.error = Some(rust_i18n::t!("git_branch_name_empty").to_string());
                    self.git.branch_dialog = Some(dialog);
                } else if let Err(error) = self.git_create_branch(&name) {
                    dialog.error = Some(error);
                    self.git.branch_dialog = Some(dialog);
                }
            } else if !cancel {
                self.git.branch_dialog = Some(dialog);
            }
        }

        if let Some(mut dialog) = self.git.stash_dialog.take() {
            let mut submit = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("git-stash-dialog")).show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.heading(rust_i18n::t!("git_stash_push_title"));
                ui.add_space(6.0);
                ui.label(rust_i18n::t!("git_stash_push_label"));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.message)
                        .hint_text(rust_i18n::t!("git_stash_push_placeholder"))
                        .desired_width(f32::INFINITY),
                );
                if !response.has_focus() {
                    response.request_focus();
                }
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if ui.input(|input| input.key_pressed(Key::Enter)) {
                    submit = true;
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("git_stash_push_ok")).clicked() {
                        submit = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            if submit {
                let message = dialog.message.trim().to_owned();
                if message.is_empty() {
                    dialog.error = Some(rust_i18n::t!("git_stash_message_empty").to_string());
                    self.git.stash_dialog = Some(dialog);
                } else {
                    self.git_stash_push(&message);
                }
            } else if !cancel {
                self.git.stash_dialog = Some(dialog);
            }
        }

        if let Some(pending) = self.git.confirm.clone() {
            let mut confirm = false;
            let mut cancel = false;
            egui::Modal::new(egui::Id::unique("git-action-confirm")).show(ctx, |ui| {
                ui.set_min_width(380.0);
                match &pending {
                    GitConfirm::DeleteBranch(name) => {
                        ui.heading(rust_i18n::t!("git_branch_delete_title"));
                        ui.add_space(6.0);
                        ui.label(rust_i18n::t!(
                            "git_branch_delete_body",
                            name = name.as_str()
                        ));
                    }
                    GitConfirm::PopStash(id) => {
                        ui.heading(rust_i18n::t!("git_stash_pop_title"));
                        ui.add_space(6.0);
                        ui.label(rust_i18n::t!("git_stash_pop_body", id = id.as_str()));
                    }
                }
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    cancel = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("delete_confirm_ok")).clicked() {
                        confirm = true;
                    }
                    if ui.button(rust_i18n::t!("cancel")).clicked() {
                        cancel = true;
                    }
                });
            });

            if confirm {
                self.git.confirm = None;
                match pending {
                    GitConfirm::DeleteBranch(name) => {
                        if let Err(error) = self.git_delete_branch(&name) {
                            self.git.notice = Some(error);
                        }
                    }
                    GitConfirm::PopStash(id) => self.git_stash_pop(&id),
                }
            } else if cancel {
                self.git.confirm = None;
            }
        }

        if let Some(notice) = self.git.notice.clone() {
            let mut close = false;
            egui::Modal::new(egui::Id::unique("git-notice")).show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.heading(rust_i18n::t!("git_action_failed"));
                ui.add_space(6.0);
                ui.label(notice);
                if ui.input(|input| input.key_pressed(Key::Escape)) {
                    close = true;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(rust_i18n::t!("git_action_ok")).clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.git.notice = None;
            }
        }
    }

    fn draw_status_bar(&mut self, ui: &mut Ui, rect: Rect) {
        let visuals = ui.visuals().clone();
        ui.painter().rect_filled(rect, 0.0, visuals.faint_bg_color);
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            visuals.widgets.noninteractive.bg_stroke,
        );

        let panel = self.active_panel();
        let items = rust_i18n::t!("items_count", count = panel.item_count());
        let selected = rust_i18n::t!("selected_count", count = panel.selected_count());
        let path = panel.cwd.display().to_string();
        let notice = self.ops.notice.as_ref().map(|notice| {
            let color = if notice.error {
                visuals.error_fg_color
            } else {
                visuals.text_color()
            };
            (notice.text.clone(), color)
        });
        let weak = visuals.weak_text_color();
        let hints = self.status_hints();

        ui.scope_builder(
            UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 0.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(format!("{items}, {selected}")).color(weak));
                    ui.add_space(12.0);
                    ui.add(Label::new(RichText::new(path).color(weak)).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if !hints.is_empty() {
                            ui.add(
                                Label::new(RichText::new(&hints).size(12.5).color(weak)).truncate(),
                            );
                        }
                        if let Some((text, color)) = notice {
                            ui.add_space(12.0);
                            ui.add(Label::new(RichText::new(text).color(color)).truncate());
                        }
                    });
                });
            },
        );
    }

    /// Hints compactos das ações principais, gerados do keymap efetivo:
    /// `F3 Preview · F5 Copy · ...`. Ações sem binding (ex.: `move` no preset
    /// vim) são omitidas.
    fn status_hints(&self) -> String {
        STATUS_HINTS
            .iter()
            .filter_map(|action| {
                let binding = self.keymap.effective(*action).into_iter().next()?;
                Some(format!("{binding} {}", action_description(*action)))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Cheatsheet de atalhos sobre o conteúdo (F1).
    ///
    /// Segue o padrão de `draw_dialogs` (um `egui::Modal`), que também escurece
    /// o fundo e bloqueia a interação por trás. Fecha com F1, Escape (tratado
    /// em `handle_keyboard`) ou clique fora do painel.
    fn draw_hints_overlay(&mut self, ctx: &egui::Context) {
        if !self.show_hints {
            return;
        }

        let entries: Vec<(String, String)> = Action::ALL
            .iter()
            .map(|action| {
                let binding = self
                    .keymap
                    .effective(*action)
                    .into_iter()
                    .next()
                    .map_or_else(|| "—".to_owned(), |binding| binding.to_string());
                (binding, action_description(*action))
            })
            .collect();
        let rows = entries.len().div_ceil(2);

        let modal = egui::Modal::new(egui::Id::unique("hints-cheatsheet")).show(ctx, |ui| {
            ui.set_min_width(560.0);
            ui.heading(rust_i18n::t!("hints_title"));
            ui.add_space(6.0);
            let accent = ui.visuals().selection.stroke.color;
            egui::Grid::new("hints-grid")
                .num_columns(4)
                .spacing([24.0, 6.0])
                .min_col_width(80.0)
                .show(ui, |ui| {
                    for row in 0..rows {
                        for column in 0..2 {
                            let Some((binding, description)) = entries.get(column * rows + row)
                            else {
                                continue;
                            };
                            ui.label(RichText::new(binding).strong().color(accent));
                            ui.label(description);
                        }
                        ui.end_row();
                    }
                });
        });

        if modal.should_close() {
            self.show_hints = false;
        }
    }
}

impl Default for AranduApp {
    fn default() -> Self {
        Self::new(Config::default())
    }
}

impl eframe::App for AranduApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

/// Desenha um painel completo e devolve se o usuário interagiu com ele.
fn draw_panel(
    ui: &mut Ui,
    rect: Rect,
    panel: &mut PanelState,
    side: PanelSide,
    active: bool,
    accent: Color32,
    tags: &HashMap<String, Vec<String>>,
) -> bool {
    let visuals = ui.visuals().clone();
    let mut interacted = false;

    ui.scope_builder(UiBuilder::new().max_rect(rect.shrink(1.0)), |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.set_width((rect.width() - 2.0).max(1.0));

        let header = ui.allocate_ui_with_layout(
            vec2(ui.available_width(), HEADER_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(6.0);
                let ancestors: Vec<PathBuf> =
                    panel.cwd.ancestors().map(Path::to_path_buf).collect();
                let mut navigate = None;

                for (position, ancestor) in visible_breadcrumb(&ancestors).iter().enumerate() {
                    if position > 0 {
                        ui.label(RichText::new(">").color(secondary_color(&visuals, active)));
                    }
                    let is_current = *ancestor == panel.cwd;
                    let text = breadcrumb_label(ancestor);
                    if is_current {
                        ui.label(RichText::new(text).color(visuals.text_color()));
                    } else {
                        let response = ui.add(
                            Label::new(RichText::new(text).color(breadcrumb_color(active, accent)))
                                .sense(Sense::CLICK),
                        );
                        if response.clicked() {
                            navigate = Some(ancestor.clone());
                        }
                    }
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(4.0);
                    let drop_down = ui.add(
                        Label::new(RichText::new("v").color(secondary_color(&visuals, active)))
                            .sense(Sense::CLICK),
                    );
                    egui::Popup::menu(&drop_down).show(|ui| {
                        for ancestor in &ancestors {
                            let label = breadcrumb_label(ancestor);
                            if ui.add(Label::new(label).sense(Sense::CLICK)).clicked() {
                                navigate = Some(ancestor.clone());
                                ui.close();
                            }
                        }
                    });
                });

                navigate
            },
        );

        if active {
            ui.painter().hline(
                header.response.rect.x_range(),
                header.response.rect.bottom() - 1.0,
                Stroke::new(2.0, accent),
            );
        } else {
            ui.painter().hline(
                header.response.rect.x_range(),
                header.response.rect.bottom() - 1.0,
                Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            );
        }

        if let Some(path) = header.inner {
            panel.navigate_to(path);
            interacted = true;
        }

        // Cabeçalho de colunas (apenas no modo lista).
        if panel.view_mode == ViewMode::List {
            let (column_rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), COLUMN_HEADER_H), Sense::hover());
            paint_columns(ui, column_rect, &visuals, active);
        }

        if let Some(error) = &panel.error {
            draw_error(ui, error, &visuals);
            return;
        }

        if panel.entries.is_empty() {
            draw_empty(ui, &visuals);
            return;
        }

        let scroll = ScrollArea::vertical()
            .id_salt(("list", side))
            .auto_shrink([false, false]);
        let scroll = if let Some(offset) = panel.pending_scroll.take() {
            scroll.vertical_scroll_offset(offset)
        } else {
            scroll
        };

        let focused = panel.focus;
        let active_selection = panel.multi.clone();
        let mut action = None;

        let output = match panel.view_mode {
            ViewMode::List => {
                let entries = &panel.entries;
                let selection = &active_selection;
                scroll.show_rows(ui, ROW_H, entries.len(), |ui, range| {
                    for index in range {
                        let entry = &entries[index];
                        let selected = selection.contains(&index);
                        let style = RowStyle {
                            selected,
                            focused: focused == index,
                            active,
                            accent,
                            dim: !active,
                        };
                        if let Some(row_action) =
                            draw_row(ui, entry, index, &style, &visuals, side, tags)
                        {
                            action = Some(row_action);
                        }
                    }
                })
            }
            ViewMode::Icons => {
                let width = ui.available_width();
                let cols = (((width + CELL_GAP) / (CELL_W + CELL_GAP)).floor() as usize).max(1);
                panel.grid_cols = cols;
                let rows = panel.entries.len().div_ceil(cols);
                let entries = &panel.entries;
                let selection = &active_selection;
                scroll.show_rows(ui, GRID_ROW_H, rows, |ui, range| {
                    for row in range {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = CELL_GAP;
                            for column in 0..cols {
                                let index = row * cols + column;
                                let Some(entry) = entries.get(index) else {
                                    break;
                                };
                                let selected = selection.contains(&index);
                                let style = RowStyle {
                                    selected,
                                    focused: focused == index,
                                    active,
                                    accent,
                                    dim: !active,
                                };
                                if let Some(row_action) =
                                    draw_grid_cell(ui, entry, index, &style, &visuals, side)
                                {
                                    action = Some(row_action);
                                }
                            }
                        });
                    }
                })
            }
        };

        panel.scroll_offset = output.state.clamped_offset().y;
        panel.viewport_h = output.inner_rect.height();

        match action {
            Some(RowAction::Click { index, ctrl }) => {
                panel.select(index, ctrl);
                interacted = true;
            }
            Some(RowAction::Open { index }) => {
                panel.open(index);
                interacted = true;
            }
            None => {}
        }
    });

    let stroke = if active {
        Stroke::new(1.0, accent)
    } else {
        visuals.widgets.noninteractive.bg_stroke
    };
    ui.painter()
        .rect_stroke(rect, 0.0, stroke, StrokeKind::Inside);

    interacted
}

/// Pinta o cabeçalho das três colunas dentro de `rect`.
fn paint_columns(ui: &mut Ui, rect: Rect, visuals: &egui::Visuals, active: bool) {
    let left = rect.left() + 6.0;
    let right = rect.right() - 6.0;
    let modified_right = right;
    let size_right = modified_right - MODIFIED_COL_W;
    let center_y = rect.center().y;
    let small = FontId::proportional(12.5);
    let weak = secondary_color(visuals, active);

    ui.painter().text(
        pos2(left, center_y),
        Align2::LEFT_CENTER,
        rust_i18n::t!("column_name"),
        small.clone(),
        weak,
    );
    ui.painter().text(
        pos2(size_right, center_y),
        Align2::RIGHT_CENTER,
        rust_i18n::t!("column_size"),
        small.clone(),
        weak,
    );
    ui.painter().text(
        pos2(modified_right, center_y),
        Align2::RIGHT_CENTER,
        rust_i18n::t!("column_modified"),
        small,
        weak,
    );
}

/// Estilo visual de uma linha.
struct RowStyle {
    selected: bool,
    focused: bool,
    active: bool,
    accent: Color32,
    dim: bool,
}

/// Ação resultante de interação com uma linha.
enum RowAction {
    Click { index: usize, ctrl: bool },
    Open { index: usize },
}

/// Desenha uma linha da lista, com fundo de seleção e foco.
fn draw_row(
    ui: &mut Ui,
    entry: &Entry,
    index: usize,
    style: &RowStyle,
    visuals: &egui::Visuals,
    side: PanelSide,
    tags: &HashMap<String, Vec<String>>,
) -> Option<RowAction> {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
    let id = ui.make_persistent_id(("entry", side, &entry.path));
    let response = ui.interact(rect, id, Sense::CLICK);

    let left = rect.left() + 6.0;
    let right = rect.right() - 6.0;
    let modified_right = right;
    let size_right = modified_right - MODIFIED_COL_W;
    let mut name_right = size_right - SIZE_COL_W - 8.0;

    let text_color = visuals.text_color();
    let secondary = if style.dim {
        with_alpha(secondary_color(visuals, true), 153)
    } else {
        secondary_color(visuals, style.active)
    };

    {
        let painter = ui.painter();
        if style.selected {
            painter.rect_filled(rect, 0.0, with_alpha(style.accent, SELECTION_ALPHA));
        } else if response.hovered() {
            painter.rect_filled(rect, 0.0, visuals.widgets.hovered.weak_bg_fill);
        }

        let size_text = if entry.is_dir {
            String::from("-")
        } else {
            format_size(entry.size)
        };
        painter.text(
            pos2(size_right, rect.center().y),
            Align2::RIGHT_CENTER,
            size_text,
            FontId::proportional(12.5),
            secondary,
        );
        painter.text(
            pos2(modified_right, rect.center().y),
            Align2::RIGHT_CENTER,
            format_modified(entry.modified),
            FontId::proportional(12.5),
            secondary,
        );

        if style.focused && style.active {
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(1.0, style.accent),
                StrokeKind::Inside,
            );
        }
    }

    if let Some(entry_tags) = tags.get(entry.path.to_string_lossy().as_ref()) {
        name_right = draw_tag_chips(
            ui,
            left,
            name_right,
            rect.top(),
            rect.bottom(),
            entry_tags,
            visuals,
        );
    }

    let name_rect = Rect::from_min_max(
        pos2(left, rect.top()),
        pos2(name_right.max(left + 1.0), rect.bottom()),
    );
    let mut name_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(name_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let name = if entry.is_dir {
        format!("{}/", entry.name)
    } else {
        entry.name.clone()
    };
    let mut text = RichText::new(name).size(14.0).color(text_color);
    if entry.is_dir {
        text = text.strong();
    }
    // `selectable(false)` impede que o clique no texto seja consumido pelo
    // Label: quem trata o clique é a linha inteira (`response`).
    name_ui.add(Label::new(text).truncate().selectable(false));

    if response.double_clicked() {
        return Some(RowAction::Open { index });
    }
    if response.clicked() {
        let ctrl = response.ctx.input(|input| input.modifiers.ctrl);
        return Some(RowAction::Click { index, ctrl });
    }
    None
}

/// Desenha uma célula da grade (modo ícones), com seleção e foco.
fn draw_grid_cell(
    ui: &mut Ui,
    entry: &Entry,
    index: usize,
    style: &RowStyle,
    visuals: &egui::Visuals,
    side: PanelSide,
) -> Option<RowAction> {
    let (rect, _) = ui.allocate_exact_size(vec2(CELL_W, GRID_ROW_H), Sense::hover());
    let id = ui.make_persistent_id(("grid", side, &entry.path));
    let response = ui.interact(rect, id, Sense::CLICK);

    if style.selected {
        ui.painter()
            .rect_filled(rect, 4.0, with_alpha(style.accent, SELECTION_ALPHA));
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }

    let text_color = if style.dim {
        with_alpha(visuals.text_color(), 153)
    } else {
        visuals.text_color()
    };
    // O accent vem do tema (aplicado em `apply_theme`); nada hardcoded.
    let icon_color = if entry.is_dir {
        visuals.selection.stroke.color
    } else {
        text_color
    };
    let icon = if entry.is_dir { "🗀" } else { "🗋" };
    ui.painter().text(
        pos2(rect.center().x, rect.top() + ICON_SIZE * 0.5 + 8.0),
        Align2::CENTER_CENTER,
        icon,
        FontId::proportional(ICON_SIZE),
        icon_color,
    );

    let name_rect = Rect::from_min_max(
        pos2(rect.left() + 4.0, rect.top() + ICON_SIZE + 12.0),
        pos2(
            rect.right() - 4.0,
            rect.top() + ICON_SIZE + 12.0 + GRID_NAME_H,
        ),
    );
    let mut name_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(name_rect)
            .layout(Layout::top_down(Align::Center)),
    );
    let name = if entry.is_dir {
        format!("{}/", entry.name)
    } else {
        entry.name.clone()
    };
    let display = compact_grid_name(&name);
    // `wrap()` quebra em até duas linhas; a área de recorte limita o excesso.
    name_ui.add(
        Label::new(RichText::new(display).size(12.0).color(text_color))
            .wrap()
            .selectable(false),
    );

    if style.focused && style.active {
        ui.painter().rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0, style.accent),
            StrokeKind::Inside,
        );
    }

    if response.double_clicked() {
        return Some(RowAction::Open { index });
    }
    if response.clicked() {
        let ctrl = response.ctx.input(|input| input.modifiers.ctrl);
        return Some(RowAction::Click { index, ctrl });
    }
    None
}

/// Encurta um nome longo para a célula da grade com elipse no meio.
///
/// O limite de 26 caracteres corresponde a duas linhas de ~13 caracteres na
/// largura de uma célula; nomes menores passam intactos.
fn compact_grid_name(name: &str) -> String {
    const MAX_CHARS: usize = 26;
    let count = name.chars().count();
    if count <= MAX_CHARS {
        return name.to_owned();
    }
    let keep = MAX_CHARS.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let start: String = name.chars().take(head).collect();
    let end: String = name.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

/// Desenha chips de tags antes das colunas de Tamanho/Modificado.
///
/// Mostra no máximo três tags e um `+N` para o restante. Devolve a borda
/// direita disponível para o nome; se não houver largura suficiente, não
/// desenha nada e devolve `name_right` inalterado. As cores vêm de
/// `visuals` (fundo `faint_bg_color`, texto fraco), como o resto da UI.
fn draw_tag_chips(
    ui: &mut Ui,
    left: f32,
    name_right: f32,
    top: f32,
    bottom: f32,
    tags: &[String],
    visuals: &egui::Visuals,
) -> f32 {
    const MAX_CHIPS: usize = 3;
    const CHIP_H: f32 = 14.0;
    const CHIP_PAD: f32 = 5.0;
    const CHIP_GAP: f32 = 4.0;
    const MIN_NAME_W: f32 = 80.0;

    let mut labels: Vec<String> = tags.iter().take(MAX_CHIPS).cloned().collect();
    if tags.len() > MAX_CHIPS {
        labels.push(format!("+{}", tags.len() - MAX_CHIPS));
    }
    if labels.is_empty() {
        return name_right;
    }

    let font = FontId::proportional(11.0);
    let color = visuals.weak_text_color();
    let widths: Vec<f32> = labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap(label.clone(), font.clone(), color)
                .size()
                .x
        })
        .collect();
    let total = widths
        .iter()
        .map(|width| width + 2.0 * CHIP_PAD)
        .sum::<f32>()
        + CHIP_GAP * labels.len().saturating_sub(1) as f32;

    if name_right - total - 6.0 - left < MIN_NAME_W {
        return name_right;
    }

    let mut x = name_right - total;
    let center_y = (top + bottom) / 2.0;
    {
        let painter = ui.painter();
        for (label, width) in labels.iter().zip(widths.iter()) {
            let chip = Rect::from_min_size(
                pos2(x, center_y - CHIP_H / 2.0),
                vec2(width + 2.0 * CHIP_PAD, CHIP_H),
            );
            painter.rect_filled(chip, CHIP_H / 2.0, visuals.faint_bg_color);
            painter.text(
                chip.center(),
                Align2::CENTER_CENTER,
                label,
                font.clone(),
                color,
            );
            x += width + 2.0 * CHIP_PAD + CHIP_GAP;
        }
    }

    name_right - total - 6.0
}

/// Card de metadados usado por diretórios, binários e erros de leitura.
fn draw_preview_metadata(ui: &mut Ui, entry: &Entry) {
    ui.add_space(8.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(&entry.name).size(15.0).strong());
        ui.add_space(4.0);
        let weak = ui.visuals().weak_text_color();
        let size = if entry.is_dir {
            String::from("—")
        } else {
            format_size(entry.size)
        };
        ui.label(
            RichText::new(format!("{}: {size}", rust_i18n::t!("preview_size")))
                .size(12.5)
                .color(weak),
        );
        ui.label(
            RichText::new(format!(
                "{}: {}",
                rust_i18n::t!("preview_modified"),
                format_modified(entry.modified)
            ))
            .size(12.5)
            .color(weak),
        );
    });
}

/// Mensagem centralizada (carregando / nenhum item focado).
/// Símbolo curto do estado de um arquivo no painel Git.
fn status_glyph(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Modified => "M",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
    }
}

fn draw_preview_centered(
    ui: &mut Ui,
    body: Rect,
    text: impl Into<String>,
    visuals: &egui::Visuals,
) {
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(body)
            .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
        |ui| {
            ui.label(RichText::new(text.into()).color(visuals.weak_text_color()));
        },
    );
}

fn preview_reason(reason: UnsupportedReason) -> String {
    let key = match reason {
        UnsupportedReason::Directory => "preview_reason_directory",
        UnsupportedReason::Binary => "preview_reason_binary",
        UnsupportedReason::TooLarge => "preview_reason_too_large",
        UnsupportedReason::Pdf => "preview_reason_pdf",
    };
    rust_i18n::t!(key).to_string()
}

/// Divide uma linha CSV/TSV em campos, tratando aspas duplas básicas.
fn parse_csv_row(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
        } else if ch == '"' && field.is_empty() {
            quoted = true;
        } else if ch == delimiter {
            fields.push(std::mem::take(&mut field));
        } else {
            field.push(ch);
        }
    }
    fields.push(field);
    fields
}

/// Constrói o `LayoutJob` com syntax highlight (ou monospace puro).
///
/// Limitado a [`MAX_HIGHLIGHT_LINES`] para manter o frame barato.
fn build_text_job(text: &str, syntax_name: Option<&str>, dark: bool) -> egui::text::LayoutJob {
    let base = TextFormat {
        font_id: FontId::monospace(12.0),
        color: if dark {
            Color32::from_rgb(0xd4, 0xd4, 0xd4)
        } else {
            Color32::from_rgb(0x24, 0x24, 0x24)
        },
        ..Default::default()
    };

    let syntaxes = syntax_set();
    let syntax = syntax_name.and_then(|name| syntaxes.find_syntax_by_extension(name));
    let Some(syntax) = syntax else {
        return plain_text_job(text, base);
    };

    let themes = theme_set();
    let theme_name = if dark {
        "base16-ocean.dark"
    } else {
        "InspiredGitHub"
    };
    let Some(theme) = themes.themes.get(theme_name) else {
        return plain_text_job(text, base);
    };

    let mut job = egui::text::LayoutJob::default();
    let mut highlighter = HighlightLines::new(syntax, theme);
    for line in LinesWithEndings::from(text).take(MAX_HIGHLIGHT_LINES) {
        match highlighter.highlight_line(line, syntaxes) {
            Ok(ranges) => {
                for (style, piece) in ranges {
                    job.append(
                        piece,
                        0.0,
                        TextFormat {
                            font_id: FontId::monospace(12.0),
                            color: Color32::from_rgb(
                                style.foreground.r,
                                style.foreground.g,
                                style.foreground.b,
                            ),
                            italics: style.font_style.contains(FontStyle::ITALIC),
                            ..Default::default()
                        },
                    );
                }
            }
            Err(_) => job.append(line, 0.0, base.clone()),
        }
    }
    job
}

fn plain_text_job(text: &str, format: TextFormat) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for line in text.lines().take(MAX_HIGHLIGHT_LINES) {
        job.append(line, 0.0, format.clone());
        job.append("\n", 0.0, format.clone());
    }
    job
}

fn draw_empty(ui: &mut Ui, visuals: &egui::Visuals) {
    let body = ui.available_rect_before_wrap();
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((body.height() / 2.0 - 48.0).max(8.0));
            ui.label(RichText::new("🗀").size(48.0));
            ui.add_space(4.0);
            ui.label(RichText::new(rust_i18n::t!("empty_dir")).color(visuals.weak_text_color()));
        });
    });
}

fn draw_error(ui: &mut Ui, error: &CoreError, visuals: &egui::Visuals) {
    let body = ui.available_rect_before_wrap();
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((body.height() / 2.0 - 40.0).max(8.0));
            ui.label(
                RichText::new(rust_i18n::t!("error_dir_title"))
                    .size(16.0)
                    .strong()
                    .color(visuals.error_fg_color),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(error.to_string()).color(visuals.weak_text_color()));
        });
    });
}

/// Cor accent configurada, com fallback para a cor de destaque do tema.
fn accent_color(theme: &Theme, visuals: &egui::Visuals) -> Color32 {
    parse_hex_color(&theme.accent).unwrap_or(visuals.selection.bg_fill)
}

/// Texto secundário, atenuado a 60% quando o painel está inativo.
fn secondary_color(visuals: &egui::Visuals, active: bool) -> Color32 {
    let color = visuals.weak_text_color();
    if active {
        color
    } else {
        with_alpha(color, 153)
    }
}

fn breadcrumb_color(active: bool, accent: Color32) -> Color32 {
    if active {
        accent
    } else {
        with_alpha(accent, 153)
    }
}

/// Segmentos visíveis do breadcrumb, evitando estourar a largura mínima.
fn visible_breadcrumb(ancestors: &[PathBuf]) -> Vec<PathBuf> {
    const MAX_SEGMENTS: usize = 4;
    if ancestors.len() <= MAX_SEGMENTS {
        return ancestors.to_vec();
    }
    let mut visible = Vec::with_capacity(MAX_SEGMENTS);
    visible.push(ancestors[0].clone());
    visible.extend_from_slice(&ancestors[ancestors.len() - (MAX_SEGMENTS - 1)..]);
    visible
}

fn breadcrumb_label(path: &Path) -> String {
    let raw = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => name.to_owned(),
        None => path.display().to_string(),
    };
    const MAX_CHARS: usize = 20;
    if raw.chars().count() <= MAX_CHARS {
        raw
    } else {
        let mut short: String = raw.chars().take(MAX_CHARS - 3).collect();
        short.push_str("...");
        short
    }
}

/// Formata `SystemTime` como `YYYY-MM-DD HH:MM` em UTC (locale-independente).
fn format_modified(time: SystemTime) -> String {
    let Ok(duration) = time.duration_since(UNIX_EPOCH) else {
        return String::from("-");
    };
    let secs = duration.as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Converte dias desde 1970-01-01 em (ano, mês, dia) — algoritmo de Hinnant.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Substitui apenas o alpha, preservando RGB.
fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// Mistura `from` em direção a `to` por `amount` (0..=1).
fn mix_color(from: Color32, to: Color32, amount: f32) -> Color32 {
    let t = amount.clamp(0.0, 1.0);
    let blend = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        blend(from.r(), to.r()),
        blend(from.g(), to.g()),
        blend(from.b(), to.b()),
    )
}

/// Converte um `Key` do egui no nome usado pelo keymap.
fn key_name(key: Key) -> String {
    match key {
        Key::Num0 => "0".to_owned(),
        Key::Num1 => "1".to_owned(),
        Key::Num2 => "2".to_owned(),
        Key::Num3 => "3".to_owned(),
        Key::Num4 => "4".to_owned(),
        Key::Num5 => "5".to_owned(),
        Key::Num6 => "6".to_owned(),
        Key::Num7 => "7".to_owned(),
        Key::Num8 => "8".to_owned(),
        Key::Num9 => "9".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Diretório home do usuário, com fallback para o diretório atual.
fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Descrição i18n curta de uma ação (usada nos hints e no cheatsheet).
fn action_description(action: Action) -> String {
    let key = match action {
        Action::TogglePreview => "action_toggle_preview",
        Action::ToggleGitPanel => "action_toggle_git_panel",
        Action::TogglePanel => "action_toggle_panel",
        Action::NavigateUp => "action_navigate_up",
        Action::NavigateDown => "action_navigate_down",
        Action::Enter => "action_enter",
        Action::GoBack => "action_go_back",
        Action::Copy => "action_copy",
        Action::Move => "action_move",
        Action::Rename => "action_rename",
        Action::Delete => "action_delete",
        Action::Undo => "action_undo",
        Action::CycleTheme => "action_cycle_theme",
        Action::Quit => "action_quit",
        Action::ShowHints => "action_show_hints",
        Action::TagFiles => "action_tag_files",
        Action::Checksums => "action_checksums",
        Action::BatchRename => "action_batch_rename",
        Action::Compress => "action_compress",
        Action::Extract => "action_extract",
        Action::ToggleViewMode => "action_toggle_view_mode",
        Action::ToggleQuickOpen => "action_toggle_quick_open",
        Action::ToggleGitLog => "action_toggle_git_log",
        Action::OpenWith => "action_open_with",
        Action::OpenWithDefault => "action_open_with_default",
    };
    rust_i18n::t!(key).into_owned()
}

/// Calcula os novos nomes de uma seleção para pré-visualização.
///
/// Devolve `(caminho, nome atual, novo nome)` na ordem recebida.
fn batch_renames(paths: &[PathBuf], pattern: &str) -> Vec<(PathBuf, String, String)> {
    let total = paths.len();
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let current = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = path
                .extension()
                .map(|ext| format!(".{}", ext.to_string_lossy()))
                .unwrap_or_default();
            let new_name = apply_pattern(pattern, &name, &ext, index + 1, total);
            (path.clone(), current, new_name)
        })
        .collect()
}

/// Valida os novos nomes antes de executar: vazio, fora do diretório,
/// duplicado ou conflitante com um arquivo existente.
fn validate_batch_renames(renames: &[(PathBuf, String, String)]) -> Option<String> {
    let mut seen = BTreeSet::new();
    for (path, current, new_name) in renames {
        let new_name = new_name.trim();
        if new_name.is_empty() {
            return Some(rust_i18n::t!("batch_rename_empty").to_string());
        }
        let mut components = Path::new(new_name).components();
        let valid = matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none();
        if !valid {
            return Some(rust_i18n::t!("batch_rename_invalid").to_string());
        }
        if new_name != current
            && let Some(parent) = path.parent()
            && parent.join(new_name).exists()
        {
            return Some(rust_i18n::t!("batch_rename_exists", name = new_name).to_string());
        }
        if !seen.insert(new_name.to_owned()) {
            return Some(rust_i18n::t!("batch_rename_duplicate", name = new_name).to_string());
        }
    }
    None
}

/// Constrói as invocações de um comando de `[open_with]`.
///
/// O comando é dividido em argv por espaços em branco (não há shell, então
/// metacaracteres como `|`, `>`, `*` e variáveis não são interpretados).
/// `{path}` é substituído por um arquivo, gerando uma invocação por caminho;
/// `{paths}` é substituído por todos os caminhos na mesma invocação. Sem
/// placeholder, os caminhos são anexados ao final dos argumentos.
fn build_open_invocations(
    command: &str,
    paths: &[PathBuf],
) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut tokens = command.split_whitespace().map(str::to_owned);
    let program = tokens
        .next()
        .ok_or_else(|| rust_i18n::t!("open_with_empty_command").to_string())?;
    let args: Vec<String> = tokens.collect();

    if args.iter().any(|arg| arg.contains("{paths}")) {
        let joined = paths
            .iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let mut expanded = Vec::new();
        for arg in &args {
            if arg == "{paths}" {
                expanded.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
            } else {
                expanded.push(arg.replace("{paths}", &joined));
            }
        }
        return Ok(vec![(program, expanded)]);
    }

    if args.iter().any(|arg| arg.contains("{path}")) {
        let mut invocations = Vec::with_capacity(paths.len());
        for path in paths {
            let path_text = path.to_string_lossy();
            let expanded = args
                .iter()
                .map(|arg| arg.replace("{path}", &path_text))
                .collect();
            invocations.push((program.clone(), expanded));
        }
        return Ok(invocations);
    }

    let mut expanded = args;
    expanded.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
    Ok(vec![(program, expanded)])
}

/// Extrai o nome-base de um arquivo de compressão, removendo a extensão.
fn archive_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let stem = [".tar.gz", ".tgz", ".zip", ".tar"]
        .iter()
        .find_map(|suffix| {
            lower
                .strip_suffix(suffix)
                .map(|prefix| &name[..prefix.len()])
        })
        .unwrap_or(&name);
    if stem.is_empty() {
        "extracted".to_owned()
    } else {
        stem.to_owned()
    }
}

/// Monta o aviso de conclusão/falha de um trabalho de arquivo.
fn archive_notice(destination: &Path, result: Result<(), String>, compress: bool) -> Notice {
    match result {
        Ok(()) => {
            let name = destination
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| destination.display().to_string());
            let text = if compress {
                rust_i18n::t!("compress_done", name = name)
            } else {
                rust_i18n::t!("extract_done", name = name)
            };
            Notice::info(text.to_string())
        }
        Err(error) => {
            let text = if compress {
                rust_i18n::t!("compress_failed", error = error)
            } else {
                rust_i18n::t!("extract_failed", error = error)
            };
            Notice::error(text.to_string())
        }
    }
}

/// Corta um texto longo para exibição, sinalizando com `…`.
fn truncate_for_display(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut output: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        output.push('…');
    }
    output
}

/// Linha `Rótulo: hash [Copiar]` em fonte monoespaçada.
fn hash_row(ui: &mut Ui, label: &str, hash: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{label}:")).color(ui.visuals().weak_text_color()));
        ui.add(Label::new(RichText::new(hash).text_style(TextStyle::Monospace)).truncate());
        if ui.button(rust_i18n::t!("checksums_copy")).clicked() {
            ui.ctx().copy_text(hash.to_owned());
        }
    });
}

/// Teclas pressionadas neste frame, no vocabulário do [`Keymap`].
fn pressed_keys(ctx: &egui::Context) -> Vec<KeyBinding> {
    ctx.input(|input| {
        input
            .events
            .iter()
            .filter_map(|event| {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                {
                    Some(KeyBinding {
                        mods: KeyModifiers {
                            ctrl: modifiers.ctrl,
                            alt: modifiers.alt,
                            shift: modifiers.shift,
                            logo: modifiers.mac_cmd,
                        },
                        key: key_name(*key),
                    })
                } else {
                    None
                }
            })
            .collect()
    })
}

/// Interpreta `#rrggbb` (o `#` é opcional).
fn parse_hex_color(raw: &str) -> Option<Color32> {
    let hex = raw.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let red = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let green = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let blue = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color32::from_rgb(red, green, blue))
}

/// Resposta de um clique de linha para o teste de fumaça: expõe a contagem
/// selecionada do painel ativo.
impl AranduApp {
    /// Painel ativo (esquerda/direita) — usado em testes.
    #[cfg(test)]
    fn active_side(&self) -> PanelSide {
        self.active
    }

    /// Contagem de selecionados do painel ativo — usado em testes.
    #[cfg(test)]
    fn active_selected_count(&self) -> usize {
        self.active_panel().selected_count()
    }

    /// Se o painel de preview está aberto — usado em testes.
    #[cfg(test)]
    fn preview_visible(&self) -> bool {
        self.preview.visible
    }

    /// Tags de um caminho — usado em testes.
    #[cfg(test)]
    fn tags_of(&self, path: &Path) -> Vec<String> {
        self.tags.tags_of(path)
    }

    /// Chave do item focado quando ele é um arquivo elegível a preview.
    #[cfg(test)]
    fn focused_preview_key(&self) -> Option<PreviewKey> {
        let panel = self.active_panel();
        panel
            .entries
            .get(panel.focus)
            .filter(|entry| !entry.is_dir)
            .map(PreviewKey::for_entry)
    }

    /// Se o preview do item focado já terminou de carregar — usado em testes.
    #[cfg(test)]
    fn preview_ready(&self) -> bool {
        self.focused_preview_key()
            .and_then(|key| self.preview.cache.get(&key))
            .is_some()
    }

    /// Texto do preview focado, quando for texto — usado em testes.
    #[cfg(test)]
    fn preview_text(&self) -> Option<String> {
        let key = self.focused_preview_key()?;
        match self.preview.cache.get(&key)? {
            PreviewEntry::Ready(content) => match &*content {
                PreviewContent::Text { text, .. } => Some(text.clone()),
                _ => None,
            },
            PreviewEntry::Failed(_) => None,
        }
    }

    /// Markdown bruto do preview focado — usado em testes.
    #[cfg(test)]
    fn preview_markdown(&self) -> Option<String> {
        let key = self.focused_preview_key()?;
        match self.preview.cache.get(&key)? {
            PreviewEntry::Ready(content) => match &*content {
                PreviewContent::Markdown { text, .. } => Some(text.clone()),
                _ => None,
            },
            PreviewEntry::Failed(_) => None,
        }
    }

    /// Se o cálculo de checksums terminou — usado em testes.
    #[cfg(test)]
    fn checksums_done(&self) -> bool {
        self.checksums
            .as_ref()
            .is_some_and(|dialog| dialog.receiver.is_none())
    }

    /// Hashes calculados, na ordem dos alvos — usado em testes.
    #[cfg(test)]
    fn checksums_results(&self) -> Vec<(String, String)> {
        self.checksums
            .as_ref()
            .map(|dialog| {
                dialog
                    .results
                    .iter()
                    .filter_map(|result| match result {
                        Some(Ok(hashes)) => Some((hashes.sha256.clone(), hashes.md5.clone())),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Injeta uma operação em andamento para testar a barra de progresso sem
    /// depender do escalonamento da thread da fila.
    #[cfg(test)]
    fn inject_running_op(&mut self, kind: OpKind, name: &str, progress: Option<Progress>) {
        self.injected_ops = true;
        self.ops.current = Some(CurrentOp {
            kind,
            name: name.to_owned(),
            progress,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use arandu_config::Config;
    use arandu_core::ops::{OpKind, Progress};
    use arandu_index::IndexEntry;
    use eframe::egui;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::{
        AranduApp, GitTab, IndexScanStatus, OpenExecutor, PanelSide, ViewMode,
        build_open_invocations, format_modified, parse_hex_color, visible_breadcrumb,
    };
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, UNIX_EPOCH};

    /// Comando registrado pelo executor de teste: programa + argv.
    type RecordedCommand = (String, Vec<String>);

    /// Executor de teste: registra as chamadas sem lançar processos.
    #[derive(Clone, Default)]
    struct RecordingOpenExecutor {
        default_calls: Arc<Mutex<Vec<PathBuf>>>,
        commands: Arc<Mutex<Vec<RecordedCommand>>>,
    }

    impl OpenExecutor for RecordingOpenExecutor {
        fn open_default(&self, path: &Path) -> Result<(), String> {
            self.default_calls.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }

        fn run_command(&self, program: &str, args: &[String]) -> Result<(), String> {
            self.commands
                .lock()
                .unwrap()
                .push((program.to_owned(), args.to_vec()));
            Ok(())
        }
    }

    /// Executa `git` no diretório com configuração global isolada.
    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("executa git");
        assert!(
            output.status.success(),
            "git {:?} falhou: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Inicializa um repositório com identidade fixa.
    fn init_git_repo(dir: &Path) {
        run_git(dir, &["init", "-b", "main"]);
        run_git(dir, &["config", "user.name", "Teste"]);
        run_git(dir, &["config", "user.email", "teste@example.com"]);
    }

    fn fixture_app() -> (tempfile::TempDir, AranduApp) {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        fs::write(dir.path().join("fixture-file.txt"), b"conteudo").expect("cria arquivo");
        fs::create_dir(dir.path().join("fixture-dir")).expect("cria diretório");

        let app = AranduApp::new_at(Config::default(), dir.path());
        (dir, app)
    }

    #[test]
    fn renders_fixture_directory_without_panic() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);

        harness.run();

        let found = harness.query_all_by_label("fixture-file.txt").count();
        assert!(
            found >= 1,
            "esperava o label 'fixture-file.txt' na árvore de acessibilidade, encontrou {found}"
        );

        let dirs = harness.query_all_by_label("fixture-dir/").count();
        assert!(dirs >= 1, "esperava o diretório com sufixo '/'");
    }

    #[test]
    fn clicking_row_selects_entry() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let node = harness
            .query_all_by_label("fixture-file.txt")
            .next()
            .expect("linha do fixture na árvore de acessibilidade");
        node.click();
        harness.run();

        assert_eq!(harness.state().active_selected_count(), 1);
        assert!(harness.state().left.selected.is_some());
    }

    #[test]
    fn ctrl_g_toggles_git_panel_and_detects_non_repository() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        assert!(!harness.state().git.visible);
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::G);
        harness.run_steps(1);
        assert!(harness.state().git.visible);

        // O refresh em background mantém repaint contínuo, então `run()` (que
        // espera a UI assentar) estouraria max_steps. Avança passos com teto
        // até a descoberta terminar.
        for _ in 0..200 {
            if harness.state().git.not_repo {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        assert!(
            harness.state().git.not_repo,
            "esperava estado de não-repositório"
        );

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::G);
        harness.run_steps(1);
        assert!(!harness.state().git.visible);
    }

    #[test]
    fn git_panel_enter_stages_and_unstages_focused_row() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let run_git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .expect("executa git");
            assert!(
                output.status.success(),
                "git {:?} falhou: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&["init", "-b", "main"]);
        run_git(&["config", "user.name", "Teste"]);
        run_git(&["config", "user.email", "teste@example.com"]);
        fs::write(dir.path().join("novo.txt"), b"conteudo").expect("cria arquivo");

        let app = AranduApp::new_at(Config::default(), dir.path());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::G);
        harness.run_steps(1);

        // O refresh em background mantém repaint contínuo, então `run()` (que
        // espera a UI assentar) estouraria max_steps. Avança passos com teto
        // até o status esperado aparecer.
        for _ in 0..200 {
            if harness.state().git.status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        assert!(harness.state().git.status.is_some(), "status não carregou");

        harness.state_mut().git.focused = true;
        harness.run_steps(1);
        harness.key_press(egui::Key::Enter);
        harness.run_steps(1);

        for _ in 0..200 {
            let staged = harness
                .state()
                .git
                .status
                .as_ref()
                .is_some_and(|status| status.staged.len() == 1 && status.untracked.is_empty());
            if staged {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        let status = harness.state().git.status.clone().expect("status");
        assert_eq!(status.staged.len(), 1, "Enter deveria preparar o arquivo");
        assert!(status.untracked.is_empty());

        harness.state_mut().git.focused = true;
        harness.run_steps(1);
        harness.key_press(egui::Key::Enter);
        harness.run_steps(1);

        for _ in 0..200 {
            let unstaged = harness
                .state()
                .git
                .status
                .as_ref()
                .is_some_and(|status| status.staged.is_empty() && status.untracked.len() == 1);
            if unstaged {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        let status = harness.state().git.status.clone().expect("status");
        assert!(
            status.staged.is_empty(),
            "Enter de novo deveria despreparar o arquivo"
        );
        assert_eq!(status.untracked.len(), 1);
    }

    #[test]
    fn git_panel_tabs_cycle_with_shift_tab() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::G);
        harness.run_steps(1);
        harness.state_mut().git.focused = true;
        assert_eq!(harness.state().git.tab, GitTab::Status);

        for expected in [
            GitTab::Log,
            GitTab::Blame,
            GitTab::Branches,
            GitTab::Stashes,
            GitTab::Status,
        ] {
            harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
            harness.run_steps(1);
            assert_eq!(harness.state().git.tab, expected);
        }
    }

    #[test]
    fn git_log_lists_commits_in_graph_tab() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        init_git_repo(dir.path());
        fs::write(dir.path().join("a.txt"), b"a").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "primeiro"]);
        fs::write(dir.path().join("b.txt"), b"b").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "segundo"]);

        let app = AranduApp::new_at(Config::default(), dir.path());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let modifiers = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        harness.key_press_modifiers(modifiers, egui::Key::G);
        harness.run_steps(1);

        for _ in 0..200 {
            if harness.state().git.log.len() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }

        let git = &harness.state().git;
        assert_eq!(git.tab, GitTab::Log, "Ctrl+Shift+G deveria abrir a aba Log");
        assert_eq!(git.log.len(), 2, "log deveria listar os dois commits");
        assert_eq!(git.log[0].message, "segundo");
        assert_eq!(git.log_graph.len(), git.log.len());

        let commit = &git.log[0];
        let graph = &git.log_graph[0];
        let expected = format!(
            "{graph:<6} {} {:<44} {:<12} {}",
            commit.id, commit.message, commit.author, commit.time
        );
        assert!(
            harness.query_all_by_label(&expected).count() >= 1,
            "linha do log não apareceu na árvore de acessibilidade: {expected:?}"
        );
    }

    /// Abre o painel Git e espera o primeiro status assíncrono.
    fn open_git_panel(harness: &mut Harness<'_, AranduApp>) {
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::G);
        harness.run_steps(1);
        for _ in 0..200 {
            if harness.state().git.status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        assert!(harness.state().git.status.is_some(), "status não carregou");
    }

    #[test]
    fn git_branches_tab_lists_current_branch() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        init_git_repo(dir.path());
        fs::write(dir.path().join("a.txt"), b"a").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "inicial"]);
        run_git(dir.path(), &["branch", "feature"]);

        let app = AranduApp::new_at(Config::default(), dir.path());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        open_git_panel(&mut harness);

        harness.state_mut().git.tab = GitTab::Branches;
        harness.state_mut().git.focus = 0;
        for _ in 0..200 {
            if harness.state().git.branches.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }

        let git = &harness.state().git;
        assert_eq!(git.tab, GitTab::Branches);
        assert_eq!(git.branches.len(), 2, "deveria listar main e feature");
        let main = git
            .branches
            .iter()
            .find(|branch| branch.name == "main")
            .expect("branch main");
        assert!(main.current);
        let text = format!("● {:<28} {}", main.name, main.commit);
        assert!(
            harness.query_all_by_label(&text).count() >= 1,
            "linha da branch não apareceu: {text:?}"
        );
    }

    #[test]
    fn git_create_branch_via_modal() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        init_git_repo(dir.path());
        fs::write(dir.path().join("a.txt"), b"a").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "inicial"]);

        let app = AranduApp::new_at(Config::default(), dir.path());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        open_git_panel(&mut harness);

        harness.state_mut().git.tab = GitTab::Branches;
        harness.state_mut().git.focused = true;
        for _ in 0..200 {
            if !harness.state().git.branches.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        assert!(!harness.state().git.branches.is_empty());

        harness.key_press(egui::Key::N);
        harness.run_steps(1);
        assert!(
            harness.state().git.branch_dialog.is_some(),
            "N deveria abrir o diálogo de nova branch"
        );

        harness
            .state_mut()
            .git
            .branch_dialog
            .as_mut()
            .expect("diálogo aberto")
            .name = "nova".to_string();
        harness.key_press(egui::Key::Enter);
        harness.run_steps(1);

        for _ in 0..200 {
            if harness
                .state()
                .git
                .branches
                .iter()
                .any(|branch| branch.name == "nova")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        assert!(harness.state().git.branch_dialog.is_none());
        assert!(
            harness
                .state()
                .git
                .branches
                .iter()
                .any(|branch| branch.name == "nova"),
            "a branch nova deveria aparecer na lista"
        );
    }

    #[test]
    fn git_stashes_tab_lists_entries() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        init_git_repo(dir.path());
        fs::write(dir.path().join("a.txt"), b"a").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "inicial"]);
        fs::write(dir.path().join("a.txt"), b"alterado").expect("altera arquivo");
        run_git(dir.path(), &["stash", "push", "-m", "teste de stash"]);

        let app = AranduApp::new_at(Config::default(), dir.path());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        open_git_panel(&mut harness);

        harness.state_mut().git.tab = GitTab::Stashes;
        harness.state_mut().git.focus = 0;
        for _ in 0..200 {
            if harness.state().git.stashes.len() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }

        let git = &harness.state().git;
        assert_eq!(git.tab, GitTab::Stashes);
        assert_eq!(git.stashes.len(), 1, "deveria listar um stash");
        let stash = &git.stashes[0];
        let text = format!("{} {:<40} {}", stash.id, stash.message, stash.time);
        assert!(
            harness.query_all_by_label(&text).count() >= 1,
            "linha do stash não apareceu: {text:?}"
        );
    }

    #[test]
    fn git_blame_shows_lines_of_focused_file() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        init_git_repo(dir.path());
        fs::write(dir.path().join("arquivo.txt"), "um\ndois\n").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "inicial"]);
        fs::write(dir.path().join("arquivo.txt"), "UM\ndois\n").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "altera primeira"]);

        let app = AranduApp::new_at(Config::default(), dir.path());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let focus = harness
            .state()
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "arquivo.txt")
            .expect("arquivo no painel");
        harness.state_mut().left.focus = focus;

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::G);
        harness.run_steps(1);
        harness.state_mut().git.tab = GitTab::Blame;
        harness.run_steps(1);

        for _ in 0..200 {
            if harness.state().git.blame.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }

        let blame = &harness.state().git.blame;
        assert_eq!(blame.len(), 2, "blame deveria anotar as duas linhas");
        assert_eq!(blame[0].content, "UM");
        assert_eq!(blame[1].content, "dois");
        assert_ne!(
            blame[0].commit, blame[1].commit,
            "linhas de commits diferentes devem ter hashes diferentes"
        );
        assert!(
            harness.query_all_by_label("dois").count() >= 1,
            "conteúdo do blame não apareceu na árvore de acessibilidade"
        );
    }

    #[test]
    fn git_blame_shows_hint_when_focused_item_is_directory() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        init_git_repo(dir.path());
        fs::create_dir(dir.path().join("subpasta")).expect("cria diretório");
        fs::write(dir.path().join("subpasta/a.txt"), b"a").expect("escreve arquivo");
        run_git(dir.path(), &["add", "."]);
        run_git(dir.path(), &["commit", "-m", "inicial"]);

        let app = AranduApp::new_at(Config::default(), dir.path());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let focus = harness
            .state()
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "subpasta")
            .expect("diretório no painel");
        harness.state_mut().left.focus = focus;

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::G);
        harness.run_steps(1);
        harness.state_mut().git.tab = GitTab::Blame;
        harness.run_steps(1);

        assert!(
            harness.state().git_blame_target().is_none(),
            "diretório não deve ter alvo de blame"
        );
        let hint = rust_i18n::t!("git_blame_need_file").into_owned();
        assert!(
            harness.query_all_by_label(&hint).count() >= 1,
            "hint de diretório sem blame não apareceu: {hint:?}"
        );
    }

    #[test]
    fn tab_switches_active_panel() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        assert_eq!(harness.state().active_side(), PanelSide::Left);
        harness.key_press(egui::Key::Tab);
        harness.run();
        assert_eq!(harness.state().active_side(), PanelSide::Right);
        harness.key_press(egui::Key::Tab);
        harness.run();
        assert_eq!(harness.state().active_side(), PanelSide::Left);
    }

    #[test]
    fn arrow_down_moves_focus_and_selects() {
        let (_dir, mut app) = fixture_app();
        app.left.select(0, false);
        app.left.move_focus(1);
        assert_eq!(app.left.focus, 1);
        assert_eq!(app.left.selected, Some(0));
        assert_eq!(app.active_selected_count(), 1);
    }

    #[test]
    fn ctrl_click_toggles_selection() {
        let (_dir, mut app) = fixture_app();
        app.left.select(0, false);
        app.left.select(1, true);
        assert_eq!(app.active_selected_count(), 2);
        app.left.select(1, true);
        assert_eq!(app.active_selected_count(), 1);
    }

    #[test]
    fn quick_open_activation_navigates_and_focuses_file() {
        let (dir, mut app) = fixture_app();
        let target = dir.path().join("fixture-file.txt");
        app.quick_open.store.update(vec![IndexEntry {
            path: target.clone(),
            name: "fixture-file.txt".to_owned(),
            is_dir: false,
            size: 8,
            modified: UNIX_EPOCH,
        }]);
        app.quick_open.open = true;
        app.quick_open.query = "fixture".to_owned();
        app.refresh_quick_open_results();

        assert_eq!(app.quick_open.results.len(), 1);
        app.activate_quick_open();

        assert!(!app.quick_open.open);
        assert_eq!(app.left.cwd, dir.path());
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.path == target)
            .expect("arquivo visível no painel");
        assert_eq!(app.left.focus, index);
        assert_eq!(app.left.selected, Some(index));
    }

    #[test]
    fn reopening_quick_open_reuses_ready_index() {
        let (_dir, mut app) = fixture_app();
        app.quick_open.status = IndexScanStatus::Ready;
        app.quick_open.last_scan = Some(Instant::now());
        app.quick_open.store.update(vec![IndexEntry {
            path: PathBuf::from("/tmp/a.txt"),
            name: "a.txt".to_owned(),
            is_dir: false,
            size: 0,
            modified: UNIX_EPOCH,
        }]);

        app.open_quick_open();

        assert!(app.quick_open.open);
        assert!(app.quick_open.receiver.is_none());
        assert_eq!(app.quick_open.store.len(), 1);
        assert!(app.quick_open.query.is_empty());
    }

    #[test]
    fn enter_opens_selected_directory_and_backspace_goes_up() {
        let (dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.is_dir)
            .expect("fixture tem um diretório");
        app.left.select(index, false);
        app.left.open_focused();
        assert_eq!(app.left.cwd, dir.path().join("fixture-dir"));

        app.left.go_up();
        assert_eq!(app.left.cwd, dir.path());
    }

    #[test]
    fn renders_error_state_for_missing_directory() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        let missing = dir.path().join("nao-existe");
        let app = AranduApp::new_at(Config::default(), missing);
        assert!(app.left.error.is_some());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let title = rust_i18n::t!("error_dir_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o título de erro '{title}'"
        );
    }

    #[test]
    fn formats_modified_as_sortable_utc() {
        let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(format_modified(time), "2023-11-14 22:13");
    }

    #[test]
    fn parses_theme_accent() {
        let color = parse_hex_color("#7aa2f7").expect("cor válida");
        assert_eq!((color.r(), color.g(), color.b()), (0x7a, 0xa2, 0xf7));
        assert!(parse_hex_color("nope").is_none());
    }

    #[test]
    fn breadcrumb_compacts_long_paths() {
        let ancestors: Vec<PathBuf> = (0..8).map(|i| PathBuf::from(format!("/seg{i}"))).collect();
        let visible = visible_breadcrumb(&ancestors);
        assert_eq!(visible.len(), 4);
        assert_eq!(visible[0], PathBuf::from("/seg0"));
        assert_eq!(visible[3], PathBuf::from("/seg7"));
    }

    #[test]
    fn delete_key_opens_trash_confirmation_modal() {
        let (_dir, mut app) = fixture_app();
        let file = app
            .left
            .entries
            .iter()
            .position(|entry| !entry.is_dir)
            .expect("fixture tem um arquivo");
        app.left.select(file, false);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::Delete);
        harness.run();

        let title = rust_i18n::t!("delete_confirm_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o modal de confirmação '{title}'"
        );
        let body = rust_i18n::t!("delete_confirm_body", name = "fixture-file.txt").to_string();
        assert!(
            harness.query_all_by_label(&body).count() >= 1,
            "esperava o aviso de lixeira não desfazível"
        );
        assert!(harness.state().delete_confirm.is_some());
    }

    #[test]
    fn progress_bar_appears_for_running_copy() {
        let (_dir, mut app) = fixture_app();
        app.inject_running_op(
            OpKind::Copy,
            "grande.bin",
            Some(Progress {
                current: PathBuf::from("/tmp/grande.bin"),
                bytes_done: 45,
                bytes_total: 100,
                files_done: 45,
                files_total: 100,
            }),
        );

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let cancel = rust_i18n::t!("cancel_operation").to_string();
        assert!(
            harness.query_all_by_label(&cancel).count() >= 1,
            "esperava o botão de cancelar na barra de operações"
        );
        assert!(
            harness.query_all_by_label("45%").count() >= 1,
            "esperava a ProgressBar em 45%"
        );
    }

    fn fixture_preview_app() -> (tempfile::TempDir, AranduApp) {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        fs::write(
            dir.path().join("main.rs"),
            b"fn main() {\n    println!(\"oi\");\n}\n",
        )
        .expect("cria main.rs");
        let mut app = AranduApp::new_at(Config::default(), dir.path());
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "main.rs")
            .expect("fixture tem main.rs");
        app.left.select(index, false);
        (dir, app)
    }

    #[test]
    fn f3_toggles_preview_and_loads_focused_text() {
        let (_dir, app) = fixture_preview_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        assert!(!harness.state().preview_visible());

        harness.key_press(egui::Key::F3);
        harness.run();
        assert!(harness.state().preview_visible());

        let title = rust_i18n::t!("preview_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o título '{title}' do painel de preview"
        );

        // A leitura acontece em `std::thread`; bombeamos frames até concluir.
        for _ in 0..200 {
            if harness.state().preview_ready() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run();
        }
        assert!(
            harness.state().preview_ready(),
            "o preview de texto deveria carregar em background"
        );
        let text = harness.state().preview_text().expect("preview de texto");
        assert!(text.contains("fn main"), "conteúdo inesperado: {text:?}");

        harness.key_press(egui::Key::F3);
        harness.run();
        assert!(!harness.state().preview_visible());
        assert_eq!(
            harness.query_all_by_label(&title).count(),
            0,
            "o painel de preview deveria sumir ao pressionar F3 novamente"
        );
    }

    #[test]
    fn parse_csv_row_handles_quotes_and_delimiter() {
        assert_eq!(
            super::parse_csv_row("a,\"b,c\",\"d\"\"e\"", ','),
            vec!["a", "b,c", "d\"e"]
        );
        assert_eq!(super::parse_csv_row("a\tb", '\t'), vec!["a", "b"]);
    }

    fn preview_app_for(name: &str, bytes: &[u8]) -> (tempfile::TempDir, AranduApp) {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        fs::write(dir.path().join(name), bytes).expect("cria arquivo de fixture");
        let mut app = AranduApp::new_at(Config::default(), dir.path());
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .expect("fixture contém o arquivo");
        app.left.select(index, false);
        (dir, app)
    }

    fn wait_for_preview(harness: &mut Harness<'_, AranduApp>) {
        for _ in 0..200 {
            if harness.state().preview_ready() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run();
        }
    }

    #[test]
    fn preview_markdown_shows_raw_text_and_kind() {
        let (_dir, app) = preview_app_for("README.md", b"# Titulo\n\n- item\n");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        harness.key_press(egui::Key::F3);
        harness.run();
        wait_for_preview(&mut harness);

        assert_eq!(
            harness.state().preview_markdown().as_deref(),
            Some("# Titulo\n\n- item\n")
        );
        let kind = rust_i18n::t!("preview_kind_markdown").to_string();
        assert!(
            harness.query_all_by_label(&kind).count() >= 1,
            "esperava o tipo '{kind}' no header do preview"
        );
    }

    #[test]
    fn preview_csv_draws_table_and_kind() {
        let (_dir, app) = preview_app_for("dados.csv", b"nome,idade\nAna,30\n");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        harness.key_press(egui::Key::F3);
        harness.run();
        wait_for_preview(&mut harness);

        for label in ["nome", "idade", "Ana", "30"] {
            assert!(
                harness.query_all_by_label(label).count() >= 1,
                "esperava a célula '{label}' na tabela CSV"
            );
        }
        let kind = rust_i18n::t!("preview_kind_csv").to_string();
        assert!(harness.query_all_by_label(&kind).count() >= 1);
    }

    #[test]
    fn preview_hex_shows_offsets() {
        let (_dir, app) = preview_app_for("dados.bin", &[0u8; 20]);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        harness.key_press(egui::Key::F3);
        harness.run();
        wait_for_preview(&mut harness);

        for offset in ["00000000", "00000010"] {
            assert!(
                harness.query_all_by_label(offset).count() >= 1,
                "esperava o offset '{offset}' no hex dump"
            );
        }
    }

    #[test]
    fn clicking_theme_button_cycles_active_theme() {
        let (_dir, app) = fixture_app();
        let initial = app.config().theme.name.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let button = harness
            .query_by_label(&initial)
            .expect("botão do tema na toolbar");
        button.click();
        harness.run();

        let current = harness.state().config().theme.name.clone();
        assert_ne!(current, initial, "o clique deveria trocar o tema ativo");
        assert!(
            harness.query_all_by_label(&current).count() >= 1,
            "a toolbar deveria exibir o novo tema '{current}'"
        );
    }

    #[test]
    fn vim_preset_navigates_with_j_and_k() {
        let (_dir, mut app) = fixture_app();
        app.keymap.preset = "vim".to_owned();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let start = harness.state().left.focus;
        harness.key_press(egui::Key::J);
        harness.run();
        assert_eq!(harness.state().left.focus, start + 1);
        harness.key_press(egui::Key::K);
        harness.run();
        assert_eq!(harness.state().left.focus, start);
        // Tab continua alternando o painel no preset vim.
        harness.key_press(egui::Key::Tab);
        harness.run();
        assert_eq!(harness.state().active_side(), PanelSide::Right);
    }

    #[test]
    fn f1_opens_cheatsheet_and_escape_closes_it() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let title = rust_i18n::t!("hints_title").to_string();
        assert_eq!(harness.query_all_by_label(&title).count(), 0);

        harness.key_press(egui::Key::F1);
        harness.run();
        assert!(harness.state().show_hints);
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o cheatsheet '{title}'"
        );

        // Os atalhos globais ficam suspensos enquanto o cheatsheet está aberto.
        harness.key_press(egui::Key::Tab);
        harness.run();
        assert_eq!(harness.state().active_side(), PanelSide::Left);
        assert!(harness.state().show_hints);

        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(!harness.state().show_hints);
        assert_eq!(
            harness.query_all_by_label(&title).count(),
            0,
            "o cheatsheet deveria sumir ao pressionar Escape"
        );
    }

    #[test]
    fn status_bar_shows_dynamic_hints_from_keymap() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let expected = format!(
            "F3 {} · Ctrl+1 {} · F5 {} · F6 {} · F2 {} · Del {} · Ctrl+Z {}",
            rust_i18n::t!("action_toggle_preview"),
            rust_i18n::t!("action_toggle_view_mode"),
            rust_i18n::t!("action_copy"),
            rust_i18n::t!("action_move"),
            rust_i18n::t!("action_rename"),
            rust_i18n::t!("action_delete"),
            rust_i18n::t!("action_undo"),
        );
        assert!(
            harness.query_all_by_label(&expected).count() >= 1,
            "esperava os hints dinâmicos na status bar: {expected}"
        );
    }

    #[test]
    fn ctrl_1_toggles_view_mode_roundtrip() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        assert_eq!(harness.state().left.view_mode, ViewMode::List);

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Num1);
        harness.run();
        assert_eq!(harness.state().left.view_mode, ViewMode::Icons);

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Num1);
        harness.run();
        assert_eq!(harness.state().left.view_mode, ViewMode::List);
    }

    #[test]
    fn icons_toolbar_reflects_current_mode() {
        let (_dir, app) = fixture_app();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let list = rust_i18n::t!("view_mode_list").to_string();
        let icons = rust_i18n::t!("view_mode_icons").to_string();
        assert!(harness.query_all_by_label(&list).count() >= 1);

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Num1);
        harness.run();
        assert!(
            harness.query_all_by_label(&icons).count() >= 1,
            "a toolbar deveria exibir o modo 'Ícones'"
        );
        assert_eq!(harness.query_all_by_label(&list).count(), 0);
    }

    #[test]
    fn icons_mode_keeps_enter_on_focused_directory() {
        let (dir, mut app) = fixture_app();
        app.left.view_mode = ViewMode::Icons;
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.is_dir)
            .expect("fixture tem um diretório");
        app.left.focus = index;

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert_eq!(harness.state().left.cwd, dir.path().join("fixture-dir"));
    }

    #[test]
    fn arrow_keys_move_focus_through_icon_grid() {
        let dir = tempfile::tempdir().expect("cria diretório temporário");
        for index in 0..12 {
            fs::write(dir.path().join(format!("arquivo-{index:02}.txt")), b"x")
                .expect("cria arquivo");
        }
        let mut app = AranduApp::new_at(Config::default(), dir.path());
        app.left.view_mode = ViewMode::Icons;

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        let cols = harness.state().left.grid_cols;
        assert!(cols >= 2, "esperava pelo menos duas colunas na grade");

        harness.key_press(egui::Key::ArrowRight);
        harness.run();
        assert_eq!(harness.state().left.focus, 1);

        harness.key_press(egui::Key::ArrowDown);
        harness.run();
        assert_eq!(harness.state().left.focus, 1 + cols);

        harness.key_press(egui::Key::ArrowLeft);
        harness.run();
        assert_eq!(harness.state().left.focus, cols);

        harness.key_press(egui::Key::ArrowUp);
        harness.run();
        assert_eq!(harness.state().left.focus, 0);
    }

    #[test]
    fn f4_opens_tag_editor_and_enter_adds_tag() {
        let (dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);
        let path = dir.path().join("fixture-file.txt");

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1200.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F4);
        harness.run();
        assert!(
            harness.state().tag_editor.is_some(),
            "F4 deveria abrir o editor de tags"
        );
        let title = rust_i18n::t!("tags_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o diálogo '{title}'"
        );

        // A digitação real vai para o `TextEdit`; aqui preenchemos o estado.
        harness
            .state_mut()
            .tag_editor
            .as_mut()
            .expect("editor aberto")
            .input
            .push_str("urgente");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert_eq!(
            harness.state().tags_of(&path),
            vec!["urgente".to_owned()],
            "Enter deveria adicionar a tag ao alvo"
        );
        assert!(
            harness.state().tag_editor.is_some(),
            "o editor continua aberto para adicionar mais tags"
        );

        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.state().tag_editor.is_none());
    }

    #[test]
    fn tag_map_resolves_tags_for_visible_entries() {
        let (dir, mut app) = fixture_app();
        let path = dir.path().join("fixture-file.txt");
        app.tags.add(&path, "chip").expect("adiciona tag");

        let map = app.tag_map_for(&app.left);
        let key = path.to_string_lossy().into_owned();
        assert_eq!(map.get(&key), Some(&vec!["chip".to_owned()]));
    }

    #[test]
    fn checksums_dialog_shows_sha256_and_md5() {
        let (dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);
        let _ = dir;

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F9);
        harness.run();
        assert!(
            harness.state().checksums.is_some(),
            "F9 deveria abrir o diálogo de checksums"
        );
        let title = rust_i18n::t!("checksums_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o diálogo '{title}'"
        );

        // O cálculo roda em thread; bombeia frames até o canal esvaziar.
        for _ in 0..200 {
            if harness.state().checksums_done() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run();
        }
        assert!(
            harness.state().checksums_done(),
            "o cálculo de checksums não terminou a tempo"
        );

        let results = harness.state().checksums_results();
        assert_eq!(
            results,
            vec![(
                "92359bb294288000958de4f1f20d5778681b14bfe2f0868104f79230942a6984".to_owned(),
                "b59853db2f3ef8f156a72e38c30ba7d2".to_owned(),
            )],
            "hashes de 'conteudo'"
        );
    }

    #[test]
    fn batch_rename_dialog_previews_unchanged_pattern() {
        let (_dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F7);
        harness.run();
        assert!(
            harness.state().batch_rename.is_some(),
            "F7 deveria abrir o diálogo de renomeação em lote"
        );
        let title = rust_i18n::t!("batch_rename_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o diálogo '{title}'"
        );
        assert!(
            harness
                .query_all_by_label("fixture-file.txt → fixture-file.txt")
                .count()
                >= 1,
            "esperava a pré-visualização sem mudança para o padrão inicial"
        );
    }

    #[test]
    fn compress_dialog_opens_with_default_zip_name() {
        let (_dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .with_accessibility_check(false)
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F8);
        harness.run();

        let dialog = harness
            .state()
            .compress
            .as_ref()
            .expect("F8 deveria abrir o diálogo de compressão");
        assert_eq!(dialog.name, "fixture-file.txt.zip");
        assert_eq!(dialog.destination_dir, dialog.sources[0].parent().unwrap());

        let title = rust_i18n::t!("compress_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o diálogo '{title}'"
        );
    }

    #[test]
    fn extract_action_creates_directory_in_same_panel() {
        let (dir, mut app) = fixture_app();
        let archive = dir.path().join("pacote.zip");
        arandu_core::archive::create_zip(&[dir.path().join("fixture-file.txt")], &archive)
            .expect("cria o zip de teste");
        app.right.reload();

        let index = app
            .right
            .entries
            .iter()
            .position(|entry| entry.name == "pacote.zip")
            .expect("fixture tem pacote.zip");
        app.right.select(index, false);
        app.active = PanelSide::Right;

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F10);
        harness.run_steps(1);

        // O job em background mantém repaint contínuo, então `run()` (que
        // espera a UI assentar) estouraria max_steps. Avança passos com teto
        // até o diretório extraído aparecer.
        let extracted = dir.path().join("pacote").join("fixture-file.txt");
        for _ in 0..20 {
            if extracted.is_file() && harness.state().extract_job.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            harness.run_steps(1);
        }
        assert!(harness.state().extract_job.is_none());
        assert!(
            extracted.is_file(),
            "esperava o conteúdo extraído em pacote/"
        );
    }

    #[test]
    fn build_open_invocations_expands_path_and_paths() {
        let paths = [PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")];

        let per_file = build_open_invocations("editor --file {path}", &paths).expect("comando");
        assert_eq!(per_file.len(), 2);
        assert_eq!(
            per_file[0],
            (
                "editor".to_owned(),
                vec!["--file".to_owned(), "/tmp/a.txt".to_owned()]
            )
        );
        assert_eq!(
            per_file[1],
            (
                "editor".to_owned(),
                vec!["--file".to_owned(), "/tmp/b.txt".to_owned()]
            )
        );

        let all_at_once = build_open_invocations("cmp {paths}", &paths).expect("comando");
        assert_eq!(
            all_at_once,
            vec![(
                "cmp".to_owned(),
                vec!["/tmp/a.txt".to_owned(), "/tmp/b.txt".to_owned()]
            )]
        );

        let appended = build_open_invocations("xdg-open", &paths[..1]).expect("comando");
        assert_eq!(
            appended,
            vec![("xdg-open".to_owned(), vec!["/tmp/a.txt".to_owned()])]
        );

        assert!(build_open_invocations("   ", &paths).is_err());
    }

    #[test]
    fn f11_runs_configured_command_without_spawning() {
        let (dir, mut app) = fixture_app();
        app.config
            .open_with
            .insert("txt".to_owned(), "reader --file {path}".to_owned());
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);
        let executor = RecordingOpenExecutor::default();
        app.open_executor = Arc::new(executor.clone());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F11);
        harness.run();

        let commands = executor.commands.lock().unwrap();
        assert_eq!(commands.len(), 1, "F11 deveria rodar o comando configurado");
        assert_eq!(commands[0].0, "reader");
        assert_eq!(
            commands[0].1,
            vec![
                "--file".to_owned(),
                dir.path()
                    .join("fixture-file.txt")
                    .to_string_lossy()
                    .into_owned()
            ]
        );
        assert!(executor.default_calls.lock().unwrap().is_empty());
        assert!(harness.state().open_with_dialog.is_none());
    }

    #[test]
    fn shift_f11_uses_system_default_app() {
        let (dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);
        let executor = RecordingOpenExecutor::default();
        app.open_executor = Arc::new(executor.clone());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::F11);
        harness.run();

        let calls = executor.default_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], dir.path().join("fixture-file.txt"));
        assert!(executor.commands.lock().unwrap().is_empty());
    }

    #[test]
    fn f11_without_configured_app_opens_dialog() {
        let (_dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);
        let executor = RecordingOpenExecutor::default();
        app.open_executor = Arc::new(executor.clone());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F11);
        harness.run();

        assert!(harness.state().open_with_dialog.is_some());
        let title = rust_i18n::t!("open_with_title").to_string();
        assert!(
            harness.query_all_by_label(&title).count() >= 1,
            "esperava o diálogo '{title}'"
        );
        assert!(executor.commands.lock().unwrap().is_empty());
        assert!(executor.default_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn open_with_dialog_persists_typed_command() {
        let (dir, mut app) = fixture_app();
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "fixture-file.txt")
            .expect("fixture tem fixture-file.txt");
        app.left.select(index, false);
        let executor = RecordingOpenExecutor::default();
        app.open_executor = Arc::new(executor.clone());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(1000.0, 700.0))
            .build_ui_state(|ui, app: &mut AranduApp| app.show(ui), app);
        harness.run();

        harness.key_press(egui::Key::F11);
        harness.run();
        let dialog = harness
            .state_mut()
            .open_with_dialog
            .as_mut()
            .expect("diálogo aberto");
        dialog.command = "writer {path}".to_owned();
        harness.run();

        harness.key_press(egui::Key::Enter);
        harness.run();

        assert!(harness.state().open_with_dialog.is_none());
        assert_eq!(
            harness
                .state()
                .config
                .open_with
                .get("txt")
                .map(String::as_str),
            Some("writer {path}")
        );
        let commands = executor.commands.lock().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].0, "writer");
        assert_eq!(
            commands[0].1,
            vec![
                dir.path()
                    .join("fixture-file.txt")
                    .to_string_lossy()
                    .into_owned()
            ]
        );
    }
}
