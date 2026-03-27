//! Plugin boilerplate generator for FutureSDR blocks.
//!
//! Reads a block source file, auto-detects the `#[derive(Block)]` struct,
//! extracts the `new()` constructor signature, and generates a complete
//! plugin crate (Cargo.toml + src/lib.rs) with the appropriate
//! `export_plugin!` invocation.
//!
//! # Usage
//!
//! ```sh
//! # Auto-detect everything from the source file:
//! cargo run -p plugin_gen -- --block-src src/blocks/head.rs
//!
//! # Override the output directory:
//! cargo run -p plugin_gen -- --block-src src/blocks/head.rs \
//!     --output examples/dylib/plugins/head_plugin
//!
//! # Override the type param:
//! cargo run -p plugin_gen -- --block-src src/blocks/head.rs --type-param f32
//! ```
//!
//! # Supported constructor patterns
//!
//! - No parameters: `NullSink::new()` → config `()`
//! - Single scalar: `Head::new(n_items: u64)` → config `u64`
//! - Multiple params: `FileSource::new(path: impl AsRef<Path>, repeat: bool)` → config `(String, bool)`
//! - `impl AsRef<Path>` / `impl Into<String>` / `S: AsRef<str>` → mapped to `String`
//! - `Vec<T>` where T is the generic param → `Vec<ConcreteType>`
//! - Closures / `FnMut` → generated with `todo!()` placeholder for manual editing.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use syn::{
    Attribute, FnArg, GenericParam, ImplItemFn, Item, ItemImpl, ItemStruct, Pat, PatType, Type,
    TypeImplTrait, TypeParam, TypePath, TypeReference, Visibility,
};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "generate",
    about = "Generate FutureSDR plugin crate from a block source file"
)]
struct Args {
    /// Path to the block's .rs source file (e.g. src/blocks/head.rs)
    #[arg(long)]
    block_src: PathBuf,

    /// Override: name of the block struct (auto-detected from #[derive(Block)])
    #[arg(long)]
    struct_name: Option<String>,

    /// Override: concrete type for the first generic type parameter (auto: "u8", or "none" if non-generic)
    #[arg(long)]
    type_param: Option<String>,

    /// Override: output directory (auto: examples/dylib/plugins/<snake_name>_plugin)
    #[arg(long)]
    output: Option<PathBuf>,

    /// Plugin description (shown in block_description())
    #[arg(long, default_value = "")]
    description: String,

    /// Path from the generated crate to plugin_api
    #[arg(long, default_value = "../../crates/plugin_api")]
    plugin_api_path: String,

    /// Path from the generated crate to the futuresdr root
    #[arg(long, default_value = "../../")]
    futuresdr_path: String,
}

