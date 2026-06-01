use emacs_egui_sdk::{EguiEmacsApp, ThemeColors};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReaderBuilder, RowSelection, RowSelector};
use parquet::arrow::ProjectionMask;
use arrow_array::{Array, ArrayRef, StringArray};
use arrow_cast::cast;
use arrow_schema::DataType;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::wasm_bindgen;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

use std::sync::atomic::{AtomicUsize, Ordering};

lazy_static::lazy_static! {
    static ref LOADED_TABLE: Mutex<Option<Result<ParquetTable, String>>> = Mutex::new(None);
    static ref ASYNC_TRIGGERED: Mutex<Option<String>> = Mutex::new(None);
    static ref LOADED_ROWS: Mutex<Option<Result<PageLoadResult, String>>> = Mutex::new(None);
    static ref FILTER_RESULTS: Mutex<Option<Result<FilterResult, String>>> = Mutex::new(None);
}

static LATEST_FILTER_VERSION: AtomicUsize = AtomicUsize::new(0);

/// Wait this long after the last search keystroke before launching a scan, so
/// typing a multi-character query triggers a single scan instead of one per key.
const SEARCH_DEBOUNCE_SECS: f64 = 0.25;


pub struct PageLoadResult {
    pub rows: Vec<Vec<String>>,
    pub indices: Vec<usize>,
}

pub struct FilterResult {
    pub indices: Vec<usize>,
    pub version: usize,
}


#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ExplorerState {
    #[serde(default)]
    pub filepath: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug)]
pub struct ParquetTable {
    pub columns: Vec<String>,
    pub bytes: bytes::Bytes,
    pub schema: Vec<SchemaField>,
    pub stats: FileStats,
    pub compact_widths: Vec<f32>,
    pub expand_widths: Vec<f32>,
}

impl Default for ParquetTable {
    fn default() -> Self {
        Self {
            columns: Vec::new(),
            bytes: bytes::Bytes::new(),
            schema: Vec::new(),
            stats: FileStats::default(),
            compact_widths: Vec::new(),
            expand_widths: Vec::new(),
        }
    }
}

impl ParquetTable {
    pub fn read_rows_subset(&self, indices: &[usize]) -> Result<Vec<Vec<String>>, String> {
        if indices.is_empty() {
            return Ok(Vec::new());
        }

        // Sorted, de-duplicated targets drive a RowSelection so the Arrow reader
        // materializes only the requested rows and skips the rest.
        let mut sorted: Vec<usize> = indices.to_vec();
        sorted.sort_unstable();
        sorted.dedup();

        let reader = ParquetRecordBatchReaderBuilder::try_new(self.bytes.clone())
            .map_err(|e| e.to_string())?
            .with_row_selection(row_selection_for(&sorted))
            .with_batch_size(1024)
            .build()
            .map_err(|e| e.to_string())?;

        // Selected rows arrive in ascending file order, i.e. the order of `sorted`.
        let mut sorted_rows: Vec<Vec<String>> = Vec::with_capacity(sorted.len());
        for batch in reader {
            let batch = batch.map_err(|e| e.to_string())?;
            let utf8 = batch_to_utf8(&batch)?;
            let cols = downcast_utf8(&utf8);
            for r in 0..batch.num_rows() {
                sorted_rows.push((0..cols.len()).map(|c| utf8_cell(&cols, r, c).to_string()).collect());
            }
        }

        if sorted_rows.len() != sorted.len() {
            return Err(format!(
                "row subset decode mismatch: requested {} rows, decoded {}",
                sorted.len(),
                sorted_rows.len()
            ));
        }

        // Map the ascending results back to the caller's requested order.
        let mut pos = std::collections::HashMap::with_capacity(sorted.len());
        for (k, &g) in sorted.iter().enumerate() {
            pos.insert(g, k);
        }
        Ok(indices.iter().map(|&g| sorted_rows[pos[&g]].clone()).collect())
    }
}

