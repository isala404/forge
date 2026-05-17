//! Schema JSON emitter.
//!
//! Serializes `SchemaRegistry` to `forge.schema.json`, the public contract
//! between Phase 1 (parser) and Phase 2 (per-language emitters). Community
//! generators consume this file without depending on Forge's Rust crates.

use forge_core::schema::{EnumDef, FieldDef, FunctionDef, RustType, SchemaRegistry, TableDef};
use serde_json::{Value, json};

/// Wire protocol version. Bumped on breaking schema format changes.
const WIRE_VERSION: &str = "2";

/// Forge framework version stamped into the schema.
const FORGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Emit a `forge.schema.json` document from a schema registry.
pub fn emit(registry: &SchemaRegistry) -> Value {
    json!({
        "$schema": "https://forge-rs.dev/schema/v2.json",
        "version": FORGE_VERSION,
        "wire_version": WIRE_VERSION,
        "types": emit_types(registry),
        "functions": emit_functions(registry),
    })
}

/// Serialize the schema to a pretty-printed JSON string with trailing newline.
pub fn emit_string(registry: &SchemaRegistry) -> Result<String, serde_json::Error> {
    let value = emit(registry);
    let mut output = serde_json::to_string_pretty(&value)?;
    output.push('\n');
    Ok(output)
}

fn emit_types(registry: &SchemaRegistry) -> Value {
    let mut types = serde_json::Map::new();

    let mut tables = registry.all_tables();
    tables.sort_by(|a, b| a.struct_name.cmp(&b.struct_name));
    for table in tables {
        types.insert(table.struct_name.clone(), emit_table(&table));
    }

    let mut enums = registry.all_enums();
    enums.sort_by(|a, b| a.name.cmp(&b.name));
    for enum_def in enums {
        types.insert(enum_def.name.clone(), emit_enum(&enum_def));
    }

    Value::Object(types)
}

fn emit_table(table: &TableDef) -> Value {
    let fields: Vec<Value> = table.fields.iter().map(emit_field).collect();

    let mut map = serde_json::Map::new();
    map.insert(
        "kind".into(),
        json!(if table.is_dto { "dto" } else { "model" }),
    );
    map.insert("fields".into(), json!(fields));
    if let Some(doc) = &table.doc {
        map.insert("doc".into(), json!(doc));
    }

    Value::Object(map)
}

fn emit_field(field: &FieldDef) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("name".into(), json!(field.name));
    map.insert("type".into(), emit_rust_type(&field.rust_type));
    map.insert("nullable".into(), json!(field.nullable));
    if let Some(doc) = &field.doc {
        map.insert("doc".into(), json!(doc));
    }

    Value::Object(map)
}

fn emit_enum(enum_def: &EnumDef) -> Value {
    let variants: Vec<Value> = enum_def
        .variants
        .iter()
        .map(|v| {
            let mut map = serde_json::Map::new();
            map.insert("name".into(), json!(v.name));
            map.insert("value".into(), json!(v.sql_value));
            if let Some(int_val) = v.int_value {
                map.insert("int_value".into(), json!(int_val));
            }
            if let Some(doc) = &v.doc {
                map.insert("doc".into(), json!(doc));
            }
            Value::Object(map)
        })
        .collect();

    let mut map = serde_json::Map::new();
    map.insert("kind".into(), json!("enum"));
    map.insert("variants".into(), json!(variants));
    if let Some(doc) = &enum_def.doc {
        map.insert("doc".into(), json!(doc));
    }

    Value::Object(map)
}

fn emit_functions(registry: &SchemaRegistry) -> Value {
    let mut functions = serde_json::Map::new();

    let mut all_fns = registry.all_functions();
    all_fns.sort_by(|a, b| a.name.cmp(&b.name));

    for func in all_fns {
        functions.insert(func.name.clone(), emit_function(&func));
    }

    Value::Object(functions)
}