// ---------------------------------------------------------------------------
// Extracted parameter info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Param {
    name: String,
    /// The original syn type (for diagnostics)
    original_ty: String,
    /// The mapped, plugin-compatible type
    config_ty: String,
    /// true when the param is an unsupported closure / Fn trait
    is_closure: bool,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = Args::parse();

    let source = fs::read_to_string(&args.block_src).unwrap_or_else(|e| {
        eprintln!("error: cannot read {}: {}", args.block_src.display(), e);
        std::process::exit(1);
    });

    let ast = syn::parse_file(&source).unwrap_or_else(|e| {
        eprintln!("error: cannot parse {}: {}", args.block_src.display(), e);
        std::process::exit(1);
    });

    // 1. Auto-detect struct name from #[derive(Block)]
    let struct_name = args
        .struct_name
        .clone()
        .or_else(|| find_block_struct(&ast))
        .unwrap_or_else(|| {
            eprintln!(
                "error: no #[derive(Block)] struct found in {}. Use --struct-name to specify.",
                args.block_src.display()
            );
            std::process::exit(1);
        });

    // 2. Auto-detect first generic type param
    let first_type_param = find_first_type_param(&ast, &struct_name);
    let type_param = args.type_param.clone().unwrap_or_else(|| {
        if first_type_param.is_some() {
            "u8".to_string()
        } else {
            "none".to_string()
        }
    });

    // 3. Find `pub fn new(...)` and extract parameters
    let params = find_new_params(&ast, &struct_name, first_type_param.as_deref(), &type_param);

    let has_closures = params.iter().any(|p| p.is_closure);
    if params.iter().any(|p| p.config_ty == "UNSUPPORTED") {
        eprintln!(
            "warning: some constructor parameters could not be mapped to plugin-compatible types."
        );
        eprintln!("         You will need to edit the generated code manually.");
    }

    // 4. Build config type string
    let config_type = match params.len() {
        0 => "()".to_string(),
        1 => params[0].config_ty.clone(),
        _ => format!(
            "({})",
            params
                .iter()
                .map(|p| p.config_ty.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    // 5. Build the create closure body
    let create_body = build_create_body(&struct_name, &type_param, &params, has_closures);

    // 6. Build the import path
    let import_path = build_import_path(&args.block_src, &struct_name);

    // 7. Description
    let description = if args.description.is_empty() {
        format!("{} block plugin", struct_name)
    } else {
        args.description.clone()
    };

    // 8. Output dir
    let plugin_name = to_snake_case(&struct_name);
    let crate_name = format!("{}_plugin", plugin_name);
    let output = args.output.clone().unwrap_or_else(|| {
        PathBuf::from(format!("plugins/{}", crate_name))
    });

    // 9. Generate files
    generate_cargo_toml(&output, &crate_name, &args.plugin_api_path, &args.futuresdr_path);
    generate_lib_rs(
        &output,
        &import_path,
        &struct_name,
        &description,
        &config_type,
        &create_body,
        has_closures,
    );

    println!("generated plugin crate: {}", output.display());
    println!("  crate name:  {}", crate_name);
    println!("  struct:      {}", struct_name);
    println!("  type param:  {}", type_param);
    println!("  config type: {}", config_type);
    if has_closures {
        println!("  NOTE: contains closure params — edit src/lib.rs to fill in the closure body");
    }
}

// ---------------------------------------------------------------------------
// AST analysis
// ---------------------------------------------------------------------------

/// Find the struct annotated with `#[derive(Block)]`.
fn find_block_struct(ast: &syn::File) -> Option<String> {
    for item in &ast.items {
        if let Item::Struct(s) = item {
            if has_derive_block(&s.attrs) {
                return Some(s.ident.to_string());
            }
        }
    }
    None
}

/// Check if attrs contain `#[derive(Block)]` or `#[derive(..., Block, ...)]`.
fn has_derive_block(attrs: &[Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("derive") {
            let tokens = quote_to_string(attr);
            // Simple check: does the derive attribute contain "Block"
            if tokens.contains("Block") {
                return true;
            }
        }
    }
    false
}

/// Find the name of the first type parameter on the struct definition.
/// Skips const generics.
/// e.g. for `struct Head<T: Copy + Send, I: ..., O: ...>` returns Some("T").
/// For `struct MovingAvg<const WIDTH: usize, I, O>` returns None (WIDTH is const).
fn find_first_type_param(ast: &syn::File, struct_name: &str) -> Option<String> {
    for item in &ast.items {
        if let Item::Struct(s) = item {
            if s.ident == struct_name {
                for param in &s.generics.params {
                    if let GenericParam::Type(TypeParam { ident, bounds, .. }) = param {
                        // Skip buffer reader/writer types (I, O, IN, OUT patterns)
                        // that have CpuBufferReader or CpuBufferWriter bounds
                        let bounds_str = quote_to_string(bounds);
                        if bounds_str.contains("CpuBufferReader")
                            || bounds_str.contains("CpuBufferWriter")
                        {
                            continue;
                        }
                        return Some(ident.to_string());
                    }
                }
                return None;
            }
        }
    }
    None
}

/// Find `pub fn new(...)` in an `impl ... StructName<...>` block and extract parameters.
fn find_new_params(
    ast: &syn::File,
    struct_name: &str,
    first_type_param: Option<&str>,
    concrete_type: &str,
) -> Vec<Param> {
    for item in &ast.items {
        let Item::Impl(impl_block) = item else {
            continue;
        };

        // Match impl blocks for our struct (skip trait impls)
        if impl_block.trait_.is_some() {
            continue;
        }
        if !impl_self_type_matches(impl_block, struct_name) {
            continue;
        }

        // Collect the generic type parameter names on the impl block itself.
        let impl_generics = collect_impl_generics(impl_block);

        for impl_item in &impl_block.items {
            let syn::ImplItem::Fn(method) = impl_item else {
                continue;
            };
            if method.sig.ident != "new" {
                continue;
            }
            if !matches!(method.vis, Visibility::Public(_)) {
                continue;
            }

            return extract_params(method, first_type_param, concrete_type, &impl_generics);
        }
    }

    eprintln!(
        "warning: could not find `pub fn new(...)` in any impl block for `{}`",
        struct_name
    );
    Vec::new()
}

/// Check if an impl block's self type is `StructName` or `StructName<...>`.
fn impl_self_type_matches(impl_block: &ItemImpl, struct_name: &str) -> bool {
    match &*impl_block.self_ty {
        Type::Path(TypePath { path, .. }) => path
            .segments
            .last()
            .map_or(false, |seg| seg.ident == struct_name),
        _ => false,
    }
}

/// Collect generic type params from the impl block, mapping param name → trait bound strings.
fn collect_impl_generics(impl_block: &ItemImpl) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    for param in &impl_block.generics.params {
        if let GenericParam::Type(tp) = param {
            let name = tp.ident.to_string();
            let bounds: Vec<String> = tp
                .bounds
                .iter()
                .map(|b| quote_to_string(b))
                .collect();
            result.push((name, bounds));
        }
    }
    result
}

/// Collect generic type params from a method signature.
fn collect_method_generics(method: &ImplItemFn) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();

    for param in &method.sig.generics.params {
        if let GenericParam::Type(tp) = param {
            let name = tp.ident.to_string();
            let bounds: Vec<String> = tp.bounds.iter().map(|b| quote_to_string(b)).collect();
            result.push((name, bounds));
        }
    }

    if let Some(where_clause) = &method.sig.generics.where_clause {
        for pred in &where_clause.predicates {
            if let syn::WherePredicate::Type(pt) = pred {
                let bounded_ty = quote_to_string(&pt.bounded_ty);
                for (name, bounds) in &mut result {
                    if *name == bounded_ty {
                        for bound in &pt.bounds {
                            bounds.push(quote_to_string(bound));
                        }
                    }
                }
            }
        }
    }

    result
}

