//! One row of the inventory: `App` and its per-source `Extra`.

use super::*;

/// One row of the inventory, from any source.
///
/// The Python dataclass does not check its field types, so a malformed
/// registry or manifest puts a None or an int in `name`/`version`/... and
/// that is what the row then holds. The Rust fields stay `String` (their
/// `str()`, for display), and the value itself, when it is not a plain str,
/// is kept in `extra` under `RAW_FIELDS`; serialising the row writes it
/// back in place of the text and leaves that key out of `extra`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct App {
    pub name: String,
    pub version: String,
    pub source: String,
    pub description: String,
    /// new version if one is available
    pub update: String,
    /// what to launch, when launchable
    pub exec_path: String,
    /// Mixed-type per-source details (`dict` in the Python).
    pub extra: Extra,
}

/// [`App::extra`]: per-source details, some straight from a file (the
/// release url in an agent cache), so a value can be nested as deep as
/// Python's json reads (52000 levels). serde's own clone and drop recurse
/// once per level; these do not, so a row can be cloned and dropped on any
/// thread -- the window's, or a worker's 2 MiB stack.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extra(pub Map<String, Value>);

impl std::ops::Deref for Extra {
    type Target = Map<String, Value>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Extra {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Map<String, Value>> for Extra {
    fn from(m: Map<String, Value>) -> Self {
        Extra(m)
    }
}

impl Clone for Extra {
    fn clone(&self) -> Self {
        Extra(self.0.iter().map(|(k, v)| (k.clone(), crate::util::tree::clone_value(v))).collect())
    }
}

impl PartialEq for Extra {
    fn eq(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && self.0.iter().all(|(k, v)| other.0.get(k).is_some_and(|w| crate::util::tree::eq_value(v, w)))
    }
}

impl Drop for Extra {
    fn drop(&mut self) {
        for (_, v) in std::mem::take(&mut self.0) {
            crate::util::tree::drop_value(v);
        }
    }
}

/// The `extra` key holding the Python values of fields that are not a
/// plain str (two noncharacters no JSON the app reads contains).
pub(super) const RAW_FIELDS: &str = "\u{10FFFF}\u{FDD0}py:fields";

pub(super) const APP_FIELDS: [&str; 6] = ["name", "version", "source", "description", "update", "exec_path"];

impl App {
    fn field_mut(&mut self, field: &str) -> Option<&mut String> {
        Some(match field {
            "name" => &mut self.name,
            "version" => &mut self.version,
            "source" => &mut self.source,
            "description" => &mut self.description,
            "update" => &mut self.update,
            "exec_path" => &mut self.exec_path,
            _ => return None,
        })
    }

    /// Set a field to a Python value: its `str()` as the text, and the value
    /// itself kept when it is not a plain str.
    fn set_value(&mut self, field: &str, v: &Value) {
        let plain = matches!(v, Value::String(s) if exotic_str(s).is_none());
        if let Some(f) = self.field_mut(field) {
            // A falsy update (None, 0) shows as no update, as `has_update`
            // reads it.
            *f = if field == "update" && !truthy(v) { String::new() } else { py_str(v) };
        }
        if plain {
            if let Some(Value::Object(r)) = self.extra.get_mut(RAW_FIELDS) {
                r.remove(field);
            }
            return;
        }
        if let Value::Object(r) =
            self.extra.entry(RAW_FIELDS.to_string()).or_insert_with(|| Value::Object(Map::new()))
        {
            r.insert(field.to_string(), v.clone());
        }
    }

    /// `set_value` for several fields; returns the row.
    pub(super) fn with_values(mut self, fields: &[(&str, &Value)]) -> App {
        for (k, v) in fields {
            self.set_value(k, v);
        }
        self
    }

    /// `extra` as the Python dict holds it (without `RAW_FIELDS`).
    pub fn extra_dict(&self) -> Extra {
        let mut m = self.extra.clone();
        if let Some(raw) = m.remove(RAW_FIELDS) {
            crate::util::tree::drop_value(raw);
        }
        m
    }

    /// `App(name=..., version=..., source=...)` with every other field at
    /// its default; set the rest with struct update syntax.
    pub fn new(name: &str, version: &str, source: &str) -> App {
        App { name: name.to_string(), version: version.to_string(), source: source.to_string(), ..Default::default() }
    }

    /// `@property has_update`.
    pub fn has_update(&self) -> bool {
        // `bool(update)`, of the Python value when it is not a str.
        if let Some(v) = self.extra.get(RAW_FIELDS).and_then(|r| r.get("update")) {
            return truthy(v);
        }
        !self.update.is_empty()
    }
}

impl Serialize for App {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let raw = self.extra.get(RAW_FIELDS).and_then(Value::as_object);
        let text = [&self.name, &self.version, &self.source, &self.description, &self.update, &self.exec_path];
        let mut st = s.serialize_struct("App", 7)?;
        for (k, t) in APP_FIELDS.iter().zip(text) {
            match raw.and_then(|r| r.get(*k)) {
                Some(v) => st.serialize_field(k, v)?,
                None => st.serialize_field(k, t)?,
            }
        }
        if raw.is_some() {
            st.serialize_field("extra", &self.extra_dict())?;
        } else {
            st.serialize_field("extra", &self.extra)?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for App {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<App, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Wire {
            name: Option<Value>,
            version: Option<Value>,
            source: Option<Value>,
            description: Option<Value>,
            update: Option<Value>,
            exec_path: Option<Value>,
            extra: Map<String, Value>,
        }
        let w = Wire::deserialize(d)?;
        let mut app = App { extra: w.extra.into(), ..Default::default() };
        app.extra.remove(RAW_FIELDS);
        for (k, v) in APP_FIELDS.iter().zip([w.name, w.version, w.source, w.description, w.update, w.exec_path]) {
            if let Some(v) = v {
                app.set_value(k, &v);
            }
        }
        Ok(app)
    }
}
