# emacs-parquet-explorer

An interactive, GPU-accelerated visual data browser and query tool for large Parquet files, built inside Emacs using Rust and **egui** WebAssembly. 

Layered on top of the generic [emacs-egui](file:///Users/randall/projects/emacs-egui) host framework, this package enables database-client-grade performance and fluid virtual scrolling directly within a standard Emacs buffer.

![Parquet Explorer Demo](file:///Users/randall/projects/emacs-parquet-explorer/renderer/pkg/emacs_parquet_explorer_renderer_bg.wasm)

---

## Features

1.  **High-Volume In-Memory Parsing:** Downloads the local binary Parquet streams over the framework gateway asynchronously and decodes column schemas and row groups in memory using pure Rust Arrow/Parquet APIs.
2.  **Adaptive Responsive Grid:** Automatically scales to 100% of the active Emacs window height using nested horizontal and vertical `ScrollArea` containers.
3.  **Sticky Column Headers:** Pinned column headers stay locked at the top of the buffer during vertical scrolling, while scrolling together in lockstep horizontally across wide schemas (tested up to 19+ columns).
4.  **Configurable Paging Presets:** Paginates results dynamically with quick presets (`100`, `500`, `1000`) and a custom `DragValue` input (double-click to type any custom row limit).
5.  **Global Text Search:** Fast, case-insensitive global text filtering matching substrings across all cells in every column.
6.  **Interactive Clipboard Integration:** Selecting any cell displays its full detailed content in a resizable bottom panel and copies the cell value instantly to the Emacs `kill-ring` clipboard.

---

## Requirements

- Emacs 29.1+ built **with xwidget support** (`(featurep 'xwidget-internal)`).
- A standard Rust toolchain and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) to compile the WebAssembly renderer.
- The [emacs-egui](file:///Users/randall/projects/emacs-egui) framework package installed on your load path.

---

## How to Build the Renderer

Run the standard `just` recipe or compile manually inside the project directory:

```sh
# Using just:
just wasm

# Or manually:
cd renderer && wasm-pack build --target web --release
```

This compiles the Rust data explorer and places the WebAssembly binary bundles under `renderer/pkg/`.

---

## Running the Explorer in Emacs

Add both the host framework and this explorer package to your Emacs configuration:

```elisp
(add-to-list 'load-path "/Users/randall/projects/emacs-egui/lisp")
(add-to-list 'load-path "/Users/randall/projects/emacs-parquet-explorer/lisp")

(require 'emacs-parquet-explorer)
```

To open a Parquet file:
```text
M-x emacs-parquet-explorer-open
```
And select any local `.parquet` file on your filesystem. 

---

## Feature Development Roadmap

To provide complete visibility and track expansion goals, here is our sequenced step-by-step roadmap:

| Priority | Feature | Status | Description |
| :--- | :--- | :--- | :--- |
| **1** | **Schema & Metadata Inspection** | **[x] Completed** | Side-by-side dashboard showing physical stats (compression, row groups) and schema type discovery. |
| **2** | **Performance Polish (Virtual Scrolling)** | **[x] Completed** | Renders up to 100,000+ rows instantly using egui virtual scrolling to prevent memory spikes. |
| **3** | **Column Pruning Toggle** | **[x] Completed** | Collapsing checklist panel to dynamically show or hide columns in the grid. |
| **4** | **Format Interoperability (CSV Export)** | **[x] Completed** | Memory-efficient asynchronous loopback background streaming export of datasets directly to CSV. |
| **5** | **Predicate Pushdown & Cell Filtering** | **[x] Completed** | Cell quick-filtering and column-specific predicate pushdowns to isolate anomalies instantly. |
| **6** | **In-App SQL Execution** | **[ ] Planned** | An ad-hoc SQL query bar (`WHERE`, `GROUP BY`, `ORDER BY`) running locally on the loaded dataset. |

---

## Testing with Sample Data

We provide a pre-compiled native generator inside the project. To generate a sample 5-row Parquet file (`test.parquet`):

```sh
cd renderer && cargo run --bin generate_sample
```

Alternatively, you can test high-volume scrolling with a real-world dataset (over 3 million rows) by downloading a monthly Yellow Taxi trip record:

```sh
curl -L -o /Users/randall/projects/emacs-parquet-explorer/yellow_tripdata_2023-01.parquet \
  "https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2023-01.parquet"
```

---

## License

This project is licensed under the MIT License.
