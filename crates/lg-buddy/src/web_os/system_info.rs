use super::{WebOsClient, WebOsClientError};
use serde_json::{json, Value};
use std::error::Error;
use std::fmt;

const GET_SYSTEM_INFO_URI: &str = "ssap://system/getSystemInfo";

#[derive(Debug)]
pub enum WebOsModelNameError {
    Request { source: WebOsClientError },
    MissingPayload,
    InvalidPayload,
    MissingReturnValue,
    InvalidReturnValue,
    RequestRejected { message: Option<String> },
    MissingModelName,
    InvalidModelName,
    EmptyModelName,
}

impl fmt::Display for WebOsModelNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { source } => {
                write!(f, "could not read webOS system information: {source}")
            }
            Self::MissingPayload => write!(f, "webOS system-information response has no payload"),
            Self::InvalidPayload => {
                write!(
                    f,
                    "webOS system-information response payload is not an object"
                )
            }
            Self::MissingReturnValue => {
                write!(f, "webOS system-information response has no return value")
            }
            Self::InvalidReturnValue => write!(
                f,
                "webOS system-information response return value is not a boolean"
            ),
            Self::RequestRejected {
                message: Some(message),
            } => write!(
                f,
                "webOS system-information request was rejected: {message}"
            ),
            Self::RequestRejected { message: None } => {
                write!(f, "webOS system-information request was rejected")
            }
            Self::MissingModelName => {
                write!(f, "webOS system-information response has no model name")
            }
            Self::InvalidModelName => {
                write!(
                    f,
                    "webOS system-information response model name is not a string"
                )
            }
            Self::EmptyModelName => {
                write!(f, "webOS system-information response model name is empty")
            }
        }
    }
}

impl Error for WebOsModelNameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Request { source } => Some(source),
            Self::MissingPayload
            | Self::InvalidPayload
            | Self::MissingReturnValue
            | Self::InvalidReturnValue
            | Self::RequestRejected { .. }
            | Self::MissingModelName
            | Self::InvalidModelName
            | Self::EmptyModelName => None,
        }
    }
}

impl WebOsClient {
    pub fn model_name(&mut self) -> Result<String, WebOsModelNameError> {
        let response = self
            .send_request(GET_SYSTEM_INFO_URI, json!({}))
            .map_err(|source| WebOsModelNameError::Request { source })?;
        parse_model_name_response(&response)
    }
}

fn parse_model_name_response(response: &Value) -> Result<String, WebOsModelNameError> {
    let payload = match response.get("payload") {
        Some(Value::Object(payload)) => payload,
        Some(_) => return Err(WebOsModelNameError::InvalidPayload),
        None => return Err(WebOsModelNameError::MissingPayload),
    };

    match payload.get("returnValue") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => {
            let message = payload
                .get("errorText")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .map(str::to_string);
            return Err(WebOsModelNameError::RequestRejected { message });
        }
        Some(_) => return Err(WebOsModelNameError::InvalidReturnValue),
        None => return Err(WebOsModelNameError::MissingReturnValue),
    }

    let model_name = match payload.get("modelName") {
        Some(Value::String(model_name)) => model_name,
        Some(_) => return Err(WebOsModelNameError::InvalidModelName),
        None => return Err(WebOsModelNameError::MissingModelName),
    };
    if model_name.trim().is_empty() {
        return Err(WebOsModelNameError::EmptyModelName);
    }

    Ok(model_name.clone())
}

#[cfg(test)]
mod tests {
    use super::{parse_model_name_response, WebOsModelNameError};
    use serde_json::json;

    #[test]
    fn parses_model_name_from_system_info() {
        assert_eq!(
            parse_model_name_response(&json!({
                "payload": {"returnValue": true, "modelName": "OLED42C2"}
            }))
            .expect("model name"),
            "OLED42C2"
        );
    }

    #[test]
    fn rejects_missing_and_empty_model_names() {
        let missing = parse_model_name_response(&json!({
            "payload": {"returnValue": true}
        }))
        .expect_err("missing model name should fail");
        assert!(matches!(missing, WebOsModelNameError::MissingModelName));

        let empty = parse_model_name_response(&json!({
            "payload": {"returnValue": true, "modelName": "  "}
        }))
        .expect_err("empty model name should fail");
        assert!(matches!(empty, WebOsModelNameError::EmptyModelName));
    }

    #[test]
    fn rejects_malformed_and_rejected_system_info_responses() {
        let malformed = parse_model_name_response(&json!({"payload": []}))
            .expect_err("malformed payload should fail");
        assert!(matches!(malformed, WebOsModelNameError::InvalidPayload));

        let rejected = parse_model_name_response(&json!({
            "payload": {"returnValue": false, "errorText": "denied"}
        }))
        .expect_err("rejected response should fail");
        assert!(matches!(
            rejected,
            WebOsModelNameError::RequestRejected { .. }
        ));
    }
}
