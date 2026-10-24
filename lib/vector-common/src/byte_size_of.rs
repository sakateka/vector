use std::{
    collections::{BTreeMap, BTreeSet},
    mem,
};

use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use serde_json::{Value as JsonValue, value::RawValue};
use smallvec::SmallVec;
use vrl::value::{KeyString, ObjectMap, Value};

pub trait ByteSizeOf {
    /// Returns the in-memory size of this type
    ///
    /// This function returns the total number of bytes that
    /// [`std::mem::size_of`] does in addition to any interior allocated
    /// bytes. Its default implementation is `std::mem::size_of` +
    /// `ByteSizeOf::allocated_bytes`.
    fn size_of(&self) -> usize {
        mem::size_of_val(self) + self.allocated_bytes()
    }

    /// Returns the allocated bytes of this type
    ///
    /// This function returns the total number of bytes that have been allocated
    /// interior to this type instance. It does not include any bytes that are
    /// captured by [`std::mem::size_of`] except for any bytes that are interior
    /// to this type. For instance, `BTreeMap<String, Vec<u8>>` would count all
    /// bytes for `String` and `Vec<u8>` instances but not the exterior bytes
    /// for `BTreeMap`.
    fn allocated_bytes(&self) -> usize;
}

impl<T> ByteSizeOf for &T
where
    T: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        (*self).size_of()
    }
}

impl ByteSizeOf for Bytes {
    fn allocated_bytes(&self) -> usize {
        self.len()
    }
}

impl ByteSizeOf for BytesMut {
    fn allocated_bytes(&self) -> usize {
        self.len()
    }
}

impl ByteSizeOf for String {
    fn allocated_bytes(&self) -> usize {
        self.capacity()
    }
}

impl ByteSizeOf for KeyString {
    fn allocated_bytes(&self) -> usize {
        self.capacity()
    }
}

impl ByteSizeOf for &str {
    fn allocated_bytes(&self) -> usize {
        0
    }
}

impl ByteSizeOf for ObjectMap {
    fn allocated_bytes(&self) -> usize {
        self.iter()
            .fold(self.node_storage_bytes(), |bytes, (key, value)| {
                bytes
                    .saturating_add(key.allocated_bytes())
                    .saturating_add(value.allocated_bytes())
            })
    }
}

impl<K, V> ByteSizeOf for BTreeMap<K, V>
where
    K: ByteSizeOf,
    V: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.iter()
            .fold(0, |acc, (k, v)| acc + k.size_of() + v.size_of())
    }
}

impl<T> ByteSizeOf for BTreeSet<T>
where
    T: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.iter().map(ByteSizeOf::size_of).sum()
    }
}

impl<T> ByteSizeOf for Vec<T>
where
    T: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.iter()
            .fold(self.capacity() * mem::size_of::<T>(), |bytes, item| {
                bytes.saturating_add(item.allocated_bytes())
            })
    }
}

impl<A: smallvec::Array> ByteSizeOf for SmallVec<A>
where
    A::Item: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        let backing = if self.spilled() {
            self.capacity() * mem::size_of::<A::Item>()
        } else {
            0
        };
        self.iter().fold(backing, |bytes, item| {
            bytes.saturating_add(item.allocated_bytes())
        })
    }
}

impl<T> ByteSizeOf for &[T]
where
    T: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.iter().map(ByteSizeOf::size_of).sum()
    }
}

impl<T, const N: usize> ByteSizeOf for [T; N]
where
    T: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.iter().fold(0usize, |bytes, item| {
            bytes.saturating_add(item.allocated_bytes())
        })
    }
}

impl<T> ByteSizeOf for Option<T>
where
    T: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.as_ref().map_or(0, ByteSizeOf::allocated_bytes)
    }
}

macro_rules! num {
    ($t:ty) => {
        impl ByteSizeOf for $t {
            fn allocated_bytes(&self) -> usize {
                0
            }
        }
    };
}

