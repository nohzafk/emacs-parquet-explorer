use emacs_egui_sdk::{EguiEmacsApp, ThemeColors};
use parquet::file::reader::{FileReader, SerializedFileReader};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
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

#[derive(Clone, Default, Debug)]
pub struct ParquetTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
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

    // 2. Read Rows (cap at 10,000 for high-performance virtual rendering inside WASM)
    let mut rows = Vec::new();
    let row_iter = reader.get_row_iter(None).map_err(|e| e.to_string())?;

    let mut count = 0;
    for record in row_iter {
        if count >= 10000 {
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

    Ok(ParquetTable { columns, rows })
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
            page_size: 100,
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

        // 1. Draw resizable bottom details panel first (if selection exists)
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
            });

            if let Some(ref err) = self.error_message {
                ui.colored_label(egui::Color32::from_rgb(198, 88, 94), format!("Error: {}", err));
            }

            ui.label(egui::RichText::new(format!("File: {}", self.filepath)).weak());

            if let Some(ref table) = self.parquet_table {
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
                    ui.label(format!("Showing {}-{} of {} filtered", start_idx + 1, end_idx, total_filtered));
                    if ui.button("Next ▶").clicked() && self.page_offset + self.page_size < total_filtered {
                        self.page_offset += self.page_size;
                    }

                    ui.add_space(20.0);
                    ui.label("Page Size:");
                    for size in [100, 500, 1000] {
                        if ui.selectable_label(self.page_size == size, format!("{}", size)).clicked() {
                            self.page_size = size;
                            self.page_offset = 0; // reset
                        }
                    }

                    ui.add_space(8.0);
                    ui.label("Custom:");
                    let mut temp_size = self.page_size;
                    let resp = ui.add(egui::DragValue::new(&mut temp_size).range(1..=10000).speed(5.0));
                    if resp.changed() {
                        self.page_size = temp_size;
                        self.page_offset = 0; // reset
                    }
                });

                ui.separator();

                // 2. Data Table Grid
                // Outer scroll area: Horizontal only (synchronizes header + data columns horizontally)
                egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
                    ui.vertical(|ui| {
                        // Sticky Header Grid
                        egui::Grid::new("header_grid")
                            .min_col_width(140.0)
                            .spacing(egui::vec2(12.0, 4.0))
                            .show(ui, |ui| {
                                for col in &table.columns {
                                    ui.heading(col);
                                }
                                ui.end_row();
                            });

                        ui.add_space(4.0);

                        // Inner scroll area: Vertical only (scrolls rows, pins headers)
                        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            egui::Grid::new("rows_grid")
                                .striped(true)
                                .min_row_height(22.0)
                                .min_col_width(140.0)
                                .spacing(egui::vec2(12.0, 4.0))
                                .show(ui, |ui| {
                                    // Draw rows in current page
                                    for (_p_idx, &row_idx) in filtered_rows[start_idx..end_idx].iter().enumerate() {
                                        let actual_row = &table.rows[row_idx];
                                        for (col_idx, cell) in actual_row.iter().enumerate() {
                                            let is_selected = self.selected_cell == Some((row_idx, col_idx));
                                            
                                            // Cell label button
                                            let cell_text = if cell.len() > 30 {
                                                format!("{}...", &cell[0..27])
                                            } else {
                                                cell.clone()
                                            };

                                            let resp = ui.selectable_label(is_selected, cell_text);
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
                        });
                    });
                });
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
