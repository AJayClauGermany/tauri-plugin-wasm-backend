//! Conversions between JSON (what Tauri IPC speaks) and component model
//! values, plus type rendering in WIT and TypeScript syntax.
//!
//! The JSON shape follows the conventions of `jco`, the JavaScript component
//! toolchain, so a value looks the same to the frontend as it does to a JS
//! guest:
//!
//! | WIT                | JSON                                         |
//! |--------------------|----------------------------------------------|
//! | `record`           | object with camelCase keys                   |
//! | `variant`          | `{ "tag": "case-name", "val": ... }`         |
//! | `enum`             | `"case-name"`                                |
//! | `option<T>`        | `null` or `T`                                |
//! | `result<T, E>`     | `{ "tag": "ok" \| "err", "val": ... }`       |
//! | `flags`            | array of set flag names                      |
//! | `tuple`, `list`    | arrays                                       |
//! | `map<string, V>`   | object                                       |
//!
//! Input is lenient: record keys may be kebab-, snake- or camelCase,
//! variants also accept the serde form `{ "case": val }` or `"case"`, and
//! flags also accept `{ "flag": true }`.

use serde_json::{Map, Number, Value};
use wasmtime::component::{Type, Val};

use crate::engine::naming::{candidates, to_camel, to_kebab};

