use emacs_parquet_explorer_ui::{parse_parquet, scan_parallel, ColumnFilter};
use std::time::Instant;

/// Native multi-threaded filter sidecar: scans a Parquet file for rows matching
/// a global text query and/or column filters, parallelized across all cores,
/// and prints the matching row indices as a JSON array on stdout.
///
/// Usage: parquet_filter <input.parquet> [query] [filters-json] [out-file]
///   query        global case-insensitive substring (empty = no text query)
///   filters-json JSON array of {"column","operator","value"} objects
///   out-file     write the index JSON here instead of stdout (used by the
///                Emacs broker so the WASM UI can fetch it via the asset server)
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: parquet_filter <input.parquet> [query] [filters-json] [out-file]");
        std::process::exit(1);
    }

    let path = &args[1];
    let query = args.get(2).map(String::as_str).unwrap_or("");
    let filters: Vec<ColumnFilter> = match args.get(3) {
        Some(j) if !j.is_empty() => serde_json::from_str(j)?,
        _ => Vec::new(),
    };
    let out_file = args.get(4).filter(|s| !s.is_empty());

    let bytes = std::fs::read(path)?;
    let table = match parse_parquet(bytes) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    };

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let t = Instant::now();
    let indices = match scan_parallel(&table, query, &filters, threads) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("scan error: {e}");
            std::process::exit(1);
        }
    };
    let ms = t.elapsed().as_millis();

    eprintln!(
        "matched {} / {} rows in {} ms using {} threads",
        indices.len(),
        table.stats.total_rows,
        ms,
        threads
    );

    // Result payload for the Emacs broker: a JSON array of matching row indices.
    let payload = serde_json::to_string(&indices)?;
    match out_file {
        Some(path) => std::fs::write(path, payload)?,
        None => println!("{}", payload),
    }
    Ok(())
}