/// Build a RowSelection that selects exactly the given ascending, de-duplicated
/// global row indices, coalescing consecutive runs into single selectors.
fn row_selection_for(sorted: &[usize]) -> RowSelection {
    let mut selectors = Vec::new();
    let mut prev_end = 0usize;
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start + 1;
        let mut j = i + 1;
        while j < sorted.len() && sorted[j] == end {
            end += 1;
            j += 1;
        }
        if start > prev_end {
            selectors.push(RowSelector::skip(start - prev_end));
        }
        selectors.push(RowSelector::select(end - start));
        prev_end = end;
        i = j;
    }
    RowSelection::from(selectors)
}

/// Cast every column of a batch to Utf8 once (vectorized) so values can be read
/// and searched as text with consistent formatting.
fn batch_to_utf8(batch: &arrow_array::RecordBatch) -> Result<Vec<ArrayRef>, String> {
    batch
        .columns()
        .iter()
        .map(|c| cast(c, &DataType::Utf8).map_err(|e| e.to_string()))
        .collect()
}

/// Borrow each cast column as a concrete StringArray for fast cell access.
fn downcast_utf8(cols: &[ArrayRef]) -> Vec<&StringArray> {
    cols.iter()
        .map(|a| a.as_any().downcast_ref::<StringArray>().expect("cast to Utf8 yields StringArray"))
        .collect()
}

/// Read cell (row, col) as text, rendering null as "null".
#[inline]
fn utf8_cell<'a>(cols: &[&'a StringArray], row: usize, col: usize) -> &'a str {
    let a = cols[col];
    if a.is_null(row) {
        "null"
    } else {
        a.value(row)
    }
}