pub(crate) fn json_to_val(value: &Value, ty: &Type) -> Result<Val, String> {
    fn int<T: TryFrom<i64> + TryFrom<u64>>(v: &Value, name: &str) -> Result<T, String> {
        if let Some(i) = v.as_i64() {
            T::try_from(i).map_err(|_| format!("{i} is out of range for {name}"))
        } else if let Some(u) = v.as_u64() {
            T::try_from(u).map_err(|_| format!("{u} is out of range for {name}"))
        } else {
            Err(format!("expected an integer ({name}), got {v}"))
        }
    }
    fn float(v: &Value) -> Result<f64, String> {
        v.as_f64()
            .ok_or_else(|| format!("expected a number, got {v}"))
    }

    Ok(match ty {
        Type::Bool => Val::Bool(
            value
                .as_bool()
                .ok_or_else(|| format!("expected a boolean, got {value}"))?,
        ),
        Type::S8 => Val::S8(int(value, "s8")?),
        Type::U8 => Val::U8(int(value, "u8")?),
        Type::S16 => Val::S16(int(value, "s16")?),
        Type::U16 => Val::U16(int(value, "u16")?),
        Type::S32 => Val::S32(int(value, "s32")?),
        Type::U32 => Val::U32(int(value, "u32")?),
        Type::S64 => Val::S64(int(value, "s64")?),
        Type::U64 => Val::U64(int(value, "u64")?),
        Type::Float32 => Val::Float32(float(value)? as f32),
        Type::Float64 => Val::Float64(float(value)?),
        Type::Char => {
            let s = value
                .as_str()
                .ok_or_else(|| format!("expected a char, got {value}"))?;
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Val::Char(c),
                _ => return Err(format!("expected a single character, got {value}")),
            }
        }
        Type::String => Val::String(
            value
                .as_str()
                .ok_or_else(|| format!("expected a string, got {value}"))?
                .to_string(),
        ),
        Type::List(list) => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("expected an array, got {value}"))?;
            let elem = list.ty();
            Val::List(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| json_to_val(v, &elem).map_err(|e| format!("[{i}]: {e}")))
                    .collect::<Result<_, _>>()?,
            )
        }
        Type::FixedLengthList(list) => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("expected an array, got {value}"))?;
            if items.len() != list.len() as usize {
                return Err(format!(
                    "expected an array of length {}, got {}",
                    list.len(),
                    items.len()
                ));
            }
            let elem = list.ty();
            Val::FixedLengthList(
                items
                    .iter()
                    .map(|v| json_to_val(v, &elem))
                    .collect::<Result<_, _>>()?,
            )
        }
        Type::Map(map) => {
            let (kt, vt) = (map.key(), map.value());
            let pairs = match value {
                Value::Object(obj) => obj
                    .iter()
                    .map(|(k, v)| {
                        Ok((
                            json_to_val(&Value::String(k.clone()), &kt)?,
                            json_to_val(v, &vt).map_err(|e| format!(".{k}: {e}"))?,
                        ))
                    })
                    .collect::<Result<_, String>>()?,
                Value::Array(items) => items
                    .iter()
                    .map(|pair| match pair.as_array().map(Vec::as_slice) {
                        Some([k, v]) => Ok((json_to_val(k, &kt)?, json_to_val(v, &vt)?)),
                        _ => Err(format!("expected a [key, value] pair, got {pair}")),
                    })
                    .collect::<Result<_, String>>()?,
                _ => return Err(format!("expected an object, got {value}")),
            };
            Val::Map(pairs)
        }
        Type::Record(record) => {
            let obj = value
                .as_object()
                .ok_or_else(|| format!("expected an object, got {value}"))?;
            let mut fields = Vec::new();
            for field in record.fields() {
                let v = lookup(obj, field.name).unwrap_or(&Value::Null);
                if v.is_null() && !matches!(field.ty, Type::Option(_)) {
                    return Err(format!("missing field `{}`", to_camel(field.name)));
                }
                fields.push((
                    field.name.to_string(),
                    json_to_val(v, &field.ty)
                        .map_err(|e| format!(".{}: {e}", to_camel(field.name)))?,
                ));
            }
            Val::Record(fields)
        }
        Type::Tuple(tuple) => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("expected an array, got {value}"))?;
            let types: Vec<Type> = tuple.types().collect();
            if items.len() != types.len() {
                return Err(format!(
                    "expected a tuple of {} items, got {}",
                    types.len(),
                    items.len()
                ));
            }
            Val::Tuple(
                items
                    .iter()
                    .zip(&types)
                    .map(|(v, t)| json_to_val(v, t))
                    .collect::<Result<_, _>>()?,
            )
        }
        Type::Variant(variant) => {
            let (tag, payload) = tagged(value)?;
            let case = variant
                .cases()
                .find(|c| to_kebab(c.name) == to_kebab(&tag))
                .ok_or_else(|| format!("unknown variant case `{tag}`"))?;
            let payload = match (case.ty, payload) {
                (Some(t), Some(v)) => Some(Box::new(json_to_val(v, &t)?)),
                (Some(t), None) => Some(Box::new(json_to_val(&Value::Null, &t)?)),
                (None, _) => None,
            };
            Val::Variant(case.name.to_string(), payload)
        }
        Type::Enum(en) => {
            let s = value
                .as_str()
                .ok_or_else(|| format!("expected a string, got {value}"))?;
            let name = en
                .names()
                .find(|n| to_kebab(n) == to_kebab(s))
                .ok_or_else(|| format!("unknown enum case `{s}`"))?;
            Val::Enum(name.to_string())
        }
        Type::Option(opt) => match value {
            Value::Null => Val::Option(None),
            v => Val::Option(Some(Box::new(json_to_val(v, &opt.ty())?))),
        },
        Type::Result(result) => {
            let (tag, payload) = tagged(value)?;
            let payload = payload.unwrap_or(&Value::Null);
            match tag.as_str() {
                "ok" | "Ok" => Val::Result(Ok(match result.ok() {
                    Some(t) => Some(Box::new(json_to_val(payload, &t)?)),
                    None => None,
                })),
                "err" | "Err" => Val::Result(Err(match result.err() {
                    Some(t) => Some(Box::new(json_to_val(payload, &t)?)),
                    None => None,
                })),
                other => return Err(format!("expected tag `ok` or `err`, got `{other}`")),
            }
        }
        Type::Flags(flags) => {
            let set: Vec<String> = match value {
                Value::Array(items) => items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| format!("expected a flag name, got {v}"))
                    })
                    .collect::<Result<_, _>>()?,
                Value::Object(obj) => obj
                    .iter()
                    .filter(|(_, v)| v.as_bool() == Some(true))
                    .map(|(k, _)| k.clone())
                    .collect(),
                _ => return Err(format!("expected an array of flags, got {value}")),
            };
            let mut out = Vec::new();
            for s in set {
                let name = flags
                    .names()
                    .find(|n| to_kebab(n) == to_kebab(&s))
                    .ok_or_else(|| format!("unknown flag `{s}`"))?;
                out.push(name.to_string());
            }
            Val::Flags(out)
        }
        Type::Own(_) | Type::Borrow(_) => {
            return Err("resources can't be passed through JSON".into());
        }
        Type::Future(_) | Type::Stream(_) | Type::ErrorContext => {
            return Err("async types can't be passed through JSON".into());
        }
    })
}

