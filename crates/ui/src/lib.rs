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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arandu_config::Config;
use arandu_core::fs::{CoreError, Entry, format_size, list_dir};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Key, Label, Layout, Modifiers, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, TextStyle, Ui, UiBuilder, pos2, vec2,
};

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

    /// Diretório atual e listagem, para a status bar.
    fn item_count(&self) -> usize {
        self.entries.len()
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
        Self {
            config,
            left: PanelState::new(dir.clone()),
            right: PanelState::new(dir),
            active: PanelSide::Left,
            split: 0.5,
            initialized: false,
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

        self.handle_keyboard(&ctx);

        // O `Ui` entregue pelo eframe 0.37 não tem margem nem fundo: pintamos o
        // fundo do painel e dividimos a área em toolbar / conteúdo / status.
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, ui.visuals().panel_fill);

        let toolbar_rect = Rect::from_min_size(full.min, vec2(full.width(), TOOLBAR_H));
        let status_rect = Rect::from_min_max(
            pos2(full.left(), full.bottom() - STATUS_H),
            pos2(full.right(), full.bottom()),
        );
        let content = Rect::from_min_max(
            pos2(full.left(), full.top() + TOOLBAR_H),
            pos2(full.right(), full.bottom() - STATUS_H),
        );

        self.draw_toolbar(ui, toolbar_rect);
        self.draw_content(ui, content);
        self.draw_status_bar(ui, status_rect);
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let theme = if self.config.theme.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(theme);

        let mut style = (*ctx.style_of(theme)).clone();
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.5));

        let mut visuals = if self.config.theme.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        if let Some(accent) = parse_hex_color(&self.config.theme.accent) {
            visuals.selection.bg_fill = with_alpha(accent, SELECTION_ALPHA);
            visuals.selection.stroke = Stroke::new(1.0, accent);
            visuals.hyperlink_color = accent;
        }

        // O estilo é por tema no egui 0.37; um único ponto de configuração
        // concentra tema + tipografia + accent.
        style.visuals = visuals;
        ctx.set_style_of(theme, style);
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

    /// Trata atalhos globais antes de desenhar os widgets.
    ///
    /// Nenhum widget desta etapa usa `Sense::click()`/`menu_button` (todos os
    /// clicáveis são `Label` com `Sense::CLICK`, não focáveis), então o Tab do
    /// egui não tem widgets para onde pular e podemos consumi-lo com segurança.
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let tab = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab));
        if tab {
            self.active = self.active.other();
            return;
        }

        let (up, down, enter, back) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Backspace),
            )
        });

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
        let accent = accent_color(&self.config, &visuals);

        let avail = (content.width() - SPLITTER_HIT).max(1.0);
        let min_w = MIN_PANEL_W.min(avail / 2.0);
        let left_w = (self.split * avail).clamp(min_w, avail - min_w);

        let left_rect =
            Rect::from_min_max(content.min, pos2(content.left() + left_w, content.bottom()));
        let splitter_rect = Rect::from_min_max(
            pos2(left_rect.right(), content.top()),
            pos2(left_rect.right() + SPLITTER_HIT, content.bottom()),
        );
        let right_rect =
            Rect::from_min_max(pos2(splitter_rect.right(), content.top()), content.max);

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
                let new_left = (pos.x - content.left()).clamp(min_w, avail - min_w);
                self.split = new_left / avail;
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        let left_interacted = draw_panel(
            ui,
            left_rect,
            &mut self.left,
            PanelSide::Left,
            self.active == PanelSide::Left,
            accent,
        );
        let right_interacted = draw_panel(
            ui,
            right_rect,
            &mut self.right,
            PanelSide::Right,
            self.active == PanelSide::Right,
            accent,
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
            content.y_range(),
            Stroke::new(1.0, splitter_color),
        );
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
        let weak = visuals.weak_text_color();

        ui.scope_builder(
            UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 0.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new(format!("{items}, {selected}")).color(weak));
                    ui.add_space(12.0);
                    ui.add(Label::new(RichText::new(path).color(weak)).truncate());
                });
            },
        );
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
                    if let Some(row_action) = draw_row(ui, entry, index, &style, &visuals, side) {
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
) -> Option<RowAction> {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::hover());
    let id = ui.make_persistent_id(("entry", side, &entry.path));
    let response = ui.interact(rect, id, Sense::CLICK);

    let left = rect.left() + 6.0;
    let right = rect.right() - 6.0;
    let modified_right = right;
    let size_right = modified_right - MODIFIED_COL_W;
    let name_right = size_right - SIZE_COL_W - 8.0;

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
fn accent_color(config: &Config, visuals: &egui::Visuals) -> Color32 {
    parse_hex_color(&config.theme.accent).unwrap_or(visuals.selection.bg_fill)
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
}

#[cfg(test)]
mod tests {
    use std::fs;

    use arandu_config::Config;
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
}
