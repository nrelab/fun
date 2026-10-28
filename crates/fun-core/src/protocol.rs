use serde::{Deserialize, Serialize};

use crate::{FunError, FunctionError, FunctionRequest, FunctionResponse};

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireRequest {
    pub version: u16,
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub path_parameters: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    #[serde(with = "base64_bytes")]
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireResponse {
    pub version: u16,
    pub request_id: String,
    #[serde(flatten)]
    pub result: ResultEnvelope,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ResultEnvelope {
    Response {
        status: u16,
        headers: Vec<(String, String)>,
        #[serde(with = "base64_bytes")]
        body: Vec<u8>,
    },
    Error {
        error: FunctionError,
    },
}

impl From<FunctionRequest> for WireRequest {
    fn from(request: FunctionRequest) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id: request.request_id,
            method: request.method,
            path: request.path,
            query: request.query,
            path_parameters: request.path_parameters,
            headers: request.headers,
            body: request.body,
        }
    }
}

impl WireRequest {
    pub fn into_request(self) -> Result<FunctionRequest, FunError> {
        if self.version != PROTOCOL_VERSION {
            return Err(FunError::Worker(format!(
                "unsupported worker protocol version {}",
                self.version
            )));
        }
        Ok(FunctionRequest {
            request_id: self.request_id,
            method: self.method,
            path: self.path,
            query: self.query,
            path_parameters: self.path_parameters,
            headers: self.headers,
            body: self.body,
        })
    }
}

impl WireResponse {
    pub fn into_response(
        self,
        expected_request_id: &str,
    ) -> Result<Result<FunctionResponse, FunctionError>, FunError> {
        if self.version != PROTOCOL_VERSION {
            return Err(FunError::Worker(format!(
                "unsupported worker protocol version {}",
                self.version
            )));
        }
        if self.request_id != expected_request_id {
            return Err(FunError::Worker(
                "worker response request ID did not match".to_owned(),
            ));
        }
        Ok(match self.result {
            ResultEnvelope::Response {
                status,
                headers,
                body,
            } => Ok(FunctionResponse {
                status,
                headers,
                body,
            }),
            ResultEnvelope::Error { error } => Err(error),
        })
    }
}

pub fn encode_request(request: FunctionRequest) -> Result<Vec<u8>, FunError> {
    let mut line = serde_json::to_vec(&WireRequest::from(request))?;
    line.push(b'\n');
    Ok(line)
}

pub fn decode_response(
    bytes: &[u8],
    expected_request_id: &str,
) -> Result<Result<FunctionResponse, FunctionError>, FunError> {
    let response: WireResponse = serde_json::from_slice(bytes)?;
    response.into_response(expected_request_id)
}

mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FunctionErrorKind, FunctionRequest};

    #[test]
    fn round_trips_arbitrary_body_bytes() {
        let request = FunctionRequest {
            request_id: "req-1".into(),
            method: "POST".into(),
            path: "/".into(),
            query: vec![],
            path_parameters: vec![],
            headers: vec![],
            body: vec![0, 0xff, b'\n'],
        };
        let encoded = encode_request(request.clone()).unwrap();
        let decoded: WireRequest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.into_request().unwrap(), request);
    }

    #[test]
    fn response_checks_version_and_request_id() {
        let response = WireResponse {
            version: PROTOCOL_VERSION,
            request_id: "wrong".into(),
            result: ResultEnvelope::Error {
                error: FunctionError::new(FunctionErrorKind::User, "no"),
            },
        };
        assert!(response.into_response("req-1").is_err());
    }
}