/// Look up `name` (a WIT kebab-case name) in a JSON object, accepting any
/// casing convention.
pub(crate) fn lookup<'a>(obj: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    candidates(name).iter().find_map(|k| obj.get(k))
}

/// Accept `{ tag, val }`, `{ "case": val }` and `"case"`.
fn tagged(value: &Value) -> Result<(String, Option<&Value>), String> {
    match value {
        Value::String(s) => Ok((s.clone(), None)),
        Value::Object(obj) => {
            if let Some(Value::String(tag)) = obj.get("tag") {
                Ok((tag.clone(), obj.get("val")))
            } else if obj.len() == 1 {
                let (k, v) = obj.iter().next().unwrap();
                Ok((k.clone(), Some(v)))
            } else {
                Err(format!(
                    "expected `{{ \"tag\": ..., \"val\": ... }}`, got {value}"
                ))
            }
        }
        _ => Err(format!("expected a tagged value, got {value}")),
    }
}

pub(crate) fn val_to_json(val: Val) -> Value {
    fn tag(tag: &str, val: Option<Box<Val>>) -> Value {
        let mut obj = Map::new();
        obj.insert("tag".into(), Value::String(tag.to_string()));
        if let Some(v) = val {
            obj.insert("val".into(), val_to_json(*v));
        }
        Value::Object(obj)
    }
    fn float(f: f64) -> Value {
        Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }

    match val {
        Val::Bool(b) => Value::Bool(b),
        Val::S8(v) => v.into(),
        Val::U8(v) => v.into(),
        Val::S16(v) => v.into(),
        Val::U16(v) => v.into(),
        Val::S32(v) => v.into(),
        Val::U32(v) => v.into(),
        Val::S64(v) => v.into(),
        Val::U64(v) => v.into(),
        Val::Float32(v) => float(v as f64),
        Val::Float64(v) => float(v),
        Val::Char(c) => Value::String(c.to_string()),
        Val::String(s) => Value::String(s),
        Val::List(items) | Val::Tuple(items) | Val::FixedLengthList(items) => {
            Value::Array(items.into_iter().map(val_to_json).collect())
        }
        Val::Map(pairs) => {
            if pairs.iter().all(|(k, _)| matches!(k, Val::String(_))) {
                Value::Object(
                    pairs
                        .into_iter()
                        .map(|(k, v)| match k {
                            Val::String(k) => (k, val_to_json(v)),
                            _ => unreachable!(),
                        })
                        .collect(),
                )
            } else {
                Value::Array(
                    pairs
                        .into_iter()
                        .map(|(k, v)| Value::Array(vec![val_to_json(k), val_to_json(v)]))
                        .collect(),
                )
            }
        }
        Val::Record(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(k, v)| (to_camel(&k), val_to_json(v)))
                .collect(),
        ),
        Val::Variant(case, payload) => tag(&case, payload),
        Val::Enum(case) => Value::String(case),
        Val::Option(v) => v.map(|v| val_to_json(*v)).unwrap_or(Value::Null),
        Val::Result(Ok(v)) => tag("ok", v),
        Val::Result(Err(v)) => tag("err", v),
        Val::Flags(flags) => Value::Array(flags.into_iter().map(Value::String).collect()),
        Val::Resource(_) => Value::String("<resource>".into()),
        Val::Future(_) | Val::Stream(_) | Val::ErrorContext(_) => Value::Null,
    }
}

