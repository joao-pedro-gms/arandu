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
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

use arandu_config::{
    Action, Config, KeyBinding, Keymap, Modifiers as KeyModifiers, TagsStore, Theme,
};
use arandu_core::fs::{CoreError, Entry, format_size, list_dir};
use arandu_core::ops::{self, Op, OpKind, OpsError, Progress};
use arandu_core::preview::{PreviewContent, PreviewError, PreviewRequest, UnsupportedReason};
use arandu_core::queue::{OpQueue, QueueEvent, UndoStack};
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
/// Teto de linhas renderizadas com syntax highlight.
///
/// Um arquivo de 1 MiB pode ter dezenas de milhares de linhas; construir e
/// dispor um `LayoutJob` completo a cada frame travaria a UI. O texto inteiro
/// continua disponível no cache, apenas a renderização é limitada.
const MAX_HIGHLIGHT_LINES: usize = 2000;

/// Ações listadas na status bar, na ordem em que aparecem (as principais).
const STATUS_HINTS: [Action; 6] = [
    Action::TogglePreview,
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

    /// Pede ao próximo frame que ajuste o scroll para mostrar o foco.
    fn ensure_focus_visible(&mut self) {
        let top = self.focus as f32 * ROW_H;
        let bottom = top + ROW_H;
        let view_h = self.viewport_h.max(ROW_H);
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
    /// Textura da imagem atual.
    texture: Option<(PreviewKey, egui::TextureHandle)>,
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
    /// Tags dos arquivos, persistidas em `tags.toml`.
    tags: TagsStore,
    /// Painel de pré-visualização (F3).
    preview: PreviewState,
    /// Tema resolvido (config + arquivo de tema), aplicado no primeiro frame.
    theme: Theme,
    /// Atalhos redefiníveis (preset + overrides).
    keymap: Keymap,
    /// Cheatsheet de atalhos visível (F1).
    show_hints: bool,
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
        rust_i18n::set_locale(&config.locale);
        let dir = dir.into();
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
            tags,
            preview: PreviewState::default(),
            theme,
            keymap,
            show_hints: false,
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

        // O `TextEdit` dos diálogos de renomeação/edição de tags recebe a
        // digitação; os atalhos globais ficam suspensos.
        if self.rename.is_some() || self.tag_editor.is_some() {
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

        let toggle_preview = self.action_pressed(Action::TogglePreview, &pressed);
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

        if toggle_preview {
            self.preview.visible = !self.preview.visible;
        }
        if toggle_panel {
            self.active = self.active.other();
        }

        {
            let panel = self.active_panel_mut();
            if up {
                panel.move_focus(-1);
            }
            if down {
                panel.move_focus(1);
            }
            if enter {
                panel.open_focused();
            }
            if back {
                panel.go_up();
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
        if delete {
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

        // O preview fica fixo à direita; os dois painéis dividem o restante.
        let preview_w = if self.preview.visible {
            let max_w = content.width() * PREVIEW_MAX_FRACTION;
            (content.width() * PREVIEW_FRACTION)
                .max(PREVIEW_MIN_W.min(max_w))
                .min(max_w)
        } else {
            0.0
        };
        let panes_rect = Rect::from_min_max(
            content.min,
            pos2(
                (content.right() - preview_w).max(content.left() + 1.0),
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
        } else if right_interacted {
            self.active = PanelSide::Right;
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
            let preview_rect =
                Rect::from_min_max(pos2(panes_rect.right(), content.top()), content.max);
            let focused = self
                .active_panel()
                .entries
                .get(self.active_panel().focus)
                .cloned();
            self.ensure_preview(focused.as_ref());
            self.draw_preview(ui, preview_rect, focused.as_ref(), &visuals);
        }
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
        ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let header = ui.allocate_ui_with_layout(
                vec2(ui.available_width(), HEADER_H),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(RichText::new(rust_i18n::t!("preview_title")).strong());
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
                PreviewContent::Image { image, .. } => {
                    self.draw_preview_image(ui, body, &key, image, visuals);
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

    fn draw_preview_image(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        key: &PreviewKey,
        image: &image::DynamicImage,
        _visuals: &egui::Visuals,
    ) {
        let stale = self
            .preview
            .texture
            .as_ref()
            .map(|(cached_key, _)| cached_key != key)
            .unwrap_or(true);
        if stale {
            let rgba = image.to_rgba8();
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
        let width = image.width() as f32;
        let height = image.height() as f32;
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

        // Cabeçalho de colunas (apenas visual nesta etapa).
        let (column_rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), COLUMN_HEADER_H), Sense::hover());
        paint_columns(ui, column_rect, &visuals, active);

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

        let output = {
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
    };
    rust_i18n::t!(key).to_string()
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
            ui.label(RichText::new("📁").size(48.0));
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

/// Descrição i18n curta de uma ação (usada nos hints e no cheatsheet).
fn action_description(action: Action) -> String {
    let key = match action {
        Action::TogglePreview => "action_toggle_preview",
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
    };
    rust_i18n::t!(key).into_owned()
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
    use eframe::egui;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::{AranduApp, PanelSide, format_modified, parse_hex_color, visible_breadcrumb};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

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
            "F3 {} · F5 {} · F6 {} · F2 {} · Del {} · Ctrl+Z {}",
            rust_i18n::t!("action_toggle_preview"),
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
}