fn emit_function(func: &FunctionDef) -> Value {
    let args: Vec<Value> = func
        .args
        .iter()
        .map(|arg| {
            let mut map = serde_json::Map::new();
            map.insert("name".into(), json!(arg.name));
            map.insert("type".into(), emit_rust_type(&arg.rust_type));
            if let Some(doc) = &arg.doc {
                map.insert("doc".into(), json!(doc));
            }
            Value::Object(map)
        })
        .collect();

    let mut map = serde_json::Map::new();
    map.insert("kind".into(), json!(func.kind.as_str()));
    map.insert("args".into(), json!(args));
    map.insert("returns".into(), emit_rust_type(&func.return_type));
    if let Some(doc) = &func.doc {
        map.insert("doc".into(), json!(doc));
    }

    Value::Object(map)
}

fn emit_rust_type(rust_type: &RustType) -> Value {
    match rust_type {
        RustType::String => json!("string"),
        RustType::I32 => json!({"base": "number", "format": "i32"}),
        RustType::I64 => json!({"base": "number", "format": "i64"}),
        RustType::F32 => json!({"base": "number", "format": "f32"}),
        RustType::F64 => json!({"base": "number", "format": "f64"}),
        RustType::Bool => json!("boolean"),
        RustType::Uuid => json!({"base": "string", "format": "uuid"}),
        RustType::Instant => json!({"base": "string", "format": "datetime"}),
        RustType::LocalDate => json!({"base": "string", "format": "date"}),
        RustType::LocalTime => json!({"base": "string", "format": "time"}),
        RustType::Upload => json!("upload"),
        RustType::Json => json!("any"),
        RustType::Bytes => json!("bytes"),
        RustType::Option(inner) => json!({"nullable": emit_rust_type(inner)}),
        RustType::Vec(inner) => json!({"array": emit_rust_type(inner)}),
        RustType::Custom(name) => json!({"$ref": name}),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use forge_core::schema::{
        EnumDef, EnumVariant, FieldDef, FunctionArg, FunctionDef, SchemaRegistry, TableDef,
    };

    #[test]
    fn empty_registry_produces_valid_schema() {
        let registry = SchemaRegistry::new();
        let schema = emit(&registry);

        assert_eq!(schema["wire_version"], "2");
        assert!(schema["version"].is_string());
        assert!(schema["types"].is_object());
        assert!(schema["functions"].is_object());
    }

    #[test]
    fn models_and_enums_appear_in_types() {
        let registry = SchemaRegistry::new();

        let mut table = TableDef::new("users", "User");
        table.fields.push(FieldDef::new("id", RustType::Uuid));
        table.fields.push(FieldDef::new("email", RustType::String));
        registry.register_table(table);

        let mut enum_def = EnumDef::new("Role");
        enum_def.variants.push(EnumVariant::new("Admin"));
        enum_def.variants.push(EnumVariant::new("Member"));
        registry.register_enum(enum_def);

        let schema = emit(&registry);

        assert_eq!(schema["types"]["User"]["kind"], "model");
        assert_eq!(schema["types"]["User"]["fields"][0]["name"], "id");
        assert_eq!(schema["types"]["Role"]["kind"], "enum");
        assert_eq!(schema["types"]["Role"]["variants"][0]["name"], "Admin");
    }

    #[test]
    fn functions_appear_with_kind_and_args() {
        let registry = SchemaRegistry::new();

        let mut func = FunctionDef::query("get_user", RustType::Custom("User".into()));
        func.args.push(FunctionArg::new("id", RustType::Uuid));
        func.doc = Some("Fetch a user by ID".into());
        registry.register_function(func);

        let schema = emit(&registry);

        let get_user = &schema["functions"]["get_user"];
        assert_eq!(get_user["kind"], "query");
        assert_eq!(get_user["args"][0]["name"], "id");
        assert_eq!(get_user["doc"], "Fetch a user by ID");
    }

    #[test]
    fn type_mapping_round_trips() {
        assert_eq!(emit_rust_type(&RustType::String), json!("string"));
        assert_eq!(emit_rust_type(&RustType::Bool), json!("boolean"));
        assert_eq!(
            emit_rust_type(&RustType::I32),
            json!({"base": "number", "format": "i32"})
        );
        assert_eq!(
            emit_rust_type(&RustType::Uuid),
            json!({"base": "string", "format": "uuid"})
        );
        assert_eq!(
            emit_rust_type(&RustType::Option(Box::new(RustType::String))),
            json!({"nullable": "string"})
        );
        assert_eq!(
            emit_rust_type(&RustType::Vec(Box::new(RustType::I32))),
            json!({"array": {"base": "number", "format": "i32"}})
        );
        assert_eq!(
            emit_rust_type(&RustType::Custom("User".into())),
            json!({"$ref": "User"})
        );
    }

    #[test]
    fn dto_marked_correctly() {
        let registry = SchemaRegistry::new();

        let mut dto = TableDef::new("CreateUserArgs", "CreateUserArgs");
        dto.is_dto = true;
        dto.fields.push(FieldDef::new("name", RustType::String));
        registry.register_table(dto);

        let schema = emit(&registry);
        assert_eq!(schema["types"]["CreateUserArgs"]["kind"], "dto");
    }

    #[test]
    fn emit_string_has_trailing_newline() {
        let registry = SchemaRegistry::new();
        let output = emit_string(&registry).unwrap();
        assert!(output.ends_with('\n'));
        assert!(output.contains("wire_version"));
    }

    #[test]
    fn deterministic_output() {
        let registry = SchemaRegistry::new();

        registry.register_function(FunctionDef::query("z_last", RustType::String));
        registry.register_function(FunctionDef::query("a_first", RustType::String));
        registry.register_function(FunctionDef::mutation("m_middle", RustType::I32));

        let mut table_z = TableDef::new("zebras", "Zebra");
        table_z.fields.push(FieldDef::new("id", RustType::Uuid));
        registry.register_table(table_z);

        let mut table_a = TableDef::new("apples", "Apple");
        table_a.fields.push(FieldDef::new("id", RustType::Uuid));
        registry.register_table(table_a);

        let first = emit_string(&registry).unwrap();
        let second = emit_string(&registry).unwrap();
        assert_eq!(first, second, "Schema output must be deterministic");
    }

    #[test]
    fn emit_rust_type_covers_every_numeric_and_temporal_variant() {
        // Each branch in emit_rust_type is part of the public wire contract;
        // a missing variant would emit `null` silently and break consumers.
        assert_eq!(
            emit_rust_type(&RustType::I64),
            json!({"base": "number", "format": "i64"})
        );
        assert_eq!(
            emit_rust_type(&RustType::F32),
            json!({"base": "number", "format": "f32"})
        );
        assert_eq!(
            emit_rust_type(&RustType::F64),
            json!({"base": "number", "format": "f64"})
        );
        assert_eq!(
            emit_rust_type(&RustType::Instant),
            json!({"base": "string", "format": "datetime"})
        );
        assert_eq!(
            emit_rust_type(&RustType::LocalDate),
            json!({"base": "string", "format": "date"})
        );
        assert_eq!(
            emit_rust_type(&RustType::LocalTime),
            json!({"base": "string", "format": "time"})
        );
        assert_eq!(emit_rust_type(&RustType::Upload), json!("upload"));
        assert_eq!(emit_rust_type(&RustType::Json), json!("any"));
        assert_eq!(emit_rust_type(&RustType::Bytes), json!("bytes"));
    }

    #[test]
    fn emit_rust_type_nests_option_and_vec_recursively() {
        // Option<Vec<Custom>> exercises three combined transforms.
        let ty = RustType::Option(Box::new(RustType::Vec(Box::new(RustType::Custom(
            "Role".into(),
        )))));
        assert_eq!(
            emit_rust_type(&ty),
            json!({"nullable": {"array": {"$ref": "Role"}}})
        );
    }

    #[test]
    fn field_emits_nullable_flag_for_option_types() {
        // FieldDef::new sets `nullable: true` whenever the rust_type is Option.
        let field = FieldDef::new("nickname", RustType::Option(Box::new(RustType::String)));
        let json = emit_field(&field);
        assert_eq!(json["name"], "nickname");
        assert_eq!(json["nullable"], true);
        assert_eq!(json["type"], json!({"nullable": "string"}));
    }

    #[test]
    fn field_emits_doc_only_when_present() {
        let mut documented = FieldDef::new("email", RustType::String);
        documented.doc = Some("Primary contact address".into());
        let with_doc = emit_field(&documented);
        assert_eq!(with_doc["doc"], "Primary contact address");

        let plain = emit_field(&FieldDef::new("id", RustType::Uuid));
        assert!(
            plain.as_object().unwrap().get("doc").is_none(),
            "absent doc must not emit the key"
        );
    }

    #[test]
    fn enum_emits_int_value_and_variant_doc_when_set() {
        let mut enum_def = EnumDef::new("Status");
        let mut active = EnumVariant::new("Active");
        active.int_value = Some(1);
        active.doc = Some("Currently in use".into());
        enum_def.variants.push(active);
        enum_def.variants.push(EnumVariant::new("Archived"));

        let json = emit_enum(&enum_def);
        assert_eq!(json["variants"][0]["int_value"], 1);
        assert_eq!(json["variants"][0]["doc"], "Currently in use");

        // The bare variant must NOT emit int_value or doc keys.
        let second = json["variants"][1].as_object().unwrap();
        assert!(second.get("int_value").is_none());
        assert!(second.get("doc").is_none());
    }

    #[test]
    fn enum_emits_top_level_doc_when_set() {
        let mut enum_def = EnumDef::new("Role");
        enum_def.doc = Some("User permission tier".into());
        enum_def.variants.push(EnumVariant::new("Admin"));

        let json = emit_enum(&enum_def);
        assert_eq!(json["doc"], "User permission tier");
    }

    #[test]
    fn table_emits_top_level_doc_when_set() {
        let mut table = TableDef::new("users", "User");
        table.doc = Some("Account records".into());
        table.fields.push(FieldDef::new("id", RustType::Uuid));

        let json = emit_table(&table);
        assert_eq!(json["doc"], "Account records");
        assert_eq!(json["kind"], "model");
    }

    #[test]
    fn function_with_no_args_emits_empty_array() {
        let func = FunctionDef::query("ping", RustType::Bool);
        let json = emit_function(&func);
        assert_eq!(json["kind"], "query");
        assert_eq!(json["args"], json!([]));
        assert_eq!(json["returns"], json!("boolean"));
    }

    #[test]
    fn types_and_functions_sort_alphabetically_in_string_output() {
        let registry = SchemaRegistry::new();
        registry.register_function(FunctionDef::query("z_last", RustType::String));
        registry.register_function(FunctionDef::query("a_first", RustType::String));

        let mut zebra = TableDef::new("zebras", "Zebra");
        zebra.fields.push(FieldDef::new("id", RustType::Uuid));
        registry.register_table(zebra);

        let mut apple = TableDef::new("apples", "Apple");
        apple.fields.push(FieldDef::new("id", RustType::Uuid));
        registry.register_table(apple);

        let output = emit_string(&registry).unwrap();

        let a_pos = output.find("\"a_first\"").expect("a_first present");
        let z_pos = output.find("\"z_last\"").expect("z_last present");
        assert!(
            a_pos < z_pos,
            "functions must be sorted: a_first at {a_pos}, z_last at {z_pos}"
        );

        let apple_pos = output.find("\"Apple\"").expect("Apple present");
        let zebra_pos = output.find("\"Zebra\"").expect("Zebra present");
        assert!(
            apple_pos < zebra_pos,
            "types must be sorted: Apple at {apple_pos}, Zebra at {zebra_pos}"
        );
    }
}
