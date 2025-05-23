mod code;
mod file;
mod parser;

use file::MarkedFile;
use parser::ParsedTableMacro;
pub use parser::FILE_SIGNATURE;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default, Debug, Clone)]
pub struct TableOptions<'a> {
    ignore: Option<bool>,
    /// Names used for autogenerated columns which are NOT primary keys (for example: `created_at`, `updated_at`, etc.).
    autogenerated_columns: Option<Vec<&'a str>>,

    #[cfg(feature = "tsync")]
    /// Adds #[tsync] attribute to structs (see https://github.com/Wulf/tsync)
    tsync: Option<bool>,

    #[cfg(feature = "async")]
    /// Uses diesel_async for generated functions (see https://github.com/weiznich/diesel_async)
    use_async: Option<bool>,
}

impl<'a> TableOptions<'a> {
    pub fn get_ignore(&self) -> bool {
        self.ignore.unwrap_or_default()
    }

    #[cfg(feature = "tsync")]
    pub fn get_tsync(&self) -> bool {
        self.tsync.unwrap_or_default()
    }

    #[cfg(feature = "async")]
    pub fn get_async(&self) -> bool {
        self.use_async.unwrap_or_default()
    }

    pub fn get_autogenerated_columns(&self) -> &[&'_ str] {
        self.autogenerated_columns.as_deref().unwrap_or_default()
    }

    pub fn ignore(self) -> Self {
        Self {
            ignore: Some(true),
            ..self
        }
    }

    #[cfg(feature = "tsync")]
    pub fn tsync(self) -> Self {
        Self {
            tsync: Some(true),
            ..self
        }
    }

    #[cfg(feature = "async")]
    pub fn use_async(self) -> Self {
        Self {
            use_async: Some(true),
            ..self
        }
    }

    pub fn autogenerated_columns(self, cols: Vec<&'a str>) -> Self {
        Self {
            autogenerated_columns: Some(cols.clone()),
            ..self
        }
    }

    /// Fills any `None` properties with values from another TableConfig
    pub fn apply_defaults(&self, other: &TableOptions<'a>) -> Self {
        Self {
            ignore: self.ignore.or(other.ignore),
            #[cfg(feature = "tsync")]
            tsync: self.tsync.or(other.tsync),
            #[cfg(feature = "async")]
            use_async: self.use_async.or(other.use_async),
            autogenerated_columns: self
                .autogenerated_columns
                .clone()
                .or_else(|| other.autogenerated_columns.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GenerationConfig<'a> {
    pub table_options: HashMap<&'a str, TableOptions<'a>>,
    pub default_table_options: TableOptions<'a>,
    pub connection_type: String,
}

impl GenerationConfig<'_> {
    pub fn table(&self, name: &str) -> TableOptions<'_> {
        let t = self
            .table_options
            .get(name)
            .unwrap_or(&self.default_table_options);

        t.apply_defaults(&self.default_table_options)
    }
}

pub fn generate_code(
    diesel_schema_file_contents: String,
    config: GenerationConfig,
) -> anyhow::Result<Vec<ParsedTableMacro>> {
    parser::parse_and_generate_code(diesel_schema_file_contents, &config)
}

pub fn generate_files(
    input_diesel_schema_file: PathBuf,
    output_models_dir: PathBuf,
    config: GenerationConfig,
) {
    let input = input_diesel_schema_file;
    let output_dir = output_models_dir;

    let generated = generate_code(
        std::fs::read_to_string(input).expect("Could not read schema file."),
        config,
    )
    .expect("An error occurred.");

    if !output_dir.exists() {
        std::fs::create_dir(&output_dir)
            .unwrap_or_else(|_| panic!("Could not create directory '{output_dir:#?}'"));
    } else if !output_dir.is_dir() {
        panic!("Expected output argument to be a directory or non-existent.")
    }

    // check that the mod.rs file exists
    let mut mod_rs = MarkedFile::new(output_dir.join("mod.rs"));

    // pass 1: add code for new tables
    for table in generated.iter() {
        let table_dir = output_dir.join(table.name.to_string());

        if !table_dir.exists() {
            std::fs::create_dir(&table_dir)
                .unwrap_or_else(|_| panic!("Could not create directory '{table_dir:#?}'"));
        }

        if !table_dir.is_dir() {
            panic!("Expected a directory at '{table_dir:#?}'")
        }

        let mut table_generated_rs = MarkedFile::new(table_dir.join("generated.rs"));
        let mut table_mod_rs = MarkedFile::new(table_dir.join("mod.rs"));

        table_generated_rs.ensure_file_signature();
        table_generated_rs.file_contents = table.generated_code.clone();
        table_generated_rs.write();

        table_mod_rs.ensure_mod_stmt("generated");
        table_mod_rs.ensure_use_stmt("generated::*");
        table_mod_rs.write();

        mod_rs.ensure_mod_stmt(table.name.to_string().as_str());
    }

    // pass 2: delete code for removed tables
    for item in std::fs::read_dir(&output_dir)
        .unwrap_or_else(|_| panic!("Could not read directory '{output_dir:#?}'"))
    {
        let item = item.unwrap_or_else(|_| panic!("Could not read item in '{output_dir:#?}'"));

        // check if item is a directory
        let file_type = item
            .file_type()
            .unwrap_or_else(|_| panic!("Could not determine type of file '{:#?}'", item.path()));
        if !file_type.is_dir() {
            continue;
        }

        // check if it's a generated file
        let generated_rs_path = item.path().join("generated.rs");
        if !generated_rs_path.exists()
            || !generated_rs_path.is_file()
            || !MarkedFile::new(generated_rs_path.clone()).has_file_signature()
        {
            continue;
        }

        // okay, it's generated, but we need to check if it's for a deleted table
        let file_name = item.file_name();
        let associated_table_name = file_name
            .to_str()
            .unwrap_or_else(|| panic!("Could not determine name of file '{:#?}'", item.path()));
        let found = generated.iter().find(|g| {
            g.name
                .to_string()
                .eq_ignore_ascii_case(associated_table_name)
        });
        if found.is_some() {
            continue;
        }

        // this table was deleted, let's delete the generated code
        std::fs::remove_file(&generated_rs_path)
            .unwrap_or_else(|_| panic!("Could not delete redundant file '{generated_rs_path:#?}'"));

        // remove the mod.rs file if there isn't anything left in there except the use stmt
        let table_mod_rs_path = item.path().join("mod.rs");
        if table_mod_rs_path.exists() {
            let mut table_mod_rs = MarkedFile::new(table_mod_rs_path);

            table_mod_rs.remove_mod_stmt("generated");
            table_mod_rs.remove_use_stmt("generated::*");
            table_mod_rs.write();

            if table_mod_rs.file_contents.trim().is_empty() {
                table_mod_rs.delete()
            } else {
                table_mod_rs.write() // write the changes we made above
            }
        }

        // delete the table dir if there's nothing else in there
        let is_empty = item
            .path()
            .read_dir()
            .unwrap_or_else(|_| panic!("Could not read directory {:#?}", item.path()))
            .next()
            .is_none();
        if is_empty {
            std::fs::remove_dir(item.path())
                .unwrap_or_else(|_| panic!("Could not delete directory '{:#?}'", item.path()));
        }

        // remove the module from the main mod_rs file
        mod_rs.remove_mod_stmt(associated_table_name);
    }

    mod_rs.write();
}
