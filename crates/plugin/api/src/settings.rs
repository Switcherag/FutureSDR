use std::collections::HashMap;

use anyhow::anyhow;
use futuresdr::num_complex::Complex32;
use futuresdr::num_complex::Complex64;
use futuresdr::runtime::Pmt;

/// The settings of one block instance, as written in a flowgraph description.
///
/// Values are [`Pmt`]s: integers arrive as `Pmt::Isize`, floats as
/// `Pmt::F64`, lists as `Pmt::VecPmt` and tables as `Pmt::MapStrPmt`. The
/// typed getters convert and range-check them.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    block: String,
    values: HashMap<String, Pmt>,
}

impl Settings {
    /// Settings of block `block` (used in error messages).
    pub fn new(block: impl Into<String>, values: HashMap<String, Pmt>) -> Self {
        Self {
            block: block.into(),
            values,
        }
    }

    /// Name of the block these settings belong to.
    pub fn block(&self) -> &str {
        &self.block
    }

    /// Setting `key`, which must be present.
    pub fn get<T: FromSetting>(&self, key: &str) -> anyhow::Result<T> {
        self.get_opt(key)?
            .ok_or_else(|| anyhow!("block '{}': missing setting '{key}' ({})", self.block, T::EXPECTED))
    }

    /// Setting `key`, or `default` when it is absent.
    pub fn get_or<T: FromSetting>(&self, key: &str, default: T) -> anyhow::Result<T> {
        Ok(self.get_opt(key)?.unwrap_or(default))
    }

    /// Setting `key`, if present.
    pub fn get_opt<T: FromSetting>(&self, key: &str) -> anyhow::Result<Option<T>> {
        match self.values.get(key) {
            None => Ok(None),
            Some(value) => T::from_setting(value).map(Some).ok_or_else(|| {
                anyhow!(
                    "block '{}': setting '{key}' should be {}, got {value:?}",
                    self.block,
                    T::EXPECTED
                )
            }),
        }
    }

    /// The raw value of `key`.
    pub fn raw(&self, key: &str) -> Option<&Pmt> {
        self.values.get(key)
    }

    /// Setting names.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }
}

/// A type a setting can be read as.
pub trait FromSetting: Sized {
    /// What is expected, for error messages.
    const EXPECTED: &'static str;
    /// Convert, or `None` if `value` does not fit.
    fn from_setting(value: &Pmt) -> Option<Self>;
}

fn as_i128(value: &Pmt) -> Option<i128> {
    Some(match value {
        Pmt::Isize(v) => *v as i128,
        Pmt::Usize(v) => *v as i128,
        Pmt::U32(v) => *v as i128,
        Pmt::U64(v) => *v as i128,
        _ => return None,
    })
}

macro_rules! int_setting {
    ($($t:ty),*) => {$(
        impl FromSetting for $t {
            const EXPECTED: &'static str = concat!("an integer that fits ", stringify!($t));
            fn from_setting(value: &Pmt) -> Option<Self> {
                as_i128(value).and_then(|v| <$t>::try_from(v).ok())
            }
        }
    )*};
}
int_setting!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

impl FromSetting for f64 {
    const EXPECTED: &'static str = "a number";
    fn from_setting(value: &Pmt) -> Option<Self> {
        match value {
            Pmt::F64(v) => Some(*v),
            Pmt::F32(v) => Some(*v as f64),
            other => as_i128(other).map(|v| v as f64),
        }
    }
}

impl FromSetting for f32 {
    const EXPECTED: &'static str = "a number";
    fn from_setting(value: &Pmt) -> Option<Self> {
        f64::from_setting(value).map(|v| v as f32)
    }
}

impl FromSetting for bool {
    const EXPECTED: &'static str = "true or false";
    fn from_setting(value: &Pmt) -> Option<Self> {
        match value {
            Pmt::Bool(v) => Some(*v),
            _ => None,
        }
    }
}

impl FromSetting for String {
    const EXPECTED: &'static str = "a string";
    fn from_setting(value: &Pmt) -> Option<Self> {
        match value {
            Pmt::String(v) => Some(v.clone()),
            _ => None,
        }
    }
}

impl FromSetting for Complex64 {
    const EXPECTED: &'static str = "a number or [re, im]";
    fn from_setting(value: &Pmt) -> Option<Self> {
        if let Some(re) = f64::from_setting(value) {
            return Some(Complex64::new(re, 0.0));
        }
        match <Vec<f64>>::from_setting(value)?.as_slice() {
            [re, im] => Some(Complex64::new(*re, *im)),
            _ => None,
        }
    }
}

impl FromSetting for Complex32 {
    const EXPECTED: &'static str = "a number or [re, im]";
    fn from_setting(value: &Pmt) -> Option<Self> {
        Complex64::from_setting(value).map(|c| Complex32::new(c.re as f32, c.im as f32))
    }
}

impl<T: FromSetting> FromSetting for Vec<T> {
    const EXPECTED: &'static str = "a list";
    fn from_setting(value: &Pmt) -> Option<Self> {
        match value {
            Pmt::VecPmt(items) => items.iter().map(T::from_setting).collect(),
            Pmt::VecF32(items) => items.iter().map(|v| T::from_setting(&Pmt::F32(*v))).collect(),
            Pmt::VecU64(items) => items.iter().map(|v| T::from_setting(&Pmt::U64(*v))).collect(),
            Pmt::Blob(items) => items
                .iter()
                .map(|v| T::from_setting(&Pmt::Usize(*v as usize)))
                .collect(),
            _ => None,
        }
    }
}

impl FromSetting for Pmt {
    const EXPECTED: &'static str = "any value";
    fn from_setting(value: &Pmt) -> Option<Self> {
        Some(value.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, Pmt)]) -> Settings {
        Settings::new(
            "blk",
            pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        )
    }

    #[test]
    fn integers_are_range_checked() {
        let s = settings(&[("n", Pmt::Isize(300)), ("neg", Pmt::Isize(-1))]);
        assert_eq!(s.get::<u16>("n").unwrap(), 300);
        assert!(s.get::<u8>("n").is_err());
        assert!(s.get::<u64>("neg").is_err());
        assert_eq!(s.get::<i8>("neg").unwrap(), -1);
    }

    #[test]
    fn missing_and_defaults() {
        let s = settings(&[]);
        let err = s.get::<u64>("n_items").unwrap_err().to_string();
        assert!(err.contains("blk") && err.contains("n_items"), "{err}");
        assert_eq!(s.get_or("n_items", 7u64).unwrap(), 7);
        assert_eq!(s.get_opt::<f32>("x").unwrap(), None);
    }

    #[test]
    fn lists_numbers_and_complex() {
        let s = settings(&[
            ("xs", Pmt::VecPmt(vec![Pmt::Isize(1), Pmt::F64(2.5)])),
            ("c", Pmt::VecPmt(vec![Pmt::F64(1.0), Pmt::F64(-2.0)])),
            ("r", Pmt::Isize(3)),
            ("bad", Pmt::String("x".into())),
        ]);
        assert_eq!(s.get::<Vec<f32>>("xs").unwrap(), vec![1.0, 2.5]);
        assert!(s.get::<Vec<u8>>("xs").is_err());
        assert_eq!(s.get::<Complex32>("c").unwrap(), Complex32::new(1.0, -2.0));
        assert_eq!(s.get::<Complex32>("r").unwrap(), Complex32::new(3.0, 0.0));
        assert!(s.get::<f64>("bad").is_err());
    }
}
