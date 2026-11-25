//! Conversion of libmpv `mpv_node` trees into an owned Rust [`Value`].

use std::ffi::{CStr, c_char};

use mpv_sys::*;

/// An owned copy of an `mpv_node`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    None,
    Str(String),
    Flag(bool),
    Int(i64),
    Double(f64),
    Array(Vec<Value>),
    Map(Vec<(String, Value)>),
    /// Raw bytes (e.g. the pixels from `screenshot-raw`).
    Bytes(Vec<u8>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Double(d) => Some(*d as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Double(d) => Some(*d),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Flag(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> &[Value] {
        match self {
            Value::Array(items) => items,
            _ => &[],
        }
    }

    pub fn str_field(&self, key: &str) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    pub fn bool_field(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
}

pub(crate) unsafe fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Copy an `mpv_node` (and everything it points to) into a [`Value`].
///
/// # Safety
/// `node` must be a valid node as produced by libmpv.
pub(crate) unsafe fn from_node(node: &mpv_node) -> Value {
    unsafe {
        match node.format {
            MPV_FORMAT_STRING | MPV_FORMAT_OSD_STRING => Value::Str(cstr_to_string(node.u.string)),
            MPV_FORMAT_FLAG => Value::Flag(node.u.flag != 0),
            MPV_FORMAT_INT64 => Value::Int(node.u.int64),
            MPV_FORMAT_DOUBLE => Value::Double(node.u.double_),
            MPV_FORMAT_BYTE_ARRAY => {
                let ba = node.u.ba;
                if ba.is_null() || (*ba).data.is_null() {
                    return Value::Bytes(Vec::new());
                }
                Value::Bytes(std::slice::from_raw_parts((*ba).data as *const u8, (*ba).size).to_vec())
            }
            MPV_FORMAT_NODE_ARRAY | MPV_FORMAT_NODE_MAP => {
                let list = node.u.list;
                if list.is_null() {
                    return Value::None;
                }
                let list = &*list;
                let n = list.num.max(0) as usize;
                let values = if n == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(list.values, n)
                };
                if node.format == MPV_FORMAT_NODE_ARRAY {
                    Value::Array(values.iter().map(|v| from_node(v)).collect())
                } else {
                    let keys = if n == 0 {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts(list.keys, n)
                    };
                    Value::Map(
                        keys.iter()
                            .zip(values)
                            .map(|(k, v)| (cstr_to_string(*k), from_node(v)))
                            .collect(),
                    )
                }
            }
            _ => Value::None,
        }
    }
}
