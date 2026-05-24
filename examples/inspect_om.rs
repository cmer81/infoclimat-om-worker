// Quick & dirty OMfile inspector — walks the root variable, lists children,
// prints scalar values and array dimensions. Used to discover whether spatial
// OMfiles carry geographic metadata (lat0, lon0, dx, dy) inline.
//
// Run with: cargo run --example inspect_om -- /tmp/test.om

use std::sync::Arc;

use omfiles::{
    InMemoryBackend,
    reader::OmFileReader,
    traits::{OmArrayVariable, OmFileReadable, OmFileVariable, OmScalarVariable},
};

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: inspect_om <path-to-omfile>");
    let bytes = std::fs::read(&path)?;
    println!("loaded {} bytes from {}", bytes.len(), path);

    let backend = Arc::new(InMemoryBackend::new(bytes));
    let root = OmFileReader::new(backend)?;

    println!("\n=== ROOT ===");
    dump(&root, 0);

    Ok(())
}

fn dump<B: omfiles::traits::OmFileReaderBackend>(
    var: &OmFileReader<B>,
    depth: usize,
) {
    let indent = "  ".repeat(depth);
    let name = var.name();
    let n = var.number_of_children();
    println!("{indent}- name={name:?} children={n}");

    if let Ok(arr) = var.expect_array() {
        let dims = arr.get_dimensions();
        let chunks = arr.get_chunk_dimensions();
        let scale = arr.scale_factor();
        let offset = arr.add_offset();
        println!(
            "{indent}  ARRAY dims={dims:?} chunks={chunks:?} scale={scale} offset={offset}"
        );
    } else if let Ok(sc) = var.expect_scalar() {
        if let Some(v) = sc.read_scalar::<f32>() {
            println!("{indent}  SCALAR f32 = {v}");
        } else if let Some(v) = sc.read_scalar::<f64>() {
            println!("{indent}  SCALAR f64 = {v}");
        } else if let Some(v) = sc.read_scalar::<i64>() {
            println!("{indent}  SCALAR i64 = {v}");
        } else if let Some(v) = sc.read_scalar::<i32>() {
            println!("{indent}  SCALAR i32 = {v}");
        } else if let Some(v) = sc.read_scalar::<u32>() {
            println!("{indent}  SCALAR u32 = {v}");
        } else if let Some(v) = sc.read_scalar::<String>() {
            println!("{indent}  SCALAR str = {v:?}");
        } else {
            println!("{indent}  (scalar of unknown type)");
        }
    } else {
        println!("{indent}  (group / no array / no scalar)");
    }

    for i in 0..n {
        if let Some(child) = var.get_child_by_index(i) {
            dump(&child, depth + 1);
        }
    }
}
