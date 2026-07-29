use hyper::StatusCode;
use serde::ser::SerializeStructVariant;
use serde::{Serialize, Serializer};
use std::convert::Infallible;

/// The status code a response content answers with.
///
/// Returned by value, so a variant may carry a code decided at runtime rather
/// than only naming a constant.
pub trait ApiResponseContentBase {
    fn status_code(&self) -> StatusCode;
}

impl ApiResponseContentBase for Infallible {
    fn status_code(&self) -> StatusCode {
        unreachable!()
    }
}

/// The success half of a handler's response.
///
/// Serialized as `{"success": {"identifier", "description", "data"}}`. Use
/// [`Infallible`] as the type when an endpoint cannot succeed.
pub trait ApiResponseContentSuccess: ApiResponseContentBase {
    type Data: Serialize;
    fn identifier(&self) -> &'static str;
    fn description(&self) -> Option<String>;
    fn data(&self) -> &Self::Data;
}

impl ApiResponseContentSuccess for Infallible {
    type Data = ();
    fn identifier(&self) -> &'static str {
        unreachable!()
    }
    fn description(&self) -> Option<String> {
        unreachable!()
    }
    fn data(&self) -> &Self::Data {
        unreachable!()
    }
}

/// The failure half of a handler's response.
///
/// Serialized as `{"failure": {"identifier", "reason"}}`. Use [`Infallible`] as
/// the type when an endpoint cannot fail.
pub trait ApiResponseContentFailure: ApiResponseContentBase {
    fn identifier(&self) -> &'static str;
    fn reason(&self) -> Option<String>;
}

impl ApiResponseContentFailure for Infallible {
    fn identifier(&self) -> &'static str {
        unreachable!()
    }
    fn reason(&self) -> Option<String> {
        unreachable!()
    }
}

pub enum ApiResponseContent<Success, Failure>
where
    Success: ApiResponseContentSuccess,
    Failure: ApiResponseContentFailure,
{
    Success(Success),
    Failure(Failure),
}

impl<Success, Failure> ApiResponseContentBase for ApiResponseContent<Success, Failure>
where
    Success: ApiResponseContentSuccess,
    Failure: ApiResponseContentFailure,
{
    fn status_code(&self) -> StatusCode {
        match self {
            ApiResponseContent::Success(success) => success.status_code(),
            ApiResponseContent::Failure(failure) => failure.status_code(),
        }
    }
}

impl<Success, Failure> Serialize for ApiResponseContent<Success, Failure>
where
    Success: ApiResponseContentSuccess,
    Failure: ApiResponseContentFailure,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ApiResponseContent::Success(success) => {
                let mut state =
                    serializer.serialize_struct_variant("api_response_content", 0, "success", 3)?;
                state.serialize_field("identifier", success.identifier())?;
                state.serialize_field("description", &success.description())?;
                state.serialize_field("data", success.data())?;
                state.end()
            }
            ApiResponseContent::Failure(failure) => {
                let mut state =
                    serializer.serialize_struct_variant("api_response_content", 1, "failure", 2)?;
                state.serialize_field("identifier", failure.identifier())?;
                state.serialize_field("reason", &failure.reason())?;
                state.end()
            }
        }
    }
}

impl<Success, Failure> From<Result<Success, Failure>> for ApiResponseContent<Success, Failure>
where
    Success: ApiResponseContentSuccess,
    Failure: ApiResponseContentFailure,
{
    fn from(value: Result<Success, Failure>) -> Self {
        match value {
            Ok(ok) => Self::Success(ok),
            Err(err) => Self::Failure(err),
        }
    }
}

pub struct ApiResponse<Success, Failure>
where
    Success: ApiResponseContentSuccess,
    Failure: ApiResponseContentFailure,
{
    pub content: ApiResponseContent<Success, Failure>,
}

impl<Success, Failure> ApiResponse<Success, Failure>
where
    Success: ApiResponseContentSuccess,
    Failure: ApiResponseContentFailure,
{
    pub fn success(content_success: Success) -> Self {
        Self {
            content: ApiResponseContent::Success(content_success),
        }
    }

    pub fn failure(content_failure: Failure) -> Self {
        Self {
            content: ApiResponseContent::Failure(content_failure),
        }
    }
}

impl<Success, Failure> From<Result<Success, Failure>> for ApiResponse<Success, Failure>
where
    Success: ApiResponseContentSuccess,
    Failure: ApiResponseContentFailure,
{
    fn from(value: Result<Success, Failure>) -> Self {
        Self {
            content: ApiResponseContent::from(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Upstream {
        status_code: StatusCode,
    }

    impl ApiResponseContentBase for Upstream {
        fn status_code(&self) -> StatusCode {
            self.status_code
        }
    }

    impl ApiResponseContentFailure for Upstream {
        fn identifier(&self) -> &'static str {
            "UPSTREAM"
        }
        fn reason(&self) -> Option<String> {
            None
        }
    }

    #[test]
    fn a_failure_can_carry_a_status_code_decided_at_runtime() {
        for status_code in [StatusCode::BAD_GATEWAY, StatusCode::from_u16(499).unwrap()] {
            let response: ApiResponse<Infallible, Upstream> =
                ApiResponse::failure(Upstream { status_code });
            assert_eq!(response.content.status_code(), status_code);
        }
    }
}
