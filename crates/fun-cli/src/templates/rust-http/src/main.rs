use std::io;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Deserialize)]
struct Request {
    version: u16,
    request_id: String,
    method: String,
    path: String,
    #[serde(default)]
    query: Vec<(String, String)>,
    #[serde(default)]
    path_parameters: Vec<(String, String)>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Serialize)]
struct WireResponse {
    version: u16,
    request_id: String,
    outcome: &'static str,
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Serialize)]
struct WireError {
    version: u16,
    request_id: String,
    outcome: &'static str,
    error: FunctionError,
}

#[derive(Serialize)]
struct FunctionError {
    kind: &'static str,
    message: String,
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

async fn handle(request: Request, body: Vec<u8>) -> Result<Response, String> {
    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|error| format!("request body must be JSON: {error}"))?;
    let name = payload
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("world");
    let greeting = std::env::var("GREETING").unwrap_or_else(|_| "Hello".into());
    let response_body = serde_json::json!({ "message": format!("{greeting}, {name}!") });
    let _ = (
        request.method,
        request.path,
        request.query,
        request.path_parameters,
        request.headers,
    );
    Ok(Response {
        status: 200,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::to_vec(&response_body).expect("JSON value serializes"),
    })
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();
    let mut output = stdout;
    while let Some(line) = lines.next_line().await? {
        let parsed = serde_json::from_str::<Request>(&line);
        let (request_id, result) = match parsed {
            Ok(request) => {
                if request.version != 1 {
                    let request_id = request.request_id.clone();
                    (request_id, Err("unsupported protocol version".to_owned()))
                } else {
                    let request_id = request.request_id.clone();
                    match STANDARD.decode(&request.body) {
                        Ok(body) => (request_id, handle(request, body).await),
                        Err(error) => (
                            request_id,
                            Err(format!("invalid request body encoding: {error}")),
                        ),
                    }
                }
            }
            Err(error) => {
                eprintln!("invalid worker request: {error}");
                continue;
            }
        };
        let encoded = match result {
            Ok(response) => serde_json::to_vec(&WireResponse {
                version: 1,
                request_id,
                outcome: "response",
                status: response.status,
                headers: response.headers,
                body: STANDARD.encode(response.body),
            }),
            Err(message) => serde_json::to_vec(&WireError {
                version: 1,
                request_id,
                outcome: "error",
                error: FunctionError {
                    kind: "user",
                    message,
                },
            }),
        };
        match encoded {
            Ok(mut frame) => {
                frame.push(b'\n');
                output.write_all(&frame).await?;
                output.flush().await?;
            }
            Err(error) => eprintln!("failed to encode worker response: {error}"),
        }
    }
    Ok(())
}
