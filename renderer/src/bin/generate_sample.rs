use parquet::{
    file::{
        properties::WriterProperties,
        writer::SerializedFileWriter,
    },
    schema::parser::parse_message_type,
};
use std::{fs::File, sync::Arc};

fn main() {
    let path = std::path::Path::new("/Users/randall/projects/emacs-parquet-explorer/test.parquet");
    let file = File::create(&path).unwrap();

    let schema = "
        message schema {
            REQUIRED INT32 id;
            REQUIRED byte_array name (utf8);
            REQUIRED DOUBLE score;
        }
    ";
    let schema = Arc::new(parse_message_type(schema).unwrap());
    let props = Arc::new(WriterProperties::builder().build());
    let mut writer = SerializedFileWriter::new(file, schema, props).unwrap();

    let mut row_group_writer = writer.next_row_group().unwrap();

    // Column 0: id
    let mut col_writer = row_group_writer.next_column().unwrap().unwrap();
    col_writer
        .typed::<parquet::data_type::Int32Type>()
        .write_batch(&[1, 2, 3, 4, 5], None, None)
        .unwrap();
    col_writer.close().unwrap();

    // Column 1: name
    let mut col_writer = row_group_writer.next_column().unwrap().unwrap();
    let names = vec![
        parquet::data_type::ByteArray::from("Alice"),
        parquet::data_type::ByteArray::from("Bob"),
        parquet::data_type::ByteArray::from("Charlie"),
        parquet::data_type::ByteArray::from("Dave"),
        parquet::data_type::ByteArray::from("Eve"),
    ];
    col_writer
        .typed::<parquet::data_type::ByteArrayType>()
        .write_batch(&names, None, None)
        .unwrap();
    col_writer.close().unwrap();

    // Column 2: score
    let mut col_writer = row_group_writer.next_column().unwrap().unwrap();
    col_writer
        .typed::<parquet::data_type::DoubleType>()
        .write_batch(&[98.5, 92.0, 88.7, 95.2, 90.1], None, None)
        .unwrap();
    col_writer.close().unwrap();

    row_group_writer.close().unwrap();
    writer.close().unwrap();
    println!("Generated sample parquet file successfully at: {:?}", path);
}
