use emacs_egui_sdk::{EguiEmacsApp, ThemeColors};
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use wasm_bindgen::prelude::*;

lazy_static::lazy_static! {
    static ref LOADED_TABLE: Mutex<Option<Result<ParquetTable, String>>> = Mutex::new(None);
    static ref ASYNC_TRIGGERED: Mutex<Option<String>> = Mutex::new(None);
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ExplorerState {
    #[serde(default)]
    pub filepath: String,
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
        let max_expand_len = col_name_len.max(max_cell_len).min(35);
        let expand_width = (max_expand_len as f32 * 8.0).clamp(80.0, 320.0);
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

async fn fetch_bytes(url: &str) -> Result<Vec<u8>, JsValue> {
    use wasm_bindgen::JsCast;
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let resp_value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await?;
    let resp: web_sys::Response = resp_value.dyn_into()?;
    
    if !resp.ok() {
        return Err(JsValue::from_str(&format!("HTTP status {}", resp.status())));
    }
    
    let array_buffer_value = wasm_bindgen_futures::JsFuture::from(resp.array_buffer()?).await?;
    let array_buffer: js_sys::ArrayBuffer = array_buffer_value.dyn_into()?;
    let typed_array = js_sys::Uint8Array::new(&array_buffer);
    
    let mut bytes = vec![0; typed_array.length() as usize];
    typed_array.copy_to(&mut bytes);
    Ok(bytes)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Data,
    Schema,
}

pub struct ExplorerApp {
    filepath: String,
    session_id: String,
    port: u16,
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
}

impl ExplorerApp {
    pub fn new(session_id: String, port: u16) -> Self {
        Self {
            filepath: String::new(),
            session_id,
            port,
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

        let filepath = state.filepath.clone();
        let port = self.port;

        // Fetch file in async task
        wasm_bindgen_futures::spawn_local(async move {
            let encoded = js_sys::encode_uri_component(&filepath);
            let url = format!("http://127.0.0.1:{}/api/file?path={}", port, encoded);
            
            match fetch_bytes(&url).await {
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
                    egui::TopBottomPanel::bottom("details_panel")
                        .resizable(true)
                        .default_height(90.0)
                        .min_height(60.0)
                        .show(ctx, |ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);
                            ui.separator();
                            ui.horizontal(|ui| {
                                ui.heading("🔍 Selected Cell Details");
                                ui.label(egui::RichText::new(format!("Column: {}", table.columns[col])).weak());
                            });
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                ui.label(&table.rows[row][col]);
                            });
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
                });
                ui.add_space(4.0);

                match self.view_mode {
                    ViewMode::Data => {
                        // Statistics & Filters
                        ui.horizontal(|ui| {
                            ui.label(format!("Columns: {}  |  Rows: {}", table.columns.len(), table.rows.len()));
                            ui.add_space(20.0);
                            ui.label("Search:");
                            ui.text_edit_singleline(&mut self.search_query);
                        });



                        // Filter row indices matching search
                        let filtered_rows: Vec<usize> = if self.search_query.is_empty() {
                            (0..table.rows.len()).collect()
                        } else {
                            let query = self.search_query.to_lowercase();
                            (0..table.rows.len())
                                .filter(|&i| {
                                    table.rows[i].iter().any(|cell| cell.to_lowercase().contains(&query))
                                })
                                .collect()
                        };

                        // Paging Control
                        let total_filtered = filtered_rows.len();
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
                            ui.collapsing("📐 Column Visibility & Pruning", |ui| {
                                ui.horizontal_wrapped(|ui| {
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
                                                // Allocate exact size to guarantee the grid column takes up exactly `col_width` pixels
                                                let (rect, _) = ui.allocate_exact_size(
                                                    egui::vec2(col_width, 24.0),
                                                    egui::Sense::hover()
                                                );
                                                ui.allocate_ui_at_rect(rect, |ui| {
                                                    ui.horizontal(|ui| {
                                                        ui.add_space(6.0); // left padding matching selectable label
                                                        ui.add_sized(
                                                            [col_width - 12.0, 20.0],
                                                            egui::Label::new(egui::RichText::new(col).heading()).truncate()
                                                        );
                                                    });
                                                });
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
    // Read session parameters from location fragment on load
    let session_id = if let Some(win) = web_sys::window() {
        let hash = win.location().hash().unwrap_or_default().replace("#", "");
        let params = web_sys::UrlSearchParams::new_with_str(&hash).unwrap();
        params.get("session").unwrap_or_else(|| "default-session".to_string())
    } else {
        "default-session".to_string()
    };

    let port = if let Some(win) = web_sys::window() {
        let hash = win.location().hash().unwrap_or_default().replace("#", "");
        let params = web_sys::UrlSearchParams::new_with_str(&hash).unwrap();
        params.get("port").and_then(|p| p.parse::<u16>().ok()).unwrap_or(8080)
    } else {
        8080
    };

    let app = ExplorerApp::new(session_id, port);
    emacs_egui_sdk::bootstrap_app(app, canvas_id)
}
