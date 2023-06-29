use std::env;

use codegen::{Scope, Type};
use inflector::cases::classcase::to_class_case;
use itertools::Itertools;
use sqlx::{postgres::PgPoolOptions, PgPool};

#[tokio::main]
async fn main() {
    do_it().await.unwrap();
}

#[derive(sqlx::FromRow)]
pub struct TableDefinition {
    table_name: String,
    column_name: String,
    udt_name: String,
    is_nullable: bool,
    ordinal_position: i32,
}

async fn do_it() -> Result<(), anyhow::Error> {
    let db_url = env::var("DATABASE_URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .expect("Couldnt connect");

    println!("connecting to db");
    sqlx::query("SELECT 1;")
        .execute(&pool)
        .await
        .expect("Could not connect");

    println!("db connected");

    let tables: Vec<TableDefinition> = sqlx::query_as(
        "SELECT table_name, column_name, is_nullable = 'YES' as is_nullable , udt_name, ordinal_position FROM information_schema.columns WHERE table_schema='public' ORDER BY table_name, ordinal_position"
    )
    .fetch_all(&pool)
    .await?;

    println!("Fetched");

    let grouped = tables.into_iter().group_by(|t| t.table_name.clone());
    let mut scope = Scope::new();
    scope.import("sqlx", "PgConnection");
    for (table_name, columns) in &grouped {
        println!("Working on: {table_name}");
        if !should_emit(&table_name) {
            continue;
        }

        let columns_vec: Vec<(String, String, bool)> = columns
            .map(|c| (c.column_name, c.udt_name, c.is_nullable))
            .collect();
        add_structs_for_table(&mut scope, &table_name, &columns_vec);
        // add_insert_for_table(&mut scope, &table_name, &columns_vec);
        // add_select_for_table(&mut scope, &table_name, &columns_vec);
    }
    println!("{}", scope.to_string());
    Ok(())
}

fn should_emit(table_name: &str) -> bool {
    return table_name != "_sqlx_migrations"; // && table_name == "customers";
}

fn add_insert_for_table(scope: &mut Scope, table_name: &str, columns: &[(String, String, bool)]) {
    let new_fn = scope.new_fn(&format!("insert_{}", table_name));
    new_fn.set_async(true);
    new_fn.vis("pub");
    new_fn.arg("conn", Type::new("&mut PgConnection"));
    new_fn.arg(
        "row",
        Type::new(&format!("&{}", input_row_struct_name(table_name))),
    );
    new_fn.ret(Type::new(&format!(
        "Result<{}, sqlx::Error>",
        row_struct_name(table_name)
    )));
    let columns: Vec<_> = columns.iter().filter(|c| c.0 != "id").collect();
    let insert_name_list = columns.iter().map(|c| format!("\"{}\"", c.0)).join(", ");
    let args_list = columns.iter().map(|c| format!("row.{}", c.0)).join(", ");
    let insert_placeholders = columns
        .iter()
        .enumerate()
        .map(|(i, _)| format!("${}", (i + 1).to_string()))
        .join(", ");

    let body = format!(
        r##"
    let result = sqlx::query_as!({},
        r#"INSERT INTO {} ({})
        VALUES ({}) RETURNING *"#, {}
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(result)"##,
        row_struct_name(table_name),
        table_name,
        insert_name_list,
        insert_placeholders,
        args_list
    );
    new_fn.line(body);
}

fn add_select_for_table(scope: &mut Scope, table_name: &str, columns: &[(String, String, bool)]) {
    let new_fn = scope.new_fn(&format!("select_{}", table_name));
    new_fn.set_async(true);
    new_fn.vis("pub");
    new_fn.arg("conn", Type::new("&mut PgConnection"));
    new_fn.arg(
        "id",
        Type::new(&format!(
            "&{}",
            pg_type_to_rs_type(&columns.iter().find(|c| c.0 == "id").unwrap().1)
        )),
    );
    new_fn.ret(Type::new(&format!(
        "Result<{}, sqlx::Error>",
        row_struct_name(table_name)
    )));
    let insert_name_list = columns.iter().map(|c| format!("\"{}\"", c.0)).join(", ");

    let body = format!(
        r##"
    let result = sqlx::query_as!({},
        r#"SELECT {} FROM {} WHERE id=$1"#, id
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(result)"##,
        row_struct_name(table_name),
        insert_name_list,
        table_name
    );
    new_fn.line(body);
}

fn input_row_struct_name(table_name: &str) -> String {
    format!("{}Input", to_class_case(table_name))
}

fn row_struct_name(table_name: &str) -> String {
    format!("{}", to_class_case(table_name))
}

fn add_structs_for_table(scope: &mut Scope, table_name: &str, columns: &[(String, String, bool)]) {
    let new_struct = scope.new_struct(&row_struct_name(table_name));
    new_struct.vis("pub");
    for column in columns {
        if !column.2 {
            new_struct.field(&format!("pub {}", column.0), &pg_type_to_rs_type(&column.1));
        } else {
            new_struct.field(
                &format!("pub {}", column.0),
                format!("Option<{}>", &pg_type_to_rs_type(&column.1)),
            );
        }
    }
    //let new_in_struct = scope.new_struct(&input_row_struct_name(table_name));
    // new_in_struct.vis("pub");
    // for column in columns {
    //     if column.0 != "id" {
    //         new_in_struct.field(&format!("pub {}", column.0), &pg_type_to_rs_type(&column.1));
    //     }
    // }
}

fn pg_type_to_rs_type(pg_type: &str) -> String {
    match pg_type {
        "int8" => "i64",
        "int4" => "i32",
        "int2" => "i16",
        "text" => "String",
        "varchar" => "String",
        "jsonb" => "sqlx::Json",
        "timestamptz" => "chrono::DateTime<chrono::Utc>",
        "date" => "chrono::NaiveDate",
        "float4" => "f32",
        "float8" => "f64",
        "uuid" => "uuid::Uuid",
        "boolean" => "bool",
        "bytea" => "Vec<u8>", // is this right?
        _ => panic!("Unknown type: {}", pg_type),
    }
    .to_string()
}