num!(u8);
num!(u16);
num!(u32);
num!(u64);
num!(u128);
num!(i8);
num!(i16);
num!(i32);
num!(i64);
num!(i128);
num!(f32);
num!(f64);

impl ByteSizeOf for Box<RawValue> {
    fn allocated_bytes(&self) -> usize {
        self.get().len()
    }
}

impl ByteSizeOf for JsonValue {
    fn allocated_bytes(&self) -> usize {
        match self {
            JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => 0,
            JsonValue::String(s) => s.allocated_bytes(),
            JsonValue::Array(a) => a.allocated_bytes(),
            JsonValue::Object(o) => o.iter().map(|(k, v)| k.size_of() + v.size_of()).sum(),
        }
    }
}

impl ByteSizeOf for Value {
    fn allocated_bytes(&self) -> usize {
        match self {
            Value::Bytes(bytes) => bytes.len(),
            Value::String(string) => string.len(),
            Value::Object(map) => map.allocated_bytes(),
            Value::Array(arr) => arr.allocated_bytes(),
            _ => 0,
        }
    }
}

impl ByteSizeOf for DateTime<Utc> {
    fn allocated_bytes(&self) -> usize {
        0
    }
}

impl<K, V> ByteSizeOf for indexmap::IndexMap<K, V>
where
    K: ByteSizeOf,
    V: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.iter()
            .fold(0, |acc, (k, v)| acc + k.size_of() + v.size_of())
    }
}

impl<T> ByteSizeOf for indexmap::IndexSet<T>
where
    T: ByteSizeOf,
{
    fn allocated_bytes(&self) -> usize {
        self.iter().map(ByteSizeOf::size_of).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleared_containers_keep_their_allocated_storage() {
        let mut values = Vec::<u64>::with_capacity(256);
        let mut string = String::with_capacity(4096);
        values.push(1);
        string.push_str("retained");
        let bytes = values.allocated_bytes() + string.allocated_bytes();
        values.clear();
        string.clear();
        assert_eq!(values.allocated_bytes() + string.allocated_bytes(), bytes);
        assert!(bytes >= 256 * 8 + 4096);
    }

    #[test]
    fn nested_storage_does_not_charge_inline_headers_twice() {
        let mut string = String::with_capacity(4096);
        string.push('x');
        let mut values = Vec::with_capacity(8);
        values.push(string);
        assert_eq!(
            values.allocated_bytes(),
            8 * mem::size_of::<String>() + 4096
        );

        let fixed = vec![[1u64; 4]];
        assert_eq!(fixed.allocated_bytes(), 4 * mem::size_of::<u64>());
        assert_eq!([1u64; 4].allocated_bytes(), 0);

        let array = Value::Array(vec![Value::Integer(1)]);
        assert_eq!(array.allocated_bytes(), mem::size_of::<Value>());
    }

    #[test]
    fn smallvec_counts_inline_elements_only_after_spilling() {
        let mut values = SmallVec::<[u64; 4]>::new();
        values.push(1);
        assert_eq!(values.allocated_bytes(), 0);
        values.extend(2..=5);
        assert!(values.allocated_bytes() >= 5 * 8);
        let allocated = values.allocated_bytes();
        values.clear();
        assert_eq!(values.allocated_bytes(), allocated);
    }

    #[test]
    fn zero_sized_capacity_and_repeated_references_do_not_overflow() {
        struct Empty;
        impl ByteSizeOf for Empty {
            fn allocated_bytes(&self) -> usize {
                0
            }
        }
        let values = Vec::<Empty>::with_capacity(10);
        assert_eq!(values.allocated_bytes(), 0);

        struct Shared;
        impl ByteSizeOf for Shared {
            fn allocated_bytes(&self) -> usize {
                usize::MAX
            }
        }
        assert_eq!(vec![Shared, Shared].allocated_bytes(), usize::MAX);
    }

    #[test]
    fn string_values_count_their_utf8_bytes() {
        let value = Value::from("café");
        assert_eq!(value.allocated_bytes(), 5);
        assert_eq!(value.size_of(), Value::Bytes("café".into()).size_of());
    }
}