pub fn parse_parquet(bytes: Vec<u8>) -> Result<ParquetTable, String> {
    let bytes = bytes::Bytes::from(bytes);
    let reader = SerializedFileReader::new(bytes.clone()).map_err(|e| e.to_string())?;

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

    // 2. Read first 200 rows to compute column widths dynamically
    let mut sample_rows = Vec::new();
    if let Ok(mut row_iter) = reader.get_row_iter(None) {
        for _ in 0..200 {
            if let Some(Ok(record)) = row_iter.next() {
                let mut row = Vec::new();
                for (_, field) in record.get_column_iter() {
                    row.push(format!("{}", field));
                }
                sample_rows.push(row);
            } else {
                break;
            }
        }
    }

    // 3. Compute Column Widths dynamically for both Compact and Expand modes
    let mut compact_widths = Vec::new();
    let mut expand_widths = Vec::new();
    for col_idx in 0..columns.len() {
        let col_name_len = columns[col_idx].len();
        
        // Calculate compact width (data cell lengths only)
        let mut max_cell_len = 0;
        let check_rows = sample_rows.len().min(200);
        for row in sample_rows.iter().take(check_rows) {
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
        bytes,
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

    // Front Buffer for active page rows
    active_rows: Vec<Vec<String>>,
    // Indices currently stored in active_rows
    active_indices: Vec<usize>,
    // Indicator that background page load is in progress
    rows_loading: bool,
    
    // Predicate search/filtering variables
    filtered_indices: Vec<usize>,
    filtering_in_progress: bool,
    filter_version: usize,
    
    // Tracking for state changes to trigger requests
    last_requested_indices: Vec<usize>,
    last_search_query: String,
    last_filters: Vec<ColumnFilter>,

    // Debounce state for the search box: a pending text change and the egui
    // timestamp (seconds) of the most recent edit.
    search_pending: bool,
    search_edit_time: f64,

    // Query/filters that produced the current `filtered_indices` (for
    // incremental narrowing), and those of the in-flight scan.
    applied_query: String,
    applied_filters: Vec<ColumnFilter>,
    scanning_query: String,
    scanning_filters: Vec<ColumnFilter>,
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

            active_rows: Vec::new(),
            active_indices: Vec::new(),
            rows_loading: false,
            
            filtered_indices: Vec::new(),
            filtering_in_progress: false,
            filter_version: 0,
            
            last_requested_indices: Vec::new(),
            last_search_query: String::new(),
            last_filters: Vec::new(),

            search_pending: false,
            search_edit_time: 0.0,

            applied_query: String::new(),
            applied_filters: Vec::new(),
            scanning_query: String::new(),
            scanning_filters: Vec::new(),
        }
    }

    fn trigger_filter_update(&mut self) {
        let Some(ref table) = self.parquet_table else { return; };
        let new_version = self.filter_version + 1;
        self.filter_version = new_version;
        LATEST_FILTER_VERSION.store(new_version, Ordering::SeqCst);
        
        self.page_offset = 0; // reset paging
        self.selected_cell = None; // clear selected cell

        if self.search_query.is_empty() && self.filters.is_empty() {
            self.filtered_indices = (0..table.stats.total_rows as usize).collect();
            self.filtering_in_progress = false;
            self.applied_query = String::new();
            self.applied_filters = Vec::new();
            return;
        }

        // Incremental narrowing: if the filters are unchanged and the new query
        // merely extends the applied one (the applied query is a substring of
        // it), every new match is already inside the current result set, so we
        // re-scan only those rows. Requires a proper-subset base (a prior query
        // and/or filters); otherwise fall back to a full scan.
        let incremental = self.filters == self.applied_filters
            && self.search_query.contains(&self.applied_query)
            && (!self.applied_query.is_empty() || !self.applied_filters.is_empty());
        let base = incremental.then(|| self.filtered_indices.clone());

        self.filtering_in_progress = true;
        self.scanning_query = self.search_query.clone();
        self.scanning_filters = self.filters.clone();

        let table_cloned = table.clone();
        let query_cloned = self.search_query.clone();
        let filters_cloned = self.filters.clone();
        let version = new_version;

        emacs_egui_sdk::wasm_bindgen_futures::spawn_local(async move {
            let scan_res =
                scan_and_filter_indices(table_cloned, query_cloned, filters_cloned, base, version)
                    .await;
            if let Ok(mut guard) = FILTER_RESULTS.lock() {
                *guard = Some(scan_res);
            }
        });
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
                        self.trigger_filter_update();
                        self.error_message = None;
                    }
                    Err(err) => {
                        self.error_message = Some(err);
                    }
                }
            }
        }

        if self.parquet_table.is_some() {
            let now = ctx.input(|i| i.time);

            if self.filters != self.last_filters {
                // Filters are added/removed via discrete clicks -> apply at once.
                self.last_filters = self.filters.clone();
                self.last_search_query = self.search_query.clone();
                self.search_pending = false;
                self.trigger_filter_update();
            } else if self.search_query != self.last_search_query {
                // Text changed this frame: (re)start the debounce timer and make
                // sure egui wakes up again once the window has elapsed.
                self.last_search_query = self.search_query.clone();
                self.search_pending = true;
                self.search_edit_time = now;
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(SEARCH_DEBOUNCE_SECS));
            } else if self.search_pending && (now - self.search_edit_time) >= SEARCH_DEBOUNCE_SECS {
                self.search_pending = false;
                self.trigger_filter_update();
            }
        }

        if let Ok(mut guard) = FILTER_RESULTS.lock() {
            if let Some(res) = guard.take() {
                match res {
                    Ok(filter_result) => {
                        if filter_result.version == self.filter_version {
                            self.filtered_indices = filter_result.indices;
                            self.filtering_in_progress = false;
                            self.applied_query = self.scanning_query.clone();
                            self.applied_filters = self.scanning_filters.clone();
                        }
                    }
                    Err(err) => {
                        if err != "Aborted" {
                            self.error_message = Some(err);
                            self.filtering_in_progress = false;
                        }
                    }
                }
            }
        }

        let mut target_indices = Vec::new();
        if let Some(ref table) = self.parquet_table {
            let start = self.page_offset;
            let end = (start + self.page_size).min(self.filtered_indices.len());
            if start < end {
                target_indices = self.filtered_indices[start..end].to_vec();
            }

            if !target_indices.is_empty()
                && target_indices != self.active_indices
                && target_indices != self.last_requested_indices
                && !self.rows_loading
            {
                self.rows_loading = true;
                self.last_requested_indices = target_indices.clone();

                let table_cloned = table.clone();
                let indices_cloned = target_indices.clone();

                emacs_egui_sdk::wasm_bindgen_futures::spawn_local(async move {
                    let result = table_cloned.read_rows_subset(&indices_cloned);
                    if let Ok(mut guard) = LOADED_ROWS.lock() {
                        match result {
                            Ok(rows) => {
                                *guard = Some(Ok(PageLoadResult {
                                    rows,
                                    indices: indices_cloned,
                                }));
                            }
                            Err(err) => {
                                *guard = Some(Err(err));
                            }
                        }
                    }
                });
            }
        }

        if let Ok(mut guard) = LOADED_ROWS.lock() {
            if let Some(res) = guard.take() {
                self.rows_loading = false;
                match res {
                    Ok(page_result) => {
                        if page_result.indices == target_indices {
                            self.active_rows = page_result.rows;
                            self.active_indices = page_result.indices;
                        }
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
                if let Some((row_idx, col)) = self.selected_cell {
                    let cell_value = if let Some(local_offset) = self.active_indices.iter().position(|&idx| idx == row_idx) {
                        if local_offset < self.active_rows.len() && col < self.active_rows[local_offset].len() {
                            self.active_rows[local_offset][col].clone()
                        } else {
                            String::new()
                        }
                    } else {
                        "Loading...".to_string()
                    };
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
                if self.loading || self.filtering_in_progress || self.rows_loading {
                    ui.spinner();
                    if self.loading {
                        ui.label("Streaming binary...");
                    } else if self.filtering_in_progress {
                        ui.label("Filtering 3M+ rows...");
                    } else {
                        ui.label("Loading page...");
                    }
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
                    ui.selectable_value(&mut self.view_mode, ViewMode::Data, "Data View");
                    ui.selectable_value(&mut self.view_mode, ViewMode::Schema, "Schema & Metadata");

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
                        let filtered_rows = &self.filtered_indices;
                        let total_filtered = filtered_rows.len();

                        // Statistics & Filters
                        ui.horizontal(|ui| {
                            let cols_label = if self.hidden_columns.is_empty() {
                                format!("Columns: {}", table.columns.len())
                            } else {
                                format!("Columns: {} ({} hidden)", table.columns.len(), self.hidden_columns.len())
                            };
                            
                            let rows_label = if self.filters.is_empty() && self.search_query.is_empty() {
                                format!("Rows: {}", table.stats.total_rows)
                            } else {
                                format!("Rows: {} (filtered to {})", table.stats.total_rows, total_filtered)
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
                                                    if row_offset >= self.active_rows.len() {
                                                        continue;
                                                    }
                                                    let row_idx = filtered_rows[start_idx + row_offset];
                                                    let actual_row = &self.active_rows[row_offset];
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
                                ui.heading("Physical Properties");
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

                            // Column 1: Schema descriptor table
                            columns[1].vertical(|ui| {
                                ui.heading("Schema & Type Discovery");
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

/// ASCII case-insensitive substring test that allocates nothing. `needle` must
/// already be lowercased by the caller. Non-ASCII bytes are compared verbatim,
/// so matching is Unicode-case-sensitive only for non-ASCII text (acceptable
/// for tabular data and far cheaper than allocating `to_lowercase()` per cell).
fn ascii_contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.len() > h.len() {
        return false;
    }
    (0..=h.len() - n.len()).any(|start| {
        h[start..start + n.len()]
            .iter()
            .zip(n)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// Decide whether a single decoded row passes the global search query and every
/// column filter. `query` is expected pre-lowercased (ASCII).
fn row_matches<S: AsRef<str>>(
    row: &[S],
    query: &str,
    filters: &[ColumnFilter],
    columns: &[String],
) -> bool {
    if !query.is_empty()
        && !row
            .iter()
            .any(|cell| ascii_contains_ignore_case(cell.as_ref(), query))
    {
        return false;
    }

    for filter in filters {
        let Some(col_idx) = columns.iter().position(|c| c == &filter.column) else {
            continue;
        };
        if col_idx >= row.len() {
            return false;
        }

        let cell_val = row[col_idx].as_ref();
        let f_val = &filter.value;
        let cell_num = cell_val.trim().parse::<f64>();
        let filter_num = f_val.trim().parse::<f64>();

        let ok = match (cell_num, filter_num) {
            (Ok(c_n), Ok(f_n)) => match filter.operator.as_str() {
                "=" => (c_n - f_n).abs() < 1e-9,
                ">" => c_n > f_n,
                "<" => c_n < f_n,
                ">=" => c_n >= f_n,
                "<=" => c_n <= f_n,
                "contains" => ascii_contains_ignore_case(cell_val, &f_val.to_ascii_lowercase()),
                _ => false,
            },
            _ => {
                let (cl, fl) = (cell_val.to_ascii_lowercase(), f_val.to_ascii_lowercase());
                match filter.operator.as_str() {
                    "=" => cl == fl,
                    "contains" => ascii_contains_ignore_case(cell_val, &fl),
                    ">" => cl > fl,
                    "<" => cl < fl,
                    ">=" => cl >= fl,
                    "<=" => cl <= fl,
                    _ => false,
                }
            }
        };

        if !ok {
            return false;
        }
    }

    true
}

/// Columns the scan must decode: every column for a global text query, or just
/// the filtered columns for a filter-only scan. Returned ascending & unique so
/// the projected batch column order lines up with the names.
fn projection_for(table: &ParquetTable, query: &str, filters: &[ColumnFilter]) -> Vec<usize> {
    let mut proj: Vec<usize> = if query.is_empty() {
        filters
            .iter()
            .filter_map(|f| table.columns.iter().position(|c| c == &f.column))
            .collect()
    } else {
        (0..table.columns.len()).collect()
    };
    proj.sort_unstable();
    proj.dedup();
    proj
}

async fn scan_and_filter_indices(
    table: ParquetTable,
    search_query: String,
    filters: Vec<ColumnFilter>,
    base: Option<Vec<usize>>,
    version: usize,
) -> Result<FilterResult, String> {
    let query = search_query.to_ascii_lowercase();
    let mut filtered_indices = Vec::new();

    let builder =
        ParquetRecordBatchReaderBuilder::try_new(table.bytes.clone()).map_err(|e| e.to_string())?;

    let proj_cols = projection_for(&table, &query, &filters);
    if proj_cols.is_empty() {
        // No column to scan against -> nothing further constrains the result.
        return Ok(FilterResult {
            indices: base.unwrap_or_else(|| (0..table.stats.total_rows as usize).collect()),
            version,
        });
    }

    // Projected batches keep columns in ascending schema order, so the names
    // line up with the projected column order for row_matches.
    let proj_names: Vec<String> = proj_cols.iter().map(|&i| table.columns[i].clone()).collect();
    let mask = ProjectionMask::leaves(builder.parquet_schema(), proj_cols.iter().cloned());

    // Incremental narrowing: restrict the reader to the prior candidate rows via
    // a RowSelection. Selected rows then arrive in ascending order, so the k-th
    // decoded row maps back to base[k]. A full scan maps row r -> global r.
    let base_sorted = base.map(|mut b| {
        b.sort_unstable();
        b.dedup();
        b
    });

    let mut builder = builder.with_projection(mask).with_batch_size(8192);
    if let Some(ref sorted) = base_sorted {
        builder = builder.with_row_selection(row_selection_for(sorted));
    }
    let reader = builder.build().map_err(|e| e.to_string())?;

    let mut cursor = 0usize;
    let mut since_yield = 0usize;
    for batch in reader {
        if LATEST_FILTER_VERSION.load(Ordering::SeqCst) > version {
            return Err("Aborted".to_string());
        }

        let batch = batch.map_err(|e| e.to_string())?;
        let nrows = batch.num_rows();
        let utf8 = batch_to_utf8(&batch)?;
        let cols = downcast_utf8(&utf8);

        let mut row_buf: Vec<&str> = Vec::with_capacity(cols.len());
        for r in 0..nrows {
            row_buf.clear();
            for c in 0..cols.len() {
                row_buf.push(utf8_cell(&cols, r, c));
            }
            if row_matches(&row_buf, &query, &filters, &proj_names) {
                let global = match base_sorted {
                    Some(ref s) => s[cursor + r],
                    None => cursor + r,
                };
                filtered_indices.push(global);
            }
        }

        cursor += nrows;
        since_yield += nrows;
        if since_yield >= 25000 {
            since_yield = 0;
            yield_to_browser().await;
        }
    }

    Ok(FilterResult {
        indices: filtered_indices,
        version,
    })
}

/// Scan a contiguous global row range `[start, end)` via a projected reader with
/// a RowSelection, returning the matching global indices (ascending). Native
/// helper for the parallel scanner.
#[cfg(not(target_arch = "wasm32"))]
fn scan_range(
    bytes: &bytes::Bytes,
    query: &str,
    filters: &[ColumnFilter],
    proj_cols: &[usize],
    proj_names: &[String],
    start: usize,
    end: usize,
) -> Result<Vec<usize>, String> {
    if start >= end {
        return Ok(Vec::new());
    }

    let builder =
        ParquetRecordBatchReaderBuilder::try_new(bytes.clone()).map_err(|e| e.to_string())?;
    let mask = ProjectionMask::leaves(builder.parquet_schema(), proj_cols.iter().cloned());

    let mut selectors = Vec::new();
    if start > 0 {
        selectors.push(RowSelector::skip(start));
    }
    selectors.push(RowSelector::select(end - start));

    let reader = builder
        .with_projection(mask)
        .with_row_selection(RowSelection::from(selectors))
        .with_batch_size(8192)
        .build()
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    let mut cursor = start; // first selected row maps to global index `start`
    for batch in reader {
        let batch = batch.map_err(|e| e.to_string())?;
        let utf8 = batch_to_utf8(&batch)?;
        let cols = downcast_utf8(&utf8);
        let mut row_buf: Vec<&str> = Vec::with_capacity(cols.len());
        for r in 0..batch.num_rows() {
            row_buf.clear();
            for c in 0..cols.len() {
                row_buf.push(utf8_cell(&cols, r, c));
            }
            if row_matches(&row_buf, query, filters, proj_names) {
                out.push(cursor + r);
            }
        }
        cursor += batch.num_rows();
    }
    Ok(out)
}

/// Multi-threaded full scan for the native sidecar: split the row range into
/// `threads` contiguous slices, scan them in parallel with rayon, and merge the
/// (already ascending) partial results. Not available in the wasm build.
#[cfg(not(target_arch = "wasm32"))]
pub fn scan_parallel(
    table: &ParquetTable,
    search_query: &str,
    filters: &[ColumnFilter],
    threads: usize,
) -> Result<Vec<usize>, String> {
    use rayon::prelude::*;

    let query = search_query.to_ascii_lowercase();
    let total = table.stats.total_rows as usize;
    let proj_cols = projection_for(table, &query, filters);
    if proj_cols.is_empty() || total == 0 {
        return Ok((0..total).collect());
    }
    let proj_names: Vec<String> = proj_cols.iter().map(|&i| table.columns[i].clone()).collect();

    let threads = threads.max(1);
    let chunk = total.div_ceil(threads);
    let ranges: Vec<(usize, usize)> = (0..threads)
        .map(|t| (t * chunk, ((t + 1) * chunk).min(total)))
        .filter(|(s, e)| s < e)
        .collect();

    // collect() preserves input order, so concatenating the ascending,
    // contiguous slices yields globally ascending indices.
    let partials: Result<Vec<Vec<usize>>, String> = ranges
        .par_iter()
        .map(|&(s, e)| scan_range(&table.bytes, &query, filters, &proj_cols, &proj_names, s, e))
        .collect();

    let mut out = Vec::new();
    for part in partials? {
        out.extend(part);
    }
    Ok(out)
}

async fn yield_to_browser() {
    #[cfg(target_arch = "wasm32")]
    {
        let promise = js_sys::Promise::resolve(&wasm_bindgen::JsValue::undefined());
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cf(column: &str, operator: &str, value: &str) -> ColumnFilter {
        ColumnFilter {
            column: column.to_string(),
            operator: operator.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn test_ascii_contains_ignore_case() {
        assert!(ascii_contains_ignore_case("Hello World", "hello"));
        assert!(ascii_contains_ignore_case("Hello World", "world"));
        assert!(ascii_contains_ignore_case("ABC123", "c12"));
        assert!(ascii_contains_ignore_case("anything", "")); // empty needle matches
        assert!(!ascii_contains_ignore_case("abc", "abcd")); // needle longer
        assert!(!ascii_contains_ignore_case("Hello", "xyz"));
    }

    #[test]
    fn test_row_matches_global_search() {
        let columns = vec!["name".to_string(), "fare".to_string()];
        let row = vec!["Alice".to_string(), "23.50".to_string()];

        // Case-insensitive substring across any column.
        assert!(row_matches(&row, "ali", &[], &columns));
        assert!(row_matches(&row, "23.5", &[], &columns));
        assert!(!row_matches(&row, "bob", &[], &columns));
        // Empty query matches everything.
        assert!(row_matches(&row, "", &[], &columns));
    }

    #[test]
    fn test_row_matches_numeric_and_text_filters() {
        let columns = vec!["name".to_string(), "fare".to_string()];
        let row = vec!["Alice".to_string(), "23.50".to_string()];

        // Numeric comparisons parse both sides as f64.
        assert!(row_matches(&row, "", &[cf("fare", ">", "10")], &columns));
        assert!(row_matches(&row, "", &[cf("fare", "<", "100")], &columns));
        assert!(!row_matches(&row, "", &[cf("fare", ">", "30")], &columns));
        assert!(row_matches(&row, "", &[cf("fare", "=", "23.5")], &columns));

        // Text "contains" filter is case-insensitive.
        assert!(row_matches(&row, "", &[cf("name", "contains", "LIC")], &columns));

        // Query AND filter both apply (conjunction).
        assert!(row_matches(&row, "ali", &[cf("fare", ">", "10")], &columns));
        assert!(!row_matches(&row, "bob", &[cf("fare", ">", "10")], &columns));
    }

    /// Minimal executor: the scan only awaits the (native no-op) yield point, so
    /// it completes on the first poll.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        fn noop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    /// Stream the whole file via Arrow, render rows, apply row_matches -- an
    /// independent reference for what scan_and_filter_indices should return.
    fn reference_scan(table: &ParquetTable, query: &str, filters: &[ColumnFilter]) -> Vec<usize> {
        let q = query.to_ascii_lowercase();
        let reader = ParquetRecordBatchReaderBuilder::try_new(table.bytes.clone())
            .unwrap()
            .with_batch_size(8192)
            .build()
            .unwrap();
        let mut out = Vec::new();
        let mut off = 0usize;
        for batch in reader {
            let batch = batch.unwrap();
            let utf8 = batch_to_utf8(&batch).unwrap();
            let cols = downcast_utf8(&utf8);
            for r in 0..batch.num_rows() {
                let row: Vec<&str> = (0..cols.len()).map(|c| utf8_cell(&cols, r, c)).collect();
                if row_matches(&row, &q, filters, &table.columns) {
                    out.push(off + r);
                }
            }
            off += batch.num_rows();
        }
        out
    }

    #[test]
    #[ignore]
    fn bench_scan_serial_vs_parallel() {
        use std::time::Instant;
        let table = parse_parquet(fs::read("../yellow_tripdata_2023-01.parquet").unwrap()).unwrap();
        let query = "n";

        let t = Instant::now();
        let serial = scan_parallel(&table, query, &[], 1).unwrap();
        let serial_ms = t.elapsed().as_millis();

        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let t = Instant::now();
        let parallel = scan_parallel(&table, query, &[], threads).unwrap();
        let par_ms = t.elapsed().as_millis();

        assert_eq!(serial, parallel, "parallel split must equal serial scan");
        println!("\nserial(1):    {} ms ({} matches)", serial_ms, serial.len());
        println!("parallel({}): {} ms ({} matches)", threads, par_ms, parallel.len());
    }

    #[test]
    fn test_scan_parallel_matches_reference() {
        let table = parse_parquet(fs::read("../yellow_tripdata_2023-01.parquet").unwrap()).unwrap();
        // Native parallel scan must agree with the single-threaded reference for
        // both a global query and a column filter.
        let got = scan_parallel(&table, "2023", &[], 4).unwrap();
        assert_eq!(got, reference_scan(&table, "2023", &[]));

        let filters = vec![cf("passenger_count", ">", "4")];
        let got = scan_parallel(&table, "", &filters, 4).unwrap();
        assert_eq!(got, reference_scan(&table, "", &filters));
    }

    #[test]
    fn test_scan_matches_arrow_reference() {
        let path = "../yellow_tripdata_2023-01.parquet";
        let table = parse_parquet(fs::read(path).expect("read parquet")).expect("parse");
        LATEST_FILTER_VERSION.store(0, Ordering::SeqCst);

        // Global text query: "2023" appears in (nearly) every timestamp, so it
        // exercises the all-columns projection and timestamp rendering and should
        // match the vast majority of rows. (A handful of dirty records carry
        // out-of-range years, so it is not literally every row.)
        let got = block_on(scan_and_filter_indices(table.clone(), "2023".into(), vec![], None, 0)).unwrap().indices;
        assert_eq!(got, reference_scan(&table, "2023", &[]));
        assert!(got.len() > table.stats.total_rows as usize - 1000 && !got.is_empty());

        // Incremental narrowing: extending "2023" -> "2023-" from the prior
        // result set must match a full scan, since the matches are a subset.
        let inc = block_on(scan_and_filter_indices(table.clone(), "2023-".into(), vec![], Some(got.clone()), 0)).unwrap().indices;
        let full = block_on(scan_and_filter_indices(table.clone(), "2023-".into(), vec![], None, 0)).unwrap().indices;
        assert_eq!(inc, full);

        // Filter-only scan exercises the projected (single-column) path.
        let filters = vec![cf("passenger_count", ">", "4")];
        let got = block_on(scan_and_filter_indices(table.clone(), String::new(), filters.clone(), None, 0)).unwrap().indices;
        assert_eq!(got, reference_scan(&table, "", &filters));
        assert!(got.len() < table.stats.total_rows as usize, "filter should exclude some rows");
    }

    #[test]
    fn test_parquet_lazy_loading() {
        let path = "../yellow_tripdata_2023-01.parquet";
        let bytes = fs::read(path).expect("Failed to read Parquet test file");

        let table = parse_parquet(bytes).expect("Failed to parse Parquet metadata");

        assert_eq!(table.stats.total_rows, 3066766);
        assert_eq!(table.stats.num_row_groups, 1);
        assert_eq!(table.columns.len(), 19);

        let subset_rows = table.read_rows_subset(&[0, 1, 2]).expect("Failed to read contiguous subset");
        assert_eq!(subset_rows.len(), 3);
        assert_eq!(subset_rows[0].len(), 19);

        let target_indices = vec![0, 1500000, 3000000, 100, 2000000];
        let decoded_subset = table.read_rows_subset(&target_indices).expect("Failed to read non-contiguous subset");
        
        assert_eq!(decoded_subset.len(), 5);
        for row in &decoded_subset {
            assert_eq!(row.len(), 19);
        }

        let row_0 = table.read_rows_subset(&[0]).unwrap();
        let row_1500000 = table.read_rows_subset(&[1500000]).unwrap();
        
        assert_eq!(decoded_subset[0], row_0[0]);
        assert_eq!(decoded_subset[1], row_1500000[0]);
    }
}


