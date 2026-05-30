use parquet::file::reader::{FileReader, SerializedFileReader};
use std::{fs::File, io::Write};

/// Escape CSV fields strictly complying with RFC-4180:
/// - If a field contains commas, double quotes, or newlines, wrap it in double quotes.
/// - Any double quote characters inside the field are doubled (escaped as "").
fn escape_csv_field(val: &str) -> String {
    if val.contains(',') || val.contains('"') || val.contains('\n') || val.contains('\r') {
        let escaped = val.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        val.to_string()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: parquet_to_csv <input_file.parquet> <output_file.csv>");
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    println!("Opening Parquet source: {}", input_path);
    let file = File::open(input_path)?;
    let reader = SerializedFileReader::new(file)?;

    // 1. Extract physical column schema names
    let file_metadata = reader.metadata().file_metadata();
    let schema_descr = file_metadata.schema_descr();
    let columns: Vec<String> = (0..schema_descr.num_columns())
        .map(|i| schema_descr.column(i).name().to_string())
        .collect();

    println!("Found {} columns. Writing target CSV: {}", columns.len(), output_path);
    let mut out = File::create(output_path)?;

    // 2. Write CSV Header line
    let header_line = columns.iter()
        .map(|col| escape_csv_field(col))
        .collect::<Vec<String>>()
        .join(",");
    writeln!(out, "{}", header_line)?;

    // 3. Stream Parquet records dynamically to CSV lines (O(1) memory complexity)
    let row_iter = reader.get_row_iter(None)?;
    let mut count = 0;
    for record in row_iter {
        let record = record?;
        let mut row_fields = Vec::new();
        for (_, field) in record.get_column_iter() {
            row_fields.push(escape_csv_field(&format!("{}", field)));
        }
        writeln!(out, "{}", row_fields.join(","))?;
        count += 1;
        if count % 10000 == 0 {
            println!("Exported {} rows...", count);
        }
    }

    println!("Export complete! Total rows successfully written: {}", count);
    Ok(())
}