/// Extract parameters from the `new()` method signature.
fn extract_params(
    method: &ImplItemFn,
    first_type_param: Option<&str>,
    concrete_type: &str,
    impl_generics: &[(String, Vec<String>)],
) -> Vec<Param> {
    let mut params = Vec::new();

    // Merge impl-block generics with method-level generics
    let method_generics = collect_method_generics(method);
    let mut all_generics: Vec<(String, Vec<String>)> = impl_generics.to_vec();
    all_generics.extend(method_generics);

    for arg in &method.sig.inputs {
        let FnArg::Typed(PatType { pat, ty, .. }) = arg else {
            continue;
        };

        let name = match &**pat {
            Pat::Ident(pi) => pi.ident.to_string(),
            _ => "_".to_string(),
        };

        let original_ty = quote_to_string(&**ty);
        let (config_ty, is_closure) =
            map_type(ty, first_type_param, concrete_type, &all_generics);

        params.push(Param {
            name,
            original_ty,
            config_ty,
            is_closure,
        });
    }

    params
}

// ---------------------------------------------------------------------------
// Type mapping
// ---------------------------------------------------------------------------

/// Map a syn::Type to a plugin-compatible config type string.
/// Returns (type_string, is_closure).
fn map_type(
    ty: &Type,
    first_type_param: Option<&str>,
    concrete_type: &str,
    impl_generics: &[(String, Vec<String>)],
) -> (String, bool) {
    match ty {
        // `impl AsRef<Path>`, `impl Into<String>`, `impl FnMut(...)`, etc.
        Type::ImplTrait(TypeImplTrait { bounds, .. }) => {
            let bounds_str = quote_to_string(bounds);
            if is_fn_bound(&bounds_str) {
                return ("CLOSURE".to_string(), true);
            }
            map_trait_bounds(&bounds_str)
        }

        // Direct type path: u64, Vec<T>, String, S (where S: AsRef<str>), F (where F: FnMut), etc.
        Type::Path(TypePath { path, .. }) => {
            let full = quote_to_string(path);

            if path.segments.len() == 1 {
                let seg_name = path.segments[0].ident.to_string();

                // Is it the struct's first type param (e.g. T → u8)?
                if let Some(tp) = first_type_param {
                    if seg_name == tp {
                        return (concrete_type.to_string(), false);
                    }
                }

                // Is it a generic on the impl/method block with trait bounds?
                for (gen_name, bounds) in impl_generics {
                    if seg_name == *gen_name {
                        let all_bounds = bounds.join(" + ");
                        if is_fn_bound(&all_bounds) {
                            return ("CLOSURE".to_string(), true);
                        }
                        return map_trait_bounds(&all_bounds);
                    }
                }
            }

            // Handle Vec<T> → Vec<concrete>
            if path.segments.len() == 1 && path.segments[0].ident == "Vec" {
                if let syn::PathArguments::AngleBracketed(args) = &path.segments[0].arguments {
                    if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                        let (inner_mapped, _) =
                            map_type(inner, first_type_param, concrete_type, impl_generics);
                        return (format!("Vec<{}>", inner_mapped), false);
                    }
                }
            }

            // Handle Option<T>
            if path.segments.len() == 1 && path.segments[0].ident == "Option" {
                if let syn::PathArguments::AngleBracketed(args) = &path.segments[0].arguments {
                    if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                        let (inner_mapped, ic) =
                            map_type(inner, first_type_param, concrete_type, impl_generics);
                        return (format!("Option<{}>", inner_mapped), ic);
                    }
                }
            }

            // Concrete type — pass through
            (full, false)
        }

        // &str, &[f32], etc. — map references to owned types
        Type::Reference(TypeReference { elem, .. }) => {
            let inner = quote_to_string(&**elem);
            match inner.as_str() {
                "str" => ("String".to_string(), false),
                _ => {
                    // &[f32] → Vec<f32>
                    if let Type::Slice(slice) = &**elem {
                        let (inner_mapped, ic) =
                            map_type(&slice.elem, first_type_param, concrete_type, impl_generics);
                        return (format!("Vec<{}>", inner_mapped), ic);
                    }
                    map_type(elem, first_type_param, concrete_type, impl_generics)
                }
            }
        }

        // Tuples
        Type::Tuple(tuple) => {
            let mut any_closure = false;
            let mapped: Vec<String> = tuple
                .elems
                .iter()
                .map(|e| {
                    let (t, ic) = map_type(e, first_type_param, concrete_type, impl_generics);
                    any_closure |= ic;
                    t
                })
                .collect();
            (format!("({})", mapped.join(", ")), any_closure)
        }

        _ => {
            eprintln!(
                "warning: unsupported type `{}`, marking as UNSUPPORTED",
                quote_to_string(ty)
            );
            ("UNSUPPORTED".to_string(), false)
        }
    }
}

