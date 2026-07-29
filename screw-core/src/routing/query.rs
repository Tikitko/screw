use std::collections::HashMap;
use std::sync::OnceLock;

/// The request's query string, parsed on first use rather than on every request.
///
/// A handler that never looks at the query pays only for keeping the raw string
/// around. The first call to [`get`](Self::get) or [`as_map`](Self::as_map)
/// parses it and caches the result, so repeated lookups cost the same as they
/// did when the map was built eagerly. Repeated keys keep the last value.
#[derive(Debug, Default)]
pub struct Query {
    raw: Option<Box<str>>,
    parsed: OnceLock<HashMap<String, String>>,
}

impl Query {
    pub(super) fn new(raw: Option<&str>) -> Self {
        Self {
            raw: raw.map(Box::from),
            parsed: OnceLock::new(),
        }
    }

    /// The query string exactly as it arrived, without the leading `?`.
    pub fn as_str(&self) -> Option<&str> {
        self.raw.as_deref()
    }

    /// The parsed pairs, parsing on the first call.
    pub fn as_map(&self) -> &HashMap<String, String> {
        self.parsed.get_or_init(|| match &self.raw {
            Some(raw) => form_urlencoded::parse(raw.as_bytes())
                .into_owned()
                .collect(),
            None => HashMap::new(),
        })
    }

    /// The value for `key`, parsing the query string on the first call.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.as_map().get(key).map(|value| value.as_str())
    }

    /// An iterator over the pairs, parsing the query string on the first call.
    pub fn iter(&self) -> std::collections::hash_map::Iter<'_, String, String> {
        self.as_map().iter()
    }

    /// Whether the query string was absent or empty.
    pub fn is_empty(&self) -> bool {
        self.raw.as_deref().is_none_or(str::is_empty)
    }
}

impl From<HashMap<String, String>> for Query {
    fn from(map: HashMap<String, String>) -> Self {
        let parsed = OnceLock::new();
        let _ = parsed.set(map);
        Self { raw: None, parsed }
    }
}
