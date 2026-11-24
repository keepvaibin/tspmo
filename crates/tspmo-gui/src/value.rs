//! A plain, `Send` tree of D-Bus values, so the rest of the program (and its tests) never touches
//! zvariant.

use zbus::zvariant::Value;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum V {
    #[default]
    None,
    B(bool),
    I(i64),
    F(f64),
    S(String),
    /// Arrays and structs (`a(sy)`, `(yyy)`): the element type is not needed by the UI.
    L(Vec<V>),
}

impl V {
    pub fn from_value(v: &Value<'_>) -> V {
        match v {
            Value::U8(n) => V::I(i64::from(*n)),
            Value::Bool(b) => V::B(*b),
            Value::I16(n) => V::I(i64::from(*n)),
            Value::U16(n) => V::I(i64::from(*n)),
            Value::I32(n) => V::I(i64::from(*n)),
            Value::U32(n) => V::I(i64::from(*n)),
            Value::I64(n) => V::I(*n),
            Value::U64(n) => V::I(i64::try_from(*n).unwrap_or(i64::MAX)),
            Value::F64(f) => V::F(*f),
            Value::Str(s) => V::S(s.as_str().to_owned()),
            Value::Signature(s) => V::S(s.to_string()),
            Value::ObjectPath(p) => V::S(p.to_string()),
            Value::Value(inner) => V::from_value(inner),
            Value::Array(a) => V::L(a.iter().map(V::from_value).collect()),
            Value::Structure(s) => V::L(s.fields().iter().map(V::from_value).collect()),
            Value::Dict(d) => V::L(
                d.iter()
                    .map(|(k, v)| V::L(vec![V::from_value(k), V::from_value(v)]))
                    .collect(),
            ),
            _ => V::None,
        }
    }

    pub fn str(&self) -> &str {
        match self {
            V::S(s) => s,
            _ => "",
        }
    }

    pub fn int(&self) -> i64 {
        match self {
            V::I(n) => *n,
            V::F(f) => *f as i64,
            V::B(b) => i64::from(*b),
            _ => 0,
        }
    }

    pub fn float(&self) -> f64 {
        match self {
            V::F(f) => *f,
            V::I(n) => *n as f64,
            _ => 0.0,
        }
    }

    pub fn bool(&self) -> bool {
        match self {
            V::B(b) => *b,
            V::I(n) => *n != 0,
            V::S(s) => matches!(s.as_str(), "on" | "true" | "1"),
            _ => false,
        }
    }

    pub fn list(&self) -> &[V] {
        match self {
            V::L(l) => l,
            _ => &[],
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, V::None)
    }

    /// Field `i` of a struct (or element of an array).
    pub fn at(&self, i: usize) -> &V {
        self.list().get(i).unwrap_or(&V::None)
    }

    pub fn strings(&self) -> Vec<String> {
        self.list().iter().map(|v| v.str().to_owned()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::StructureBuilder;

    #[test]
    fn converts_nested_values() {
        let st = StructureBuilder::new()
            .add_field(7u8)
            .add_field("x")
            .build()
            .unwrap();
        let v = V::from_value(&Value::Structure(st));
        assert_eq!(v, V::L(vec![V::I(7), V::S("x".into())]));
        assert_eq!(v.at(1).str(), "x");
        assert_eq!(v.at(5), &V::None);
    }

    #[test]
    fn variants_unwrap_and_numbers_coerce() {
        let v = V::from_value(&Value::Value(Box::new(Value::U32(5))));
        assert_eq!(v.int(), 5);
        assert!(V::S("on".into()).bool());
        assert!(!V::B(false).bool());
        assert_eq!(V::F(2.9).int(), 2);
    }
}
