use std::fmt;
use std::str::FromStr;

/// Item types a flowgraph port can carry between flowgraphs.
///
/// The same list generic plugin blocks are exported for by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemType {
    /// `u8`
    U8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `f32`
    F32,
    /// `f64`
    F64,
    /// `Complex32` (also written `c32`)
    Complex32,
}

impl ItemType {
    /// Every item type.
    pub const ALL: [ItemType; 6] = [
        ItemType::U8,
        ItemType::I16,
        ItemType::I32,
        ItemType::F32,
        ItemType::F64,
        ItemType::Complex32,
    ];

    /// Name as used in block type names, e.g. `f32` in `Head<f32>`.
    pub fn name(self) -> &'static str {
        match self {
            ItemType::U8 => "u8",
            ItemType::I16 => "i16",
            ItemType::I32 => "i32",
            ItemType::F32 => "f32",
            ItemType::F64 => "f64",
            ItemType::Complex32 => "Complex32",
        }
    }

    /// The item type of a generic block type name with one parameter, like
    /// `Head<f32>`.
    pub fn of_block_type(type_name: &str) -> Option<Self> {
        let param = type_name.strip_suffix('>')?.split_once('<')?.1;
        param.parse().ok()
    }
}

impl fmt::Display for ItemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ItemType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "c32" => Ok(ItemType::Complex32),
            other => ItemType::ALL
                .into_iter()
                .find(|t| t.name() == other)
                .ok_or_else(|| {
                    format!(
                        "unknown item type '{other}' (known: {})",
                        ItemType::ALL.map(ItemType::name).join(", ")
                    )
                }),
        }
    }
}

/// Run `$body` with `$T` bound to the Rust type of an [`ItemType`].
macro_rules! with_item_type {
    ($item:expr, $T:ident => $body:expr) => {
        match $item {
            $crate::items::ItemType::U8 => {
                type $T = u8;
                $body
            }
            $crate::items::ItemType::I16 => {
                type $T = i16;
                $body
            }
            $crate::items::ItemType::I32 => {
                type $T = i32;
                $body
            }
            $crate::items::ItemType::F32 => {
                type $T = f32;
                $body
            }
            $crate::items::ItemType::F64 => {
                type $T = f64;
                $body
            }
            $crate::items::ItemType::Complex32 => {
                type $T = futuresdr::num_complex::Complex32;
                $body
            }
        }
    };
}
pub(crate) use with_item_type;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for t in ItemType::ALL {
            assert_eq!(t.name().parse::<ItemType>().unwrap(), t);
        }
        assert_eq!("c32".parse::<ItemType>().unwrap(), ItemType::Complex32);
        assert!("u16".parse::<ItemType>().is_err());
    }

    #[test]
    fn item_type_of_block_type() {
        assert_eq!(ItemType::of_block_type("Head<f32>"), Some(ItemType::F32));
        assert_eq!(
            ItemType::of_block_type("Copy<Complex32>"),
            Some(ItemType::Complex32)
        );
        assert_eq!(ItemType::of_block_type("MessageCopy"), None);
        assert_eq!(ItemType::of_block_type("Map<u8,f32>"), None);
    }
}
