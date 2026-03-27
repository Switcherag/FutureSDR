//! Dynamic config value system for parsing plugin configs from TOML.
//!
//! [`ConfigValue`] is a type-erased intermediate representation of a config.
//! [`FromConfigValue`] converts it into the concrete Rust type expected by each plugin.
//! [`parse_typed_config`] dispatches by type name string, covering all common plugin
//! config types.
//!
//! # Supported types
//!
//! | TOML                     | `config_type`               | Rust type               |
//! |--------------------------|-----------------------------|-------------------------|
//! | *(absent)*               | `"()"`                      | `()`                    |
//! | `config = true`          | `"bool"`                    | `bool`                  |
//! | `config = 42`            | `"u8"` / `"u32"` / `"u64"` / `"usize"` / `"i64"` / `"isize"` | numeric |
//! | `config = 3.14`          | `"f32"` / `"f64"`           | `f32` / `f64`           |
//! | `config = "hello"`       | `"String"`                  | `String`                |
//! | `config = [3, 5]`        | `"(usize, usize)"`          | tuple                   |
//! | `config = [48000, 2]`    | `"(u32, u16)"`              | `(u32, u16)`            |
//! | `config = [1, 2, 3]`     | `"Vec<u8>"`                 | `Vec<u8>`               |
//!
//! If `config_type` is omitted, auto-detection maps TOML types:
//! string → `String`, float → `f64`, integer → `u64`, boolean → `bool`, absent → `()`.

use std::any::Any;
use std::collections::HashMap;

/// Type-erased intermediate config value, convertible from TOML.
#[derive(Debug, Clone)]
pub enum ConfigValue {
    Unit,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<ConfigValue>),
    Table(HashMap<String, ConfigValue>),
}

impl ConfigValue {
    /// Convert from a `toml::Value`.
    pub fn from_toml(v: toml::Value) -> Self {
        match v {
            toml::Value::Boolean(b) => ConfigValue::Bool(b),
            toml::Value::Integer(i) => ConfigValue::Integer(i),
            toml::Value::Float(f) => ConfigValue::Float(f),
            toml::Value::String(s) => ConfigValue::String(s),
            toml::Value::Array(arr) => {
                ConfigValue::Array(arr.into_iter().map(ConfigValue::from_toml).collect())
            }
            toml::Value::Table(t) => {
                ConfigValue::Table(t.into_iter().map(|(k, v)| (k, ConfigValue::from_toml(v))).collect())
            }
            toml::Value::Datetime(dt) => ConfigValue::String(dt.to_string()),
        }
    }

    pub fn as_i64(&self) -> Result<i64, String> {
        match self {
            ConfigValue::Integer(i) => Ok(*i),
            other => Err(format!("expected integer, got {other:?}")),
        }
    }

    pub fn as_f64(&self) -> Result<f64, String> {
        match self {
            ConfigValue::Float(f) => Ok(*f),
            ConfigValue::Integer(i) => Ok(*i as f64),
            other => Err(format!("expected float, got {other:?}")),
        }
    }

    pub fn as_bool(&self) -> Result<bool, String> {
        match self {
            ConfigValue::Bool(b) => Ok(*b),
            other => Err(format!("expected bool, got {other:?}")),
        }
    }

    pub fn as_string(&self) -> Result<String, String> {
        match self {
            ConfigValue::String(s) => Ok(s.clone()),
            other => Err(format!("expected string, got {other:?}")),
        }
    }

    pub fn into_array(self) -> Result<Vec<ConfigValue>, String> {
        match self {
            ConfigValue::Array(a) => Ok(a),
            other => Err(format!("expected array, got {other:?}")),
        }
    }