/// Render a type in WIT syntax (structural: named types are inlined).
pub(crate) fn wit_type(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::S8 => "s8".into(),
        Type::U8 => "u8".into(),
        Type::S16 => "s16".into(),
        Type::U16 => "u16".into(),
        Type::S32 => "s32".into(),
        Type::U32 => "u32".into(),
        Type::S64 => "s64".into(),
        Type::U64 => "u64".into(),
        Type::Float32 => "f32".into(),
        Type::Float64 => "f64".into(),
        Type::Char => "char".into(),
        Type::String => "string".into(),
        Type::List(l) => format!("list<{}>", wit_type(&l.ty())),
        Type::FixedLengthList(l) => format!("list<{}, {}>", wit_type(&l.ty()), l.len()),
        Type::Map(m) => format!("map<{}, {}>", wit_type(&m.key()), wit_type(&m.value())),
        Type::Record(r) => format!(
            "record {{ {} }}",
            r.fields()
                .map(|f| format!("{}: {}", f.name, wit_type(&f.ty)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Tuple(t) => format!(
            "tuple<{}>",
            t.types()
                .map(|t| wit_type(&t))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Variant(v) => format!(
            "variant {{ {} }}",
            v.cases()
                .map(|c| match c.ty {
                    Some(t) => format!("{}({})", c.name, wit_type(&t)),
                    None => c.name.to_string(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Enum(e) => format!("enum {{ {} }}", e.names().collect::<Vec<_>>().join(", ")),
        Type::Option(o) => format!("option<{}>", wit_type(&o.ty())),
        Type::Result(r) => match (r.ok(), r.err()) {
            (None, None) => "result".into(),
            (Some(ok), None) => format!("result<{}>", wit_type(&ok)),
            (None, Some(err)) => format!("result<_, {}>", wit_type(&err)),
            (Some(ok), Some(err)) => format!("result<{}, {}>", wit_type(&ok), wit_type(&err)),
        },
        Type::Flags(f) => format!("flags {{ {} }}", f.names().collect::<Vec<_>>().join(", ")),
        Type::Own(_) => "own<resource>".into(),
        Type::Borrow(_) => "borrow<resource>".into(),
        Type::Future(_) => "future".into(),
        Type::Stream(_) => "stream".into(),
        Type::ErrorContext => "error-context".into(),
    }
}

/// Render the TypeScript type of the JSON representation of `ty`.
pub(crate) fn ts_type(ty: &Type) -> String {
    fn quote(s: &str) -> String {
        format!("'{s}'")
    }
    match ty {
        Type::Bool => "boolean".into(),
        Type::S8
        | Type::U8
        | Type::S16
        | Type::U16
        | Type::S32
        | Type::U32
        | Type::S64
        | Type::U64
        | Type::Float32
        | Type::Float64 => "number".into(),
        Type::Char | Type::String => "string".into(),
        Type::List(l) => format!("Array<{}>", ts_type(&l.ty())),
        Type::FixedLengthList(l) => format!("Array<{}>", ts_type(&l.ty())),
        Type::Map(m) => match m.key() {
            Type::String => format!("Record<string, {}>", ts_type(&m.value())),
            k => format!("Array<[{}, {}]>", ts_type(&k), ts_type(&m.value())),
        },
        Type::Record(r) => format!(
            "{{ {} }}",
            r.fields()
                .map(|f| {
                    let opt = if matches!(f.ty, Type::Option(_)) {
                        "?"
                    } else {
                        ""
                    };
                    format!("{}{opt}: {}", to_camel(f.name), ts_type(&f.ty))
                })
                .collect::<Vec<_>>()
                .join("; ")
        ),
        Type::Tuple(t) => format!(
            "[{}]",
            t.types()
                .map(|t| ts_type(&t))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::Variant(v) => v
            .cases()
            .map(|c| match c.ty {
                Some(t) => format!("{{ tag: {}; val: {} }}", quote(c.name), ts_type(&t)),
                None => format!("{{ tag: {} }}", quote(c.name)),
            })
            .collect::<Vec<_>>()
            .join(" | "),
        Type::Enum(e) => e.names().map(quote).collect::<Vec<_>>().join(" | "),
        Type::Option(o) => format!("{} | null", ts_type(&o.ty())),
        Type::Result(r) => {
            let ok = r
                .ok()
                .map(|t| format!("; val: {}", ts_type(&t)))
                .unwrap_or_default();
            let err = r
                .err()
                .map(|t| format!("; val: {}", ts_type(&t)))
                .unwrap_or_default();
            format!("{{ tag: 'ok'{ok} }} | {{ tag: 'err'{err} }}")
        }
        Type::Flags(f) => format!(
            "Array<{}>",
            f.names().map(quote).collect::<Vec<_>>().join(" | ")
        ),
        _ => "unknown".into(),
    }
}
