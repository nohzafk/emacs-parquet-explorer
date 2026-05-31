use emacs_egui_sdk::{EguiEmacsApp, ThemeColors};
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::wasm_bindgen;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

lazy_static::lazy_static! {
    static ref LOADED_TABLE: Mutex<Option<Result<ParquetTable, String>>> = Mutex::new(None);
    static ref ASYNC_TRIGGERED: Mutex<Option<String>> = Mutex::new(None);
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ExplorerState {
    #[serde(default)]
    pub filepath: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnFilter {
    pub column: String,
    pub operator: String, // "contains", "=", ">", "<", ">=", "<="
    pub value: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SchemaField {
    pub name: String,
    pub physical_type: String,
    pub logical_type: String,
    pub nullable: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FileStats {
    pub total_rows: i64,
    pub num_row_groups: usize,
    pub version: i32,
    pub created_by: String,
    pub compression_codecs: Vec<String>,
}

#[derive(Clone, Default, Debug)]
pub struct ParquetTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub schema: Vec<SchemaField>,
    pub stats: FileStats,
    pub compact_widths: Vec<f32>,
    pub expand_widths: Vec<f32>,
}

fn parse_parquet(bytes: Vec<u8>) -> Result<ParquetTable, String> {
    let bytes = bytes::Bytes::from(bytes);
    let reader = SerializedFileReader::new(bytes).map_err(|e| e.to_string())?;

    // 1. Extract Column Schema Names
    let file_metadata = reader.metadata().file_metadata();
    let schema_descr = file_metadata.schema_descr();
    let columns: Vec<String> = (0..schema_descr.num_columns())
        .map(|i| schema_descr.column(i).name().to_string())
        .collect();

    let mut schema = Vec::new();
    for i in 0..schema_descr.num_columns() {
        let col_descr = schema_descr.column(i);
        let name = col_descr.name().to_string();
        let physical_type = col_descr.physical_type().to_string();
        
        let converted_type = col_descr.converted_type();
        let logical_type_str = if let Some(ref lt) = col_descr.logical_type() {
            format!("{:?}", lt)
        } else if converted_type != parquet::basic::ConvertedType::NONE {
            converted_type.to_string()
        } else {
            "NONE".to_string()
        };

        let repetition = col_descr.self_type_ptr().get_basic_info().repetition();
        let nullable = repetition != parquet::basic::Repetition::REQUIRED;

        schema.push(SchemaField {
            name,
            physical_type,
            logical_type: logical_type_str,
            nullable,
        });
    }

    // Extract File Stats
    let total_rows = file_metadata.num_rows();
    let num_row_groups = reader.metadata().num_row_groups();
    let version = file_metadata.version();
    let created_by = file_metadata.created_by().unwrap_or("unknown").to_string();

    let mut codec_set = std::collections::HashSet::new();
    for rg_idx in 0..num_row_groups {
        let rg_meta = reader.metadata().row_group(rg_idx);
        for col_idx in 0..schema_descr.num_columns() {
            let col_meta = rg_meta.column(col_idx);
            codec_set.insert(col_meta.compression().to_string());
        }
    }
    let mut compression_codecs: Vec<String> = codec_set.into_iter().collect();
    compression_codecs.sort();

    let stats = FileStats {
        total_rows,
        num_row_groups,
        version,
        created_by,
        compression_codecs,
    };

    // 2. Read Rows (cap at 100,000 for high-performance virtual rendering inside WASM)
    let mut rows = Vec::new();
    let row_iter = reader.get_row_iter(None).map_err(|e| e.to_string())?;

    let mut count = 0;
    for record in row_iter {
        if count >= 100000 {
            break;
        }
        let record = record.map_err(|e| e.to_string())?;
        let mut row = Vec::new();
        for (_, field) in record.get_column_iter() {
            row.push(format!("{}", field));
        }
        rows.push(row);
        count += 1;
    }

    // 3. Compute Column Widths dynamically for both Compact and Expand modes
    let mut compact_widths = Vec::new();
    let mut expand_widths = Vec::new();
    for col_idx in 0..columns.len() {
        let col_name_len = columns[col_idx].len();
        
        // Calculate compact width (data cell lengths only)
        let mut max_cell_len = 0;
        let check_rows = rows.len().min(200);
        for row in rows.iter().take(check_rows) {
            if col_idx < row.len() {
                max_cell_len = max_cell_len.max(row[col_idx].len());
            }
        }
        let max_cell_len = max_cell_len.min(35);
        let compact_width = (max_cell_len as f32 * 8.0).clamp(65.0, 320.0);
        compact_widths.push(compact_width);
        
        // Calculate expand width (maximum of header length and data cell lengths)
        let max_expand_len = col_name_len.max(max_cell_len).min(80);
        let expand_width = (max_expand_len as f32 * 8.0).clamp(80.0, 640.0);
        expand_widths.push(expand_width);
    }

    Ok(ParquetTable {
        columns,
        rows,
        schema,
        stats,
        compact_widths,
        expand_widths,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Data,
    Schema,
}

pub struct ExplorerApp {
    filepath: String,
    parquet_table: Option<ParquetTable>,
    loading: bool,
    error_message: Option<String>,
    search_query: String,
    selected_cell: Option<(usize, usize)>,
    page_offset: usize,
    page_size: usize,
    custom_page_size_str: String,
    hidden_columns: std::collections::HashSet<String>,
    expand_mode: bool,
    view_mode: ViewMode,
    filters: Vec<ColumnFilter>,
    filter_col: String,
    filter_op: String,
    filter_val: String,
    show_pruning_panel: bool,
    show_filters_panel: bool,
}

impl ExplorerApp {
    pub fn new() -> Self {
        Self {
            filepath: String::new(),
            parquet_table: None,
            loading: false,
            error_message: None,
            search_query: String::new(),
            selected_cell: None,
            page_offset: 0,
            page_size: 50,
            custom_page_size_str: String::new(),
            hidden_columns: std::collections::HashSet::new(),
            expand_mode: false,
            view_mode: ViewMode::Data,
            filters: Vec::new(),
            filter_col: String::new(),
            filter_op: "contains".to_string(),
            filter_val: String::new(),
            show_pruning_panel: false,
            show_filters_panel: false,
        }
    }
}

impl EguiEmacsApp for ExplorerApp {
    type State = ExplorerState;

    fn on_state_update(&mut self, state: Self::State) {
        if state.filepath.is_empty() || state.filepath == self.filepath {
            return;
        }

        self.filepath = state.filepath.clone();
        self.loading = true;
        self.error_message = None;
        self.parquet_table = None;
        self.selected_cell = None;
        self.page_offset = 0;

        let url = emacs_egui_sdk::file_url(&state.filepath);

        // Fetch file in async task
        emacs_egui_sdk::wasm_bindgen_futures::spawn_local(async move {
            match emacs_egui_sdk::fetch_bytes(&url).await {
                Ok(bytes) => {
                    let parse_result = parse_parquet(bytes);
                    if let Ok(mut guard) = LOADED_TABLE.lock() {
                        *guard = Some(parse_result);
                    }
                }
                Err(e) => {
                    let err_str = e.as_string().unwrap_or_else(|| "Network Fetch Error".to_string());
                    if let Ok(mut guard) = LOADED_TABLE.lock() {
                        *guard = Some(Err(err_str));
                    }
                }
            }
        });
    }

    fn on_theme_update(&mut self, _theme: ThemeColors) {
        // Automatically handled by generic host theme styling
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for newly loaded data
        if let Ok(mut guard) = LOADED_TABLE.lock() {
            if let Some(res) = guard.take() {
                self.loading = false;
                match res {
                    Ok(table) => {
                        self.parquet_table = Some(table);
                        self.error_message = None;
                    }
                    Err(err) => {
                        self.error_message = Some(err);
                    }
                }
            }
        }

        // 1. Draw resizable bottom details panel first (if selection exists and in Data view)
        if self.view_mode == ViewMode::Data {
            if let Some(ref table) = self.parquet_table {
                if let Some((row, col)) = self.selected_cell {
                    let cell_value = table.rows[row][col].clone();
                    let col_name = table.columns[col].clone();
                    
                    egui::TopBottomPanel::bottom("details_panel")
                        .resizable(false)
                        .default_height(40.0)
                        .show(ctx, |ui| {
                            ui.add_space(6.0); // comfortable top margin
                            ui.horizontal(|ui| {
                                ui.add_space(8.0); // left margin
                                ui.label("🔍");
                                ui.weak(format!("{}:", col_name));

                                // Render the cell value first so it stays on the left next to the column name
                                let display_val = if cell_value.len() > 150 {
                                    format!("{}...", &cell_value[0..147])
                                } else {
                                    cell_value.clone()
                                };
                                ui.add(egui::Label::new(display_val).selectable(true));

                                // Right-aligned button
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.add_space(8.0); // right margin
                                    if ui.button("🔍 Filter by selection").clicked() {
                                        let filter = ColumnFilter {
                                            column: col_name.clone(),
                                            operator: "=".to_string(),
                                            value: cell_value.clone(),
                                        };
                                        if !self.filters.contains(&filter) {
                                            self.filters.push(filter);
                                            self.page_offset = 0;
                                            self.show_filters_panel = true;
                                        }
                                    }
                                });
                            });
                            ui.add_space(6.0); // comfortable bottom margin
                        });
                }
            }
        }

        // 2. Draw CentralPanel (automatically consumes remaining space)
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);

            // 1. Header controls
            ui.horizontal(|ui| {
                ui.heading("📊 Parquet File Explorer");
                if self.loading {
                    ui.spinner();
                    ui.label("Streaming binary...");
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new(format!("File: {}", self.filepath)).weak());
            });

            if let Some(ref err) = self.error_message {
                ui.colored_label(egui::Color32::from_rgb(198, 88, 94), format!("Error: {}", err));
            }

            if let Some(ref table) = self.parquet_table {
                // View Mode Select Tabs
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.view_mode, ViewMode::Data, "🗃 Data View");
                    ui.selectable_value(&mut self.view_mode, ViewMode::Schema, "📋 Schema & Metadata");

                    if self.view_mode == ViewMode::Data {
                        ui.add_space(20.0);
                        let text = if self.hidden_columns.is_empty() {
                            if self.show_pruning_panel { "📐 Column Visibility & Pruning ▲" } else { "📐 Column Visibility & Pruning ▼" }.to_string()
                        } else {
                            let dir = if self.show_pruning_panel { "▲" } else { "▼" };
                            format!("📐 Column Visibility & Pruning ({} hidden) {}", self.hidden_columns.len(), dir)
                        };
                        if ui.selectable_label(self.show_pruning_panel, &text).clicked() {
                            self.show_pruning_panel = !self.show_pruning_panel;
                        }

                        ui.add_space(20.0);
                        let text_f = if self.filters.is_empty() {
                            if self.show_filters_panel { "🔍 Predicate Filters ▲" } else { "🔍 Predicate Filters ▼" }.to_string()
                        } else {
                            let dir = if self.show_filters_panel { "▲" } else { "▼" };
                            format!("🔍 Predicate Filters ({} active) {}", self.filters.len(), dir)
                        };
                        if ui.selectable_label(self.show_filters_panel, text_f).clicked() {
                            self.show_filters_panel = !self.show_filters_panel;
                        }
                    }
                });

                if self.view_mode == ViewMode::Data {
                    if self.show_pruning_panel {
                        ui.add_space(4.0);
                        egui::Frame::group(ui.style())
                            .fill(ui.visuals().extreme_bg_color)
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        if !self.hidden_columns.is_empty() {
                                            let btn = egui::Button::new(egui::RichText::new("👁 Show All").color(egui::Color32::WHITE))
                                                .fill(egui::Color32::from_rgb(45, 55, 75))
                                                .rounding(4.0);
                                            if ui.add(btn).clicked() {
                                                self.hidden_columns.clear();
                                            }
                                            ui.add_space(8.0);
                                        }
                                        for col in &table.columns {
                                            let is_visible = !self.hidden_columns.contains(col);
                                            let mut temp_visible = is_visible;
                                            if ui.checkbox(&mut temp_visible, col).changed() {
                                                if temp_visible {
                                                    self.hidden_columns.remove(col);
                                                } else {
                                                    // Safety: at least 1 column must stay visible
                                                    if self.hidden_columns.len() < table.columns.len() - 1 {
                                                        self.hidden_columns.insert(col.clone());
                                                    }
                                                }
                                            }
                                        }
                                    });
                                });
                            });
                    }

                    if self.show_filters_panel {
                        ui.add_space(4.0);
                        egui::Frame::group(ui.style())
                            .fill(ui.visuals().extreme_bg_color)
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
 
                                    // 1. Active Filters Badges list (wrapped onto multiple lines cleanly!)
                                    if !self.filters.is_empty() {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(egui::RichText::new("Active Filters:").strong());
                                            let mut to_remove = None;
                                            for (idx, filter) in self.filters.iter().enumerate() {
                                                let text = format!("{} {} \"{}\"  ❌", filter.column, filter.operator, filter.value);
                                                let btn = egui::Button::new(egui::RichText::new(text).color(egui::Color32::WHITE))
                                                    .fill(egui::Color32::from_rgb(45, 55, 75))
                                                    .rounding(4.0);
                                                if ui.add(btn).clicked() {
                                                    to_remove = Some(idx);
                                                }
                                            }
                                            if let Some(idx) = to_remove {
                                                self.filters.remove(idx);
                                                self.page_offset = 0; // reset paging
                                            }
                                            
                                            ui.add_space(8.0);
                                            if ui.button("🗑 Clear All").clicked() {
                                                self.filters.clear();
                                                self.page_offset = 0;
                                            }
                                        });
                                        ui.separator();
                                    }

                                    if self.filter_col.is_empty() && !table.columns.is_empty() {
                                        self.filter_col = table.columns[0].clone();
                                    }

                                    // 2. Manual filter form
                                    ui.horizontal(|ui| {
                                        ui.label("Column:");
                                        egui::ComboBox::from_id_salt("filter_column_select")
                                            .selected_text(&self.filter_col)
                                            .width(140.0)
                                            .show_ui(ui, |ui| {
                                                for col in &table.columns {
                                                    ui.selectable_value(&mut self.filter_col, col.clone(), col);
                                                }
                                            });

                                        ui.add_space(8.0);
                                        ui.label("Operator:");
                                        egui::ComboBox::from_id_salt("filter_operator_select")
                                            .selected_text(&self.filter_op)
                                            .width(90.0)
                                            .show_ui(ui, |ui| {
                                                for op in ["contains", "=", ">", "<", ">=", "<="] {
                                                    ui.selectable_value(&mut self.filter_op, op.to_string(), op);
                                                }
                                            });

                                        ui.add_space(8.0);
                                        ui.label("Value:");
                                        let val_resp = ui.text_edit_singleline(&mut self.filter_val);

                                        ui.add_space(8.0);
                                        let mut add_filter = ui.button("➕ Add Filter").clicked();
                                        if val_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                            add_filter = true;
                                        }

                                        if add_filter {
                                            if !self.filter_val.trim().is_empty() {
                                                let filter = ColumnFilter {
                                                    column: self.filter_col.clone(),
                                                    operator: self.filter_op.clone(),
                                                    value: self.filter_val.trim().to_string(),
                                                };
                                                if !self.filters.contains(&filter) {
                                                    self.filters.push(filter);
                                                    self.page_offset = 0;
                                                    self.filter_val.clear();
                                                }
                                            }
                                        }
                                    });
                                });
                            });
                    }
                }
                ui.add_space(4.0);

                match self.view_mode {
                    ViewMode::Data => {
                        // Filter row indices matching search AND active column-specific filters
                        let filtered_rows: Vec<usize> = if self.search_query.is_empty() && self.filters.is_empty() {
                            (0..table.rows.len()).collect()
                        } else {
                            let query = self.search_query.to_lowercase();
                            (0..table.rows.len())
                                .filter(|&i| {
                                    let row = &table.rows[i];
                                    
                                    // 1. Global search matches (if query is present)
                                    let global_match = if query.is_empty() {
                                        true
                                    } else {
                                        row.iter().any(|cell| cell.to_lowercase().contains(&query))
                                    };
                                    
                                    if !global_match {
                                        return false;
                                    }
                                    
                                    // 2. All column filters must match
                                    for filter in &self.filters {
                                        let col_idx = match table.columns.iter().position(|c| c == &filter.column) {
                                            Some(idx) => idx,
                                            None => continue,
                                        };
                                        if col_idx >= row.len() {
                                            return false;
                                        }
                                        
                                        let cell_val = &row[col_idx];
                                        let f_val = &filter.value;
                                        
                                        // Attempt numeric comparison if both are parseable as floats
                                        let cell_num = cell_val.trim().parse::<f64>();
                                        let filter_num = f_val.trim().parse::<f64>();
                                        
                                        let match_filter = match (cell_num, filter_num) {
                                            (Ok(c_n), Ok(f_n)) => match filter.operator.as_str() {
                                                "=" => (c_n - f_n).abs() < 1e-9,
                                                ">" => c_n > f_n,
                                                "<" => c_n < f_n,
                                                ">=" => c_n >= f_n,
                                                "<=" => c_n <= f_n,
                                                "contains" => cell_val.to_lowercase().contains(&f_val.to_lowercase()),
                                                _ => false,
                                            },
                                            _ => match filter.operator.as_str() {
                                                "=" => cell_val.to_lowercase() == f_val.to_lowercase(),
                                                "contains" => cell_val.to_lowercase().contains(&f_val.to_lowercase()),
                                                ">" => cell_val.to_lowercase() > f_val.to_lowercase(),
                                                "<" => cell_val.to_lowercase() < f_val.to_lowercase(),
                                                ">=" => cell_val.to_lowercase() >= f_val.to_lowercase(),
                                                "<=" => cell_val.to_lowercase() <= f_val.to_lowercase(),
                                                _ => false,
                                            }
                                        };
                                        
                                        if !match_filter {
                                            return false;
                                        }
                                    }
                                    
                                    true
                                })
                                .collect()
                        };

                        let total_filtered = filtered_rows.len();

                        // Statistics & Filters
                        ui.horizontal(|ui| {
                            let cols_label = if self.hidden_columns.is_empty() {
                                format!("Columns: {}", table.columns.len())
                            } else {
                                format!("Columns: {} ({} hidden)", table.columns.len(), self.hidden_columns.len())
                            };
                            
                            let rows_label = if self.filters.is_empty() && self.search_query.is_empty() {
                                format!("Rows: {}", table.rows.len())
                            } else {
                                format!("Rows: {} (filtered to {})", table.rows.len(), total_filtered)
                            };

                            if !self.hidden_columns.is_empty() || !self.filters.is_empty() || !self.search_query.is_empty() {
                                ui.colored_label(
                                    egui::Color32::from_rgb(230, 140, 10),
                                    format!("{}  |  {}", cols_label, rows_label)
                                );
                            } else {
                                ui.label(format!("{}  |  {}", cols_label, rows_label));
                            }
                            ui.add_space(20.0);
                            ui.label("Search:");
                            if ui.text_edit_singleline(&mut self.search_query).changed() {
                                self.page_offset = 0; // reset paging
                            }
                        });

                        // Paging Control
                        let start_idx = self.page_offset;
                        let end_idx = (start_idx + self.page_size).min(total_filtered);

                        ui.horizontal(|ui| {
                            if ui.button("◀ Prev").clicked() && self.page_offset >= self.page_size {
                                self.page_offset -= self.page_size;
                            }
                            ui.add_space(4.0);

                            ui.label(format!("Showing {}-{} filtered", start_idx + 1, end_idx));
                            ui.add_space(4.0);

                            if ui.button("Next ▶").clicked() && self.page_offset + self.page_size < total_filtered {
                                self.page_offset += self.page_size;
                            }

                            ui.add_space(20.0);
                            ui.label("Page Size:");
                            for size in [50, 100, 500, 1000] {
                                if ui.selectable_label(self.page_size == size, format!("{}", size)).clicked() {
                                    self.page_size = size;
                                    self.page_offset = 0; // reset
                                    self.custom_page_size_str.clear();
                                }
                            }

                            ui.add_space(8.0);
                            ui.label("Custom:");
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.custom_page_size_str)
                                    .desired_width(55.0)
                                    .hint_text("e.g. 200")
                            );
                            if resp.changed() {
                                if let Ok(val) = self.custom_page_size_str.trim().parse::<usize>() {
                                    if val > 0 && val <= 100000 {
                                        self.page_size = val;
                                        self.page_offset = 0; // reset
                                    }
                                }
                            }

                            ui.add_space(20.0);
                            ui.checkbox(&mut self.expand_mode, "↔ Expand Columns");

                            ui.add_space(16.0);
                            if ui.button("📥 Export CSV").clicked() {
                                // Post export event with current file path
                                emacs_egui_sdk::emacs_post_message(
                                    "export-csv",
                                    serde_json::json!({
                                        "filepath": self.filepath.clone()
                                    })
                                );
                            }

                            ui.add_space(16.0);
                        });

                        ui.separator();

                         // 2. Data Table Grid
                        let column_widths = if self.expand_mode {
                            &table.expand_widths
                        } else {
                            &table.compact_widths
                        };

                        // Outer scroll area: Horizontal only (synchronizes header + data columns horizontally)
                        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
                            ui.vertical(|ui| {
                                // Sticky Header Grid with dynamic column widths
                                egui::Grid::new("header_grid")
                                    .spacing(egui::vec2(12.0, 4.0))
                                    .show(ui, |ui| {
                                        for (col_idx, col) in table.columns.iter().enumerate() {
                                            if !self.hidden_columns.contains(col) {
                                                let col_width = column_widths[col_idx];
                                                // Conditionally truncate based on expand mode
                                                let display_name = if self.expand_mode {
                                                    if col.len() > 80 {
                                                        format!("{}...", &col[0..77])
                                                    } else {
                                                        col.clone()
                                                    }
                                                } else {
                                                    let max_chars = ((col_width - 8.0) / 7.5) as usize;
                                                    if col.len() > max_chars && max_chars > 3 {
                                                        format!("{}...", &col[0..max_chars - 3])
                                                    } else {
                                                        col.clone()
                                                    }
                                                };

                                                ui.add_sized(
                                                    [col_width, 22.0],
                                                    egui::Label::new(egui::RichText::new(display_name).heading())
                                                ).on_hover_text(format!("Column: {}", col));
                                            }
                                        }
                                        ui.end_row();
                                    });

                                ui.add_space(4.0);

                                // Inner scroll area: Vertical only with virtual scrolling (scrolls rows, pins headers)
                                let row_height = 22.0;
                                let num_visible_rows = end_idx - start_idx;
                                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
                                    ui,
                                    row_height,
                                    num_visible_rows,
                                    |ui, row_range| {
                                        egui::Grid::new("rows_grid")
                                            .striped(true)
                                            .min_row_height(row_height)
                                            .spacing(egui::vec2(12.0, 4.0))
                                            .show(ui, |ui| {
                                                // Draw only the visible rows in current range
                                                for row_offset in row_range {
                                                    let row_idx = filtered_rows[start_idx + row_offset];
                                                    let actual_row = &table.rows[row_idx];
                                                    for (col_idx, cell) in actual_row.iter().enumerate() {
                                                        let col_name = &table.columns[col_idx];
                                                        if self.hidden_columns.contains(col_name) {
                                                            continue;
                                                        }
                                                        let is_selected = self.selected_cell == Some((row_idx, col_idx));
                                                        
                                                        // Cell label button
                                                        let cell_text = if cell.len() > 30 {
                                                            format!("{}...", &cell[0..27])
                                                        } else {
                                                            cell.clone()
                                                        };

                                                        let col_width = column_widths[col_idx];
                                                        let resp = ui.add_sized(
                                                            [col_width, row_height],
                                                            egui::SelectableLabel::new(is_selected, cell_text)
                                                        );
                                                        if resp.clicked() {
                                                            self.selected_cell = Some((row_idx, col_idx));
                                                            // POST message back to Emacs host!
                                                            emacs_egui_sdk::emacs_post_message(
                                                                "cell-selected",
                                                                serde_json::json!({
                                                                    "row": row_idx,
                                                                    "column": table.columns[col_idx].clone(),
                                                                    "value": cell.clone()
                                                                })
                                                            );
                                                        }
                                                    }
                                                    ui.end_row();
                                                }
                                            });
                                    }
                                );
                            });
                        });
                    }
                    ViewMode::Schema => {
                        ui.columns(2, |columns| {
                            // Column 0: File properties card
                            columns[0].vertical(|ui| {
                                ui.group(|ui| {
                                    ui.heading("📋 Physical Properties");
                                    ui.separator();
                                    
                                    ui.horizontal(|ui| {
                                        ui.strong("Total Rows:");
                                        ui.label(format!("{}", table.stats.total_rows));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.strong("Row Groups:");
                                        ui.label(format!("{}", table.stats.num_row_groups));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.strong("Parquet Version:");
                                        ui.label(format!("{}", table.stats.version));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.strong("Created By:");
                                        ui.label(&table.stats.created_by);
                                    });
                                    ui.add_space(4.0);
                                    ui.strong("Compression Codecs:");
                                    if table.stats.compression_codecs.is_empty() {
                                        ui.label(egui::RichText::new("None").weak());
                                    } else {
                                        for codec in &table.stats.compression_codecs {
                                            ui.label(format!("• {}", codec));
                                        }
                                    }
                                });
                            });

                            // Column 1: Schema descriptor table
                            columns[1].vertical(|ui| {
                                ui.heading("🧬 Schema & Type Discovery");
                                ui.separator();
                                
                                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                                    egui::Grid::new("schema_grid")
                                        .striped(true)
                                        .min_row_height(22.0)
                                        .min_col_width(80.0)
                                        .spacing(egui::vec2(16.0, 6.0))
                                        .show(ui, |ui| {
                                            // Table Header
                                            ui.strong("#");
                                            ui.strong("Column Name");
                                            ui.strong("Physical Type");
                                            ui.strong("Logical Type");
                                            ui.strong("Nullable?");
                                            ui.end_row();

                                            // Table Body
                                            for (idx, field) in table.schema.iter().enumerate() {
                                                ui.label(format!("{}", idx + 1));
                                                ui.label(egui::RichText::new(&field.name).strong());
                                                ui.label(&field.physical_type);
                                                ui.label(&field.logical_type);
                                                
                                                if field.nullable {
                                                    ui.colored_label(egui::Color32::from_rgb(120, 160, 230), "Yes");
                                                } else {
                                                    ui.colored_label(egui::Color32::from_rgb(200, 100, 100), "No");
                                                }
                                                ui.end_row();
                                            }
                                        });
                                });
                            });
                        });
                    }
                }
            } else if !self.loading {
                ui.colored_label(egui::Color32::GRAY, "No Parquet file loaded. Please use M-x emacs-parquet-explorer-open to view a file.");
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn start_app(canvas_id: &str) -> Result<(), JsValue> {
    emacs_egui_sdk::launch_simple(canvas_id, ExplorerApp::new())
}
