use emacs_parquet_explorer_ui::{parse_parquet, scan_parallel, ColumnFilter, ParquetTable};
use std::io::{BufRead, Write};
use std::time::Instant;

/// One request line in `--serve` mode (newline-delimited JSON on stdin).
#[derive(serde::Deserialize)]
struct FilterRequest {
    token: u64,
    #[serde(default)]
    query: String,
    /// JSON-encoded array of column filters (empty string => none).
    #[serde(default)]
    filters: String,
    /// File the matching indices are written to (fetched by the WASM UI).
    out: String,
}

/// Native multi-threaded filter sidecar: scans a Parquet file for rows matching
/// a global text query and/or column filters, parallelized across all cores.
///
/// One-shot:  parquet_filter <input.parquet> [query] [filters-json] [out-file]
///   query        global case-insensitive substring (empty = no text query)
///   filters-json JSON array of {"column","operator","value"} objects
///   out-file     write the index JSON here instead of stdout
///
/// Persistent: parquet_filter --serve <input.parquet>
///   Parses the file once, then reads newline-delimited JSON requests
///   {"token","query","filters","out"} on stdin and, for each, writes the
///   matching indices to `out` and prints a response line
///   {"token","path","count"} (or {"token","error"}) on stdout.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("--serve") {
        let path = args
            .get(2)
            .ok_or("usage: parquet_filter --serve <input.parquet>")?;
        return serve(path);
    }

    if args.len() < 2 {
        eprintln!("Usage: parquet_filter <input.parquet> [query] [filters-json] [out-file]");
        eprintln!("       parquet_filter --serve <input.parquet>");
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

fn parse_filters(json: &str) -> Vec<ColumnFilter> {
    if json.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(json).unwrap_or_default()
    }
}

/// Persistent mode: parse the file once, then serve newline-delimited requests.
fn serve(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let table: ParquetTable = match parse_parquet(bytes) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    };
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    eprintln!(
        "parquet_filter --serve ready: {} rows, {} cols, {} threads",
        table.stats.total_rows,
        table.columns.len(),
        threads
    );

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: FilterRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("bad request: {e}");
                continue;
            }
        };

        let filters = parse_filters(&req.filters);
        let t = Instant::now();
        let resp = match scan_parallel(&table, &req.query, &filters, threads) {
            Ok(indices) => match std::fs::write(&req.out, serde_json::to_string(&indices)?) {
                Ok(()) => {
                    eprintln!(
                        "token {} matched {} rows in {} ms",
                        req.token,
                        indices.len(),
                        t.elapsed().as_millis()
                    );
                    serde_json::json!({ "token": req.token, "path": req.out, "count": indices.len() })
                }
                Err(e) => serde_json::json!({ "token": req.token, "error": format!("write failed: {e}") }),
            },
            Err(e) => serde_json::json!({ "token": req.token, "error": e }),
        };

        writeln!(stdout, "{}", resp)?;
        stdout.flush()?;
    }
    Ok(())
}
