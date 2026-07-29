use std::collections::HashMap;
use std::sync::OnceLock;

/// The request's query string, parsed on first use rather than on every request.
///
/// A handler that never looks at the query pays only for keeping the raw string
/// around. The first call that needs the pairs -- [`get`](Self::get),
/// [`iter`](Self::iter), [`is_empty`](Self::is_empty) or
/// [`as_map`](Self::as_map) -- parses it and caches the result, so repeated
/// lookups cost the same as they did when the map was built eagerly. Repeated
/// keys keep the last value.
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
    ///
    /// `None` when the request carried no query string, and also when the
    /// `Query` was built from a map rather than from a request, since there is
    /// no original to hand back. Reach for [`as_map`](Self::as_map) instead of
    /// this when what you want is the pairs.
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

    /// Whether there are no pairs, parsing the query string on the first call.
    ///
    /// This answers the same question [`iter`](Self::iter) does, so a query
    /// string that holds no pairs -- `""`, or `"&&"` -- is empty here even
    /// though [`as_str`](Self::as_str) has something to return.
    pub fn is_empty(&self) -> bool {
        self.as_map().is_empty()
    }
}

impl From<HashMap<String, String>> for Query {
    /// Builds a `Query` that is already parsed. [`as_str`](Query::as_str)
    /// returns `None` on one of these, since there was never a query string.
    fn from(map: HashMap<String, String>) -> Self {
        Self {
            raw: None,
            parsed: OnceLock::from(map),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_built_from_a_map_agrees_with_its_own_pairs() {
        let query = Query::from(HashMap::from([("a".to_owned(), "1".to_owned())]));

        assert!(!query.is_empty());
        assert_eq!(query.get("a"), Some("1"));
        assert_eq!(query.iter().count(), 1);
        assert_eq!(query.as_str(), None);
    }

    #[test]
    fn a_query_string_that_holds_no_pairs_is_empty() {
        assert!(Query::new(None).is_empty());
        assert!(Query::new(Some("")).is_empty());
        assert!(Query::new(Some("&&")).is_empty());
        assert!(!Query::new(Some("a=")).is_empty());
    }

    #[test]
    fn the_raw_string_survives_parsing() {
        let query = Query::new(Some("a=1&a=2"));

        assert_eq!(query.get("a"), Some("2"));
        assert_eq!(query.as_str(), Some("a=1&a=2"));
    }
}
