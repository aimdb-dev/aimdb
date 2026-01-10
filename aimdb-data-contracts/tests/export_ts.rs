//! TypeScript export test
//!
//! Run with: cargo test --features ts,observable export_typescript -- --ignored
//!
//! This generates TypeScript type definitions to the bindings/ directory,
//! plus a schema-registry.ts with full metadata (fields, units, icons).

#![cfg(feature = "ts")]

use aimdb_data_contracts::contracts::{GpsLocation, Humidity, Temperature};
use aimdb_data_contracts::{Observable, SchemaType};
use std::fs;
use std::path::Path;
use ts_rs::TS;

/// Schema metadata for export to TypeScript - derived automatically from traits
struct SchemaMeta {
    name: &'static str,
    icon: &'static str,
    unit: &'static str,
    rust_schema: String,
}

impl SchemaMeta {
    /// Create metadata from any type implementing Observable + TS
    fn from_type<T: SchemaType + Observable + TS>(source_file: &str, struct_name: &str) -> Self {
        let contracts_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contracts");
        let rust_schema = extract_struct_definition(&contracts_dir.join(source_file), struct_name)
            .unwrap_or_else(|| format!("// Could not extract {} from {}", struct_name, source_file));

        SchemaMeta {
            name: T::NAME,
            icon: T::ICON,
            unit: T::UNIT,
            rust_schema,
        }
    }

    /// Derive label from name (temperature -> Temperature, gps_location -> GPS Location)
    fn label(&self) -> String {
        self.name
            .split('_')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => {
                        // Handle known acronyms
                        if word == "gps" {
                            "GPS".to_string()
                        } else {
                            c.to_uppercase().chain(chars).collect()
                        }
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Extract a struct definition from a Rust source file
fn extract_struct_definition(file_path: &Path, struct_name: &str) -> Option<String> {
    let content = fs::read_to_string(file_path).ok()?;

    // Find the struct definition with doc comments
    let struct_marker = format!("pub struct {}", struct_name);
    let struct_start = content.find(&struct_marker)?;

    // Walk backwards to find doc comments (/// lines)
    let before_struct = &content[..struct_start];
    let doc_start = before_struct
        .rfind("\n\n")
        .or_else(|| before_struct.rfind("*/\n"))
        .map(|i| i + 1)
        .unwrap_or(struct_start);

    // Find the closing brace of the struct
    let after_struct = &content[struct_start..];
    let struct_end = after_struct.find("\n}\n").or_else(|| after_struct.find("\n}"))?;

    let full_definition = &content[doc_start..struct_start + struct_end + 2];

    // Clean up: remove #[derive(...)] and #[cfg_attr(...)] lines, keep doc comments and struct
    let cleaned: String = full_definition
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with("#[derive")
                && !trimmed.starts_with("#[cfg_attr")
                && !trimmed.starts_with("#[serde")
        })
        .collect::<Vec<_>>()
        .join("\n");

    Some(cleaned.trim().to_string())
}

/// Export all contract types to TypeScript.
///
/// By default, ts-rs exports to `./bindings/` relative to the crate root.
/// Run this test with --ignored flag to generate the files:
///
/// ```sh
/// cargo test -p aimdb-data-contracts --features ts,observable export_typescript -- --ignored
/// ```
#[test]
#[ignore = "Run manually to generate TypeScript bindings"]
fn export_typescript() {
    // Export each contract type via ts-rs
    Temperature::export_all().expect("Failed to export Temperature");
    Humidity::export_all().expect("Failed to export Humidity");
    GpsLocation::export_all().expect("Failed to export GpsLocation");

    println!("✅ TypeScript bindings exported to bindings/");

    // Generate schema registry with metadata - all derived from traits + source
    let schemas = vec![
        SchemaMeta::from_type::<Temperature>("temperature.rs", "Temperature"),
        SchemaMeta::from_type::<Humidity>("humidity.rs", "Humidity"),
        SchemaMeta::from_type::<GpsLocation>("location.rs", "GpsLocation"),
    ];

    generate_schema_registry(&schemas);
}

fn generate_schema_registry(schemas: &[SchemaMeta]) {
    let mut output = String::from(
        r#"// AUTO-GENERATED from aimdb-data-contracts - DO NOT EDIT
// Run: cargo test -p aimdb-data-contracts --features ts,observable export_typescript -- --ignored

export interface SchemaMeta {
  name: string;
  label: string;
  icon: string;
  unit: string;
  rustSchema: string;
}

/**
 * Schema registry with metadata extracted from Rust Observable traits.
 * Keys match WebSocket message `type` field.
 */
export const SCHEMA_REGISTRY: Record<string, SchemaMeta> = {
"#,
    );

    for schema in schemas {
        output.push_str(&format!(
            r#"  "{}": {{
    name: "{}",
    label: "{}",
    icon: "{}",
    unit: "{}",
    rustSchema: `{}`,
  }},
"#,
            schema.name,
            schema.name,
            schema.label(),
            schema.icon,
            schema.unit,
            schema.rust_schema
        ));
    }

    output.push_str(
        r#"};

/**
 * Get schema metadata by WebSocket message type.
 */
export function getSchema(messageType: string): SchemaMeta | undefined {
  return SCHEMA_REGISTRY[messageType];
}

/**
 * List all available schema types.
 */
export const SCHEMA_TYPES = Object.keys(SCHEMA_REGISTRY);
"#,
    );

    let bindings_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("bindings");
    fs::create_dir_all(&bindings_dir).expect("Failed to create bindings dir");

    let output_path = bindings_dir.join("schema-registry.ts");
    fs::write(&output_path, output).expect("Failed to write schema registry");

    println!("✅ Schema registry exported to {:?}", output_path);
}

/// Verify that all types can be exported (doesn't write files)
#[test]
fn verify_ts_definitions() {
    // Just verify the definitions are valid
    let temp_ts = Temperature::decl();
    let humidity_ts = Humidity::decl();
    let location_ts = GpsLocation::decl();

    assert!(temp_ts.contains("Temperature"));
    assert!(humidity_ts.contains("Humidity"));
    assert!(location_ts.contains("GpsLocation"));

    println!("Temperature:\n{}\n", temp_ts);
    println!("Humidity:\n{}\n", humidity_ts);
    println!("GpsLocation:\n{}\n", location_ts);
}
