#![forbid(unsafe_code)]

#[cfg(feature = "ws")]
pub mod channel;
pub mod request;
pub mod response;

#[cfg(feature = "json")]
pub mod json;
#[cfg(feature = "xml")]
pub mod xml;
#[cfg(any(feature = "json", feature = "xml"))]
#[derive(derive_error::Error, Debug)]
enum ApiRequestContentTypeError {
    Missed,
    Incorrect,
}

#[cfg(any(feature = "json", feature = "xml"))]
fn check_content_type(
    headers: &hyper::HeaderMap,
    expected: &str,
) -> screw_components::dyn_result::DResult<()> {
    let Some(header_value) = headers.get(hyper::header::CONTENT_TYPE) else {
        return Err(ApiRequestContentTypeError::Missed.into());
    };

    let media_type = header_value
        .to_str()?
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();

    if media_type.is_empty() {
        Err(ApiRequestContentTypeError::Missed.into())
    } else if media_type.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(ApiRequestContentTypeError::Incorrect.into())
    }
}

#[cfg(test)]
#[cfg(any(feature = "json", feature = "xml"))]
mod content_type_tests {
    use super::*;
    use hyper::HeaderMap;
    use hyper::header::CONTENT_TYPE;

    fn headers_with(content_type: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(content_type) = content_type {
            headers.insert(CONTENT_TYPE, content_type.parse().unwrap());
        }
        headers
    }

    fn check(content_type: Option<&str>) -> Result<(), String> {
        check_content_type(&headers_with(content_type), "application/json")
            .map_err(|error| format!("{error:?}"))
    }

    #[test]
    fn exact_media_type_is_accepted() {
        assert!(check(Some("application/json")).is_ok());
    }

    #[test]
    fn parameters_are_ignored() {
        assert!(check(Some("application/json; charset=utf-8")).is_ok());
        assert!(check(Some("application/json;charset=UTF-8")).is_ok());
        assert!(check(Some("application/json ; boundary=x")).is_ok());
    }

    #[test]
    fn comparison_is_case_insensitive() {
        assert!(check(Some("Application/JSON")).is_ok());
        assert!(check(Some("APPLICATION/JSON; CHARSET=UTF-8")).is_ok());
    }

    #[test]
    fn other_media_types_are_rejected() {
        assert_eq!(check(Some("text/plain")), Err("Incorrect".to_owned()));
        assert_eq!(
            check(Some("application/xml; charset=utf-8")),
            Err("Incorrect".to_owned())
        );
    }

    #[test]
    fn missing_or_empty_header_is_reported_as_missed() {
        assert_eq!(check(None), Err("Missed".to_owned()));
        assert_eq!(check(Some("")), Err("Missed".to_owned()));
        assert_eq!(check(Some("  ")), Err("Missed".to_owned()));
    }
}