/// Check if a bounds string contains Fn/FnMut/FnOnce.
fn is_fn_bound(bounds: &str) -> bool {
    let normalized = bounds.replace(' ', "");
    normalized.contains("FnMut") || normalized.contains("FnOnce") || normalized.contains("Fn(")
}

/// Map common trait bound patterns to concrete config types.
fn map_trait_bounds(bounds: &str) -> (String, bool) {
    let normalized = bounds.replace(' ', "");

    if normalized.contains("AsRef<Path>") || normalized.contains("AsRef<std::path::Path>") {
        ("String".to_string(), false)
    } else if normalized.contains("Into<String>") {
        ("String".to_string(), false)
    } else if normalized.contains("AsRef<str>") {
        ("String".to_string(), false)
    } else if normalized.contains("ToString") {
        ("String".to_string(), false)
    } else {
        eprintln!(
            "warning: unsupported trait bound `{}`, marking as UNSUPPORTED",
            bounds
        );
        ("UNSUPPORTED".to_string(), false)
    }
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

/// Build the constructor call for the create closure.
fn build_create_body(struct_name: &str, type_param: &str, params: &[Param], has_closures: bool) -> String {
    if has_closures {
        return format!("todo!(\"fill in closure params for {}\")", struct_name);
    }

    let turbofish = if type_param != "none" {
        format!("::<{}>", type_param)
    } else {
        String::new()
    };

    match params.len() {
        0 => format!("{}{}::new()", struct_name, turbofish),
        1 => format!("{}{}::new(cfg)", struct_name, turbofish),
        _ => {
            let args: Vec<String> = params
                .iter()
                .enumerate()
                .map(|(i, _)| format!("cfg.{}", i))
                .collect();
            format!("{}{}::new({})", struct_name, turbofish, args.join(", "))
        }
    }
}

/// Infer the `use` import path from the source file path.
fn build_import_path(block_src: &PathBuf, struct_name: &str) -> String {
    let path_str = block_src.to_string_lossy();

    if path_str.contains("src/blocks/") {
        return format!("futuresdr::blocks::{}", struct_name);
    }

    // Fallback
    format!("futuresdr::blocks::{}", struct_name)
}

fn generate_cargo_toml(
    output: &PathBuf,
    crate_name: &str,
    plugin_api_path: &str,
    futuresdr_path: &str,
) {
    fs::create_dir_all(output.join("src")).unwrap_or_else(|e| {
        eprintln!("error: cannot create {}/src: {}", output.display(), e);
        std::process::exit(1);
    });

    let mut content = String::new();
    writeln!(content, "[package]").unwrap();
    writeln!(content, "name = \"{}\"", crate_name).unwrap();
    writeln!(content, "version = \"0.1.0\"").unwrap();
    writeln!(content, "edition = \"2021\"").unwrap();
    writeln!(content).unwrap();
    writeln!(content, "[lib]").unwrap();
    writeln!(content, "crate-type = [\"dylib\"]").unwrap();
    writeln!(content).unwrap();
    writeln!(content, "[dependencies]").unwrap();
    writeln!(
        content,
        "plugin_api = {{ path = \"{}\" }}",
        plugin_api_path
    )
    .unwrap();
    writeln!(
        content,
        "futuresdr = {{ path = \"{}\", features = [\"plugin\"] }}",
        futuresdr_path
    )
    .unwrap();

    let cargo_path = output.join("Cargo.toml");
    fs::write(&cargo_path, content).unwrap_or_else(|e| {
        eprintln!("error: cannot write {}: {}", cargo_path.display(), e);
        std::process::exit(1);
    });
}

fn generate_lib_rs(
    output: &PathBuf,
    import_path: &str,
    struct_name: &str,
    description: &str,
    config_type: &str,
    create_body: &str,
    has_closures: bool,
) {
    let cfg_ident = if config_type == "()" { "_cfg" } else { "cfg" };

    let mut content = String::new();
    if has_closures {
        writeln!(
            content,
            "// TODO: This block takes closure/function parameters."
        )
        .unwrap();
        writeln!(
            content,
            "//       Edit the `create` closure below to provide the concrete implementation."
        )
        .unwrap();
    }
    writeln!(content, "use {};", import_path).unwrap();
    writeln!(content).unwrap();
    writeln!(content, "plugin_api::export_plugin! {{").unwrap();
    writeln!(content, "    name: \"{}\",", struct_name).unwrap();
    writeln!(content, "    description: \"{}\",", description).unwrap();
    writeln!(content, "    config: {},", config_type).unwrap();
    writeln!(content, "    create: |{}, _id| {{", cfg_ident).unwrap();
    writeln!(content, "        {}", create_body).unwrap();
    writeln!(content, "    }}").unwrap();
    writeln!(content, "}}").unwrap();

    let lib_path = output.join("src").join("lib.rs");
    fs::write(&lib_path, &content).unwrap_or_else(|e| {
        eprintln!("error: cannot write {}: {}", lib_path.display(), e);
        std::process::exit(1);
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn quote_to_string(tokens: &impl quote::ToTokens) -> String {
    let ts = quote::quote!(#tokens);
    ts.to_string()
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_ascii_lowercase());
        } else {
            result.push(ch);
        }
    }
    result
}