    /// Auto-detect type and convert (used when no `config_type` is specified).
    pub fn auto_convert(self) -> Result<Box<dyn Any + Send>, String> {
        match self {
            ConfigValue::Unit => Ok(Box::new(())),
            ConfigValue::String(s) => Ok(Box::new(s)),
            ConfigValue::Float(f) => Ok(Box::new(f)),
            ConfigValue::Integer(i) => Ok(Box::new(i as u64)),
            ConfigValue::Bool(b) => Ok(Box::new(b)),
            other => Err(format!(
                "cannot auto-convert {other:?} — specify config_type explicitly"
            )),
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// FromConfigValue trait
// ════════════════════════════════════════════════════════════════════

/// Trait for converting a [`ConfigValue`] into a concrete Rust type.
pub trait FromConfigValue: Sized + Send + 'static {
    fn from_config(value: ConfigValue) -> Result<Self, String>;

    fn boxed_from_config(value: ConfigValue) -> Result<Box<dyn Any + Send>, String> {
        Ok(Box::new(Self::from_config(value)?))
    }
}

// ── Primitives ───────────────────────────────────────────────────

impl FromConfigValue for () {
    fn from_config(value: ConfigValue) -> Result<Self, String> {
        match value {
            ConfigValue::Unit => Ok(()),
            _ => Err("expected unit (no config)".into()),
        }
    }
}

impl FromConfigValue for bool {
    fn from_config(value: ConfigValue) -> Result<Self, String> {
        value.as_bool()
    }
}

impl FromConfigValue for String {
    fn from_config(value: ConfigValue) -> Result<Self, String> {
        value.as_string()
    }
}

impl FromConfigValue for f64 {
    fn from_config(value: ConfigValue) -> Result<Self, String> {
        value.as_f64()
    }
}

impl FromConfigValue for f32 {
    fn from_config(value: ConfigValue) -> Result<Self, String> {
        value.as_f64().map(|f| f as f32)
    }
}

macro_rules! impl_int_from_config {
    ($($ty:ty),*) => {
        $(
            impl FromConfigValue for $ty {
                fn from_config(value: ConfigValue) -> Result<Self, String> {
                    value.as_i64().map(|i| i as $ty)
                }
            }
        )*
    };
}

impl_int_from_config!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

// ── Vec<T> ───────────────────────────────────────────────────────

impl<T: FromConfigValue> FromConfigValue for Vec<T> {
    fn from_config(value: ConfigValue) -> Result<Self, String> {
        let arr = value.into_array()?;
        arr.into_iter().map(T::from_config).collect()
    }
}

// ── Option<T> ────────────────────────────────────────────────────

impl<T: FromConfigValue> FromConfigValue for Option<T> {
    fn from_config(value: ConfigValue) -> Result<Self, String> {
        match value {
            ConfigValue::Unit => Ok(None),
            ConfigValue::String(ref s) if s == "none" || s == "null" => Ok(None),
            other => Ok(Some(T::from_config(other)?)),
        }
    }
}

// ── Tuples (2..6 elements) ───────────────────────────────────────

macro_rules! impl_tuple_from_config {
    ($n:expr; $($T:ident),+; $($idx:expr),+) => {
        impl<$($T: FromConfigValue),+> FromConfigValue for ($($T,)+) {
            fn from_config(value: ConfigValue) -> Result<Self, String> {
                let arr = value.into_array()?;
                if arr.len() != $n {
                    return Err(format!(
                        "expected {}-element tuple, got {} elements", $n, arr.len()
                    ));
                }
                let mut iter = arr.into_iter();
                Ok((
                    $( $T::from_config(iter.next().unwrap()).map_err(|e| format!("tuple[{}]: {}", $idx, e))?, )+
                ))
            }
        }
    };
}

impl_tuple_from_config!(2; A, B; 0, 1);
impl_tuple_from_config!(3; A, B, C; 0, 1, 2);
impl_tuple_from_config!(4; A, B, C, D; 0, 1, 2, 3);
impl_tuple_from_config!(5; A, B, C, D, E; 0, 1, 2, 3, 4);
impl_tuple_from_config!(6; A, B, C, D, E, F; 0, 1, 2, 3, 4, 5);

// ════════════════════════════════════════════════════════════════════
// Type-name dispatch
// ════════════════════════════════════════════════════════════════════

type ConfigParser = fn(ConfigValue) -> Result<Box<dyn Any + Send>, String>;

/// Parse a [`ConfigValue`] into `Box<dyn Any + Send>` according to a Rust type name.
///
/// If `type_name` is `None`, auto-detection is used (TOML string → `String`,
/// float → `f64`, integer → `u64`, bool → `bool`, absent → `()`).
///
/// With an explicit `type_name`, the value is converted to the exact Rust type.
/// Supports all primitive types, common tuples, `Vec<T>`, and `Option<T>`.
pub fn parse_typed_config(
    value: ConfigValue,
    type_name: Option<&str>,
) -> Result<Box<dyn Any + Send>, String> {
    match type_name {
        None => value.auto_convert(),
        Some(name) => {
            if let Some(parser) = lookup_builtin_parser(name) {
                parser(value)
            } else {
                Err(format!(
                    "unsupported config_type '{name}'. \
                     Use a custom ConfigParser or provide config programmatically."
                ))
            }
        }
    }
}

/// Look up a built-in parser by Rust type name.
pub fn lookup_builtin_parser(type_name: &str) -> Option<ConfigParser> {
    // Normalize whitespace: "(usize , usize)" → "(usize,usize)"
    let normalized: String = type_name.chars().filter(|c| !c.is_whitespace()).collect();
    let name = normalized.as_str();

    macro_rules! dispatch {
        ($($pat:expr => $ty:ty),* $(,)?) => {
            match name {
                $($pat => return Some(<$ty>::boxed_from_config),)*
                _ => {}
            }
        };
    }

    // ── Primitives ───────────────────────────────────────────
    dispatch! {
        "()" => (),
        "bool" => bool,
        "u8" => u8,
        "u16" => u16,
        "u32" => u32,
        "u64" => u64,
        "usize" => usize,
        "i8" => i8,
        "i16" => i16,
        "i32" => i32,
        "i64" => i64,
        "isize" => isize,
        "f32" => f32,
        "f64" => f64,
        "String" => String,
    }

    // ── Vec<T> ───────────────────────────────────────────────
    dispatch! {
        "Vec<u8>" => Vec<u8>,
        "Vec<u16>" => Vec<u16>,
        "Vec<u32>" => Vec<u32>,
        "Vec<u64>" => Vec<u64>,
        "Vec<usize>" => Vec<usize>,
        "Vec<i64>" => Vec<i64>,
        "Vec<f32>" => Vec<f32>,
        "Vec<f64>" => Vec<f64>,
        "Vec<String>" => Vec<String>,
        "Vec<bool>" => Vec<bool>,
    }

    // ── 2-tuples ─────────────────────────────────────────────
    dispatch! {
        "(f64,f64)" => (f64, f64),
        "(f32,f32)" => (f32, f32),
        "(f32,usize)" => (f32, usize),
        "(u32,u16)" => (u32, u16),
        "(u32,u32)" => (u32, u32),
        "(usize,usize)" => (usize, usize),
        "(usize,f32)" => (usize, f32),
        "(String,bool)" => (String, bool),
        "(String,usize)" => (String, usize),
        "(String,String)" => (String, String),
    }

    // ── 3-tuples ─────────────────────────────────────────────
    dispatch! {
        "(usize,usize,u8)" => (usize, usize, u8),
        "(usize,f32,f32)" => (usize, f32, f32),
        "(f32,f32,f32)" => (f32, f32, f32),
        "(String,f64,f64)" => (String, f64, f64),
        "(usize,usize,usize)" => (usize, usize, usize),
    }

    // ── 4-tuples ─────────────────────────────────────────────
    dispatch! {
        "(String,f64,f64,f64)" => (String, f64, f64, f64),
        "(usize,bool,bool,Option<f32>)" => (usize, bool, bool, Option<f32>),
        "(f32,f32,f32,f32)" => (f32, f32, f32, f32),
    }

    // ── 5-tuples ─────────────────────────────────────────────
    dispatch! {
        "(f32,f32,f32,f32,f32)" => (f32, f32, f32, f32, f32),
        "(usize,usize,f64,f64,f64)" => (usize, usize, f64, f64, f64),
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_string() {
        let v = ConfigValue::String("hello".into());
        let b = v.auto_convert().unwrap();
        assert_eq!(*b.downcast::<String>().unwrap(), "hello");
    }

    #[test]
    fn auto_float() {
        let v = ConfigValue::Float(3.14);
        let b = v.auto_convert().unwrap();
        assert_eq!(*b.downcast::<f64>().unwrap(), 3.14);
    }

    #[test]
    fn typed_u8() {
        let v = ConfigValue::Integer(42);
        let b = parse_typed_config(v, Some("u8")).unwrap();
        assert_eq!(*b.downcast::<u8>().unwrap(), 42);
    }

    #[test]
    fn typed_tuple_2() {
        let v = ConfigValue::Array(vec![ConfigValue::Integer(3), ConfigValue::Integer(5)]);
        let b = parse_typed_config(v, Some("(usize, usize)")).unwrap();
        assert_eq!(*b.downcast::<(usize, usize)>().unwrap(), (3, 5));
    }

    #[test]
    fn typed_tuple_string_bool() {
        let v = ConfigValue::Array(vec![
            ConfigValue::String("/tmp/data".into()),
            ConfigValue::Bool(true),
        ]);
        let b = parse_typed_config(v, Some("(String, bool)")).unwrap();
        assert_eq!(
            *b.downcast::<(String, bool)>().unwrap(),
            ("/tmp/data".to_string(), true)
        );
    }

    #[test]
    fn typed_vec_u8() {
        let v = ConfigValue::Array(vec![
            ConfigValue::Integer(1),
            ConfigValue::Integer(2),
            ConfigValue::Integer(3),
        ]);
        let b = parse_typed_config(v, Some("Vec<u8>")).unwrap();
        assert_eq!(*b.downcast::<Vec<u8>>().unwrap(), vec![1u8, 2, 3]);
    }

    #[test]
    fn typed_5_tuple() {
        let v = ConfigValue::Array(vec![
            ConfigValue::Float(0.1),
            ConfigValue::Float(0.2),
            ConfigValue::Float(0.3),
            ConfigValue::Float(0.4),
            ConfigValue::Float(0.5),
        ]);
        let b = parse_typed_config(v, Some("(f32,f32,f32,f32,f32)")).unwrap();
        let t = *b.downcast::<(f32, f32, f32, f32, f32)>().unwrap();
        assert!((t.0 - 0.1).abs() < 0.001);
    }

    #[test]
    fn typed_option_some() {
        let v = ConfigValue::Array(vec![
            ConfigValue::Integer(1024),
            ConfigValue::Bool(true),
            ConfigValue::Bool(false),
            ConfigValue::Float(0.5),
        ]);
        let b = parse_typed_config(v, Some("(usize, bool, bool, Option<f32>)")).unwrap();
        let t = *b.downcast::<(usize, bool, bool, Option<f32>)>().unwrap();
        assert_eq!(t.0, 1024);
        assert!(t.3.is_some());
    }

    #[test]
    fn typed_option_none() {
        let v = ConfigValue::Array(vec![
            ConfigValue::Integer(1024),
            ConfigValue::Bool(true),
            ConfigValue::Bool(false),
            ConfigValue::String("none".into()),
        ]);
        let b = parse_typed_config(v, Some("(usize, bool, bool, Option<f32>)")).unwrap();
        let t = *b.downcast::<(usize, bool, bool, Option<f32>)>().unwrap();
        assert!(t.3.is_none());
    }

    #[test]
    fn unsupported_type_gives_error() {
        let v = ConfigValue::Integer(42);
        assert!(parse_typed_config(v, Some("MyCustomType")).is_err());
    }
}
