# emacs-parquet-explorer

[![Framework](https://img.shields.io/badge/Framework-emacs--egui-8A2BE2.svg?style=flat-square)](https://github.com/emacs-egui/emacs-egui)
[![Rust Version](https://img.shields.io/badge/Rust-2021_Edition-orange.svg?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![Target](https://img.shields.io/badge/Target-WebAssembly-blue.svg?style=flat-square&logo=webassembly)](https://webassembly.org/)
[![Performance](https://img.shields.io/badge/Performance-Virtual_Grid-brightgreen.svg?style=flat-square)](https://github.com/emacs-egui/emacs-parquet-explorer)
[![License](https://img.shields.io/badge/License-MIT-green.svg?style=flat-square)](LICENSE)

An interactive, GPU-accelerated visual data browser and query tool for large Parquet files, built inside Emacs using Rust and **egui** WebAssembly.

Layered on top of the generic [emacs-egui](https://github.com/nohzafk/emacs-egui) host framework, this package brings database-client-grade performance, fluid virtual scrolling, and high-volume analytics directly within a standard Emacs buffer.

---

## 🌟 Key Features

1. **High-Volume In-Memory Parsing:** Streams local binary Parquet datasets over the framework's file gateway and parses metadata, schemas, and row groups in memory using pure, lightning-fast Rust Arrow/Parquet APIs.
2. **Adaptive Layout & Responsive Grid:** Automatically scales to 100% of the active Emacs window height and width using nested horizontal and vertical `ScrollArea` containers.
3. **Sticky Column Headers:** Columns lock at the top of the viewport during vertical scrolling, while sliding in lockstep horizontally across extremely wide schemas (fully tested on 19+ columns).
4. **Configurable Paging Presets:** Paginates datasets dynamically with instant presets (`100`, `500`, `1000` rows) or a custom `DragValue` input (double-click to type any row count limit).
5. **Global Text Substring Search:** Real-time, case-insensitive global text filtering matching substrings across all cells in every column.
6. **Dynamic Column Visibility:** Interactive checklist panel to show, hide, or prune columns dynamically to focus on key attributes.
7. **Predicate Pushdown & Cell Filtering:** Quick-filtering and column-specific predicate pushdowns to isolate anomalies and inspect unique records instantly.
8. **Interactive Clipboard Integration:** Selecting any cell displays its full detailed content in a resizable bottom panel and copies the cell value instantly into the Emacs `kill-ring` clipboard.
9. **Native Asynchronous CSV Export:** Direct background export of massive Parquet datasets into clean CSV files, running non-blockingly via an Elisp process wrapper.

---

## 🏛️ How It Works (Framework Integration)

`emacs-parquet-explorer` leverages the [emacs-egui](https://github.com/nohzafk/emacs-egui) framework for asset hosting, secure data streaming, and bidirectional Elisp-to-Rust communication:

```text
  +--------------------------------------------------------------------------+
  |                          Emacs Lisp Controller                           |
  |  - `emacs-parquet-explorer-open` registers the WASM application.         |
  |  - Binds "cell-selected" hook -> copies string to Emacs `kill-ring`.     |
  |  - Binds "export-csv" hook -> starts native asynchronous CLI process.    |
  +-------------------------------------+------------------------------------+
                                        | (1) filepath state push
                                        v
  +--------------------------------------------------------------------------+
  |                       emacs-egui Asset Server                            |
  |  - /app/emacs-parquet-explorer/ index.html & WASM bundles.               |
  |  - Streams raw Parquet binaries via secure gateway: /api/file?path=      |
  +-------------------------------------+------------------------------------+
                                        | (2) binary stream
                                        v
  +--------------------------------------------------------------------------+
  |                      Rust WebAssembly App (egui)                         |
  |  - Decodes binary streams into Arrow RecordBatch containers.             |
  |  - Performs column pruning, text filtering, and virtual grid rendering.   |
  +--------------------------------------------------------------------------+
```

### Direct Callback Hook Integration

- **Emacs Clipboard Sync:** When a user selects a cell inside the grid, egui triggers the `cell-selected` event. The Elisp layer catches the action, extracts the value, pushes it onto the `kill-ring`, and outputs a clean minibuffer message.
- **Asynchronous CSV Export:** When the user clicks "Export CSV" inside the egui layout, it triggers the `export-csv` event. Emacs prompts the user for a destination path, resolves the absolute paths, and invokes `cargo run --bin parquet_to_csv` via an asynchronous process (`make-process`), keeping the Emacs UI completely responsive during massive exports.

---

## ⚙️ Requirements

- **Emacs 29.1+** built **with xwidget support** (`(featurep 'xwidget-internal)`).
- The [emacs-egui](https://github.com/nohzafk/emacs-egui) framework (bundled as a git submodule -- no separate install needed).
- A standard **Rust toolchain** (2021 edition) and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) to compile the WebAssembly UI.

---

## 🛠️ How to Build

We provide a `justfile` for convenient compilation. Run the standard recipe or build manually inside the project directory:

```sh
# Option A: Compile using just
just wasm

# Option B: Compile manually
cd ui && wasm-pack build --target web --release --out-dir pkg
```

This compiles the Rust data explorer and places the WebAssembly binary bundles under `ui/pkg/`.

---

## 📦 Installation

Clone with submodules:

```sh
git clone --recurse-submodules https://github.com/nohzafk/emacs-parquet-explorer.git
```

Add one `load-path` entry -- the bundled emacs-egui submodule is discovered automatically:

```elisp
(add-to-list 'load-path "/path/to/emacs-parquet-explorer/lisp")
(require 'emacs-parquet-explorer)
```

### Recommended `use-package` Configuration

```elisp
(use-package emacs-parquet-explorer
  :load-path "/path/to/emacs-parquet-explorer/lisp"
  :commands emacs-parquet-explorer-open
  :bind ("C-c d p" . emacs-parquet-explorer-open))
```

To browse a Parquet file, run:

```text
M-x emacs-parquet-explorer-open
```

And select any local `.parquet` file on your filesystem.

---

## 📊 Verification & Performance Benchmarking

To verify that the build environment and performance optimizations are working seamlessly, you can test with both small and massive datasets:

### 1. Small Sample Verification (5 Rows)

Generate a basic 5-row test file using our built-in CLI generator:

```sh
cd ui && cargo run --bin generate_sample
```

This generates `test.parquet` in the project root. Open it in Emacs using `M-x emacs-parquet-explorer-open` to verify metadata inspection, schema mapping, and event handling.

### 2. High-Volume Performance Benchmark (3+ Million Rows)

To test the speed of virtual rendering, schema pruning, and predicate pushdowns under heavy load, download a real-world Yellow Taxi dataset (~47MB Parquet / over 3,000,000 rows):

```sh
curl -L -o yellow_tripdata_2023-01.parquet \
  "https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2023-01.parquet"
```

Open `yellow_tripdata_2023-01.parquet` using `M-x emacs-parquet-explorer-open`.

- **Observe:** Over 3 million rows will load instantly.
- **Scroll:** Scroll vertically or horizontally with zero latency.
- **Prune:** Toggle columns (e.g. hiding `VendorID` or `tpep_pickup_datetime`) to see real-time table layout adjustments.
- **Export:** Export the entire dataset to a CSV file asynchronously in the background.

---

## 🗺️ Feature Development Roadmap

Our roadmap highlights implemented features and upcoming work:

| Priority | Feature | Status | Description |
| :---: | :--- | :---: | :--- |
| **1** | **Schema & Metadata Inspection** | **[x] Completed** | Side-by-side dashboard showing physical stats (compression, row groups, metadata) and schema type discovery. |
| **2** | **Performance Polish (Virtual Scrolling)** | **[x] Completed** | Renders up to 100,000+ rows instantly using egui virtual scrolling to prevent memory spikes. |
| **3** | **Column Pruning Toggle** | **[x] Completed** | Collapsing checklist panel to dynamically show or hide columns in the grid. |
| **4** | **Format Interoperability (CSV Export)** | **[x] Completed** | Memory-efficient asynchronous loopback background streaming export of datasets directly to CSV. |
| **5** | **Predicate Pushdown & Cell Filtering** | **[x] Completed** | Cell quick-filtering and column-specific predicate pushdowns to isolate anomalies instantly. |
| **6** | **In-App SQL Execution** | **[ ] Planned** | An ad-hoc SQL query bar (`WHERE`, `GROUP BY`, `ORDER BY`) running locally on the loaded dataset using DataFusion. |

---

## 📄 License

This application is licensed under the MIT License. Feel free to copy, modify, and distribute it.
