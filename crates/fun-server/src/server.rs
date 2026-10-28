use std::{error::Error as _, net::IpAddr, sync::Arc, time::Duration};

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, HeaderName, HeaderValue, Request, Response, StatusCode},
    response::IntoResponse,
    Router,
};
use fun_core::{FunError, Project, RouteTable};
use tokio::{net::TcpListener, time::timeout};
use tracing::{info, warn};
use url::form_urlencoded;

use crate::runtime::{worker_request, InvocationError, WorkerPool};

#[derive(Clone, Copy, Debug)]
pub struct ServerOptions {
    pub host: IpAddr,
    pub port: u16,
    pub watch: Option<bool>,
}

#[derive(Clone)]
struct AppState {
    pool: Arc<WorkerPool>,
    routes: Arc<RouteTable>,
    max_request_body_bytes: usize,
}

pub async fn serve(project: Project, options: ServerOptions) -> Result<(), FunError> {
    if !options.host.is_loopback() {
        return Err(FunError::Config(
            "server host must be a loopback address".into(),
        ));
    }
    if options.port == 0 {
        return Err(FunError::Config(
            "server port must be between 1 and 65535".into(),
        ));
    }
    let routes = Arc::new(RouteTable::new(&project.config.routes)?);
    let pool = Arc::new(WorkerPool::build_and_start(&project).await?);
    let state = AppState {
        pool: pool.clone(),
        routes,
        max_request_body_bytes: project.config.functions.max_request_body_bytes,
    };
    let app = Router::new().fallback(handle_request).with_state(state);
    let listener = TcpListener::bind((options.host, options.port))
        .await
        .map_err(|error| FunError::Worker(format!("could not bind local server: {error}")))?;
    let address = listener
        .local_addr()
        .map_err(|error| FunError::Worker(format!("could not inspect server address: {error}")))?;
    let watcher = if options.watch.unwrap_or(project.config.dev.watch) {
        match crate::watch_project(project.clone(), pool.clone()) {
            Ok(watcher) => Some(tokio::spawn(watcher)),
            Err(error) => {
                pool.shutdown().await;
                return Err(error);
            }
        }
    } else {
        None
    };
    println!("Rust Fun listening on http://{address}");
    info!(%address, "local function server started");

    let result = timeout(
        Duration::from_secs(10),
        axum::serve(listener, app).with_graceful_shutdown(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                warn!(%error, "failed to listen for shutdown signal");
            }
        }),
    )
    .await
    .map_err(|_| FunError::Worker("HTTP server did not shut down within 10 seconds".into()))
    .and_then(|result| {
        result.map_err(|error| FunError::Worker(format!("HTTP server failed: {error}")))
    });
    if let Some(watcher) = watcher {
        watcher.abort();
        if let Err(error) = watcher.await {
            if !error.is_cancelled() {
                warn!(%error, "file watcher task failed");
            }
        }
    }
    pool.shutdown().await;
    result
}

async fn handle_request(State(state): State<AppState>, request: Request<Body>) -> Response<Body> {
    let uri = request.uri().clone();
    let method = request.method().as_str().to_owned();
    if uri.path() == "/__fun/health" {
        return if method == "GET" {
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"status":"ok"}"#,
            )
                .into_response()
        } else {
            let mut response = response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET"));
            response
        };
    }
    let Some(route_match) = state.routes.resolve(uri.path(), &method) else {
        return response(StatusCode::NOT_FOUND, "route not found");
    };
    if let Some(allowed) = &route_match.allowed_methods {
        if !allowed
            .iter()
            .any(|allowed_method| allowed_method == &method)
        {
            let mut response = response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
            if let Ok(value) = HeaderValue::from_str(&allowed.join(", ")) {
                response.headers_mut().insert(header::ALLOW, value);
            }
            return response;
        }
    }

    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, state.max_request_body_bytes).await {
        Ok(body) => body.to_vec(),
        Err(error) => {
            let too_large = error
                .source()
                .is_some_and(|source| source.is::<http_body_util::LengthLimitError>());
            if too_large {
                return response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
            }
            return response(StatusCode::BAD_REQUEST, "could not read request body");
        }
    };
    let query = parts
        .uri
        .query()
        .map(|query| {
            form_urlencoded::parse(query.as_bytes())
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let headers = parts
        .headers
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect::<Result<Vec<_>, _>>();
    let headers = match headers {
        Ok(headers) => headers,
        Err(_) => return response(StatusCode::BAD_REQUEST, "invalid request header encoding"),
    };
    let function_request = worker_request(
        method,
        parts.uri.path().to_owned(),
        query,
        route_match.path_parameters,
        headers,
        body,
    );
    let request_id = function_request.request_id.clone();

    let result = state
        .pool
        .invoke(&route_match.function, function_request)
        .await;
    match result {
        Ok(function_response) => {
            let status = StatusCode::from_u16(function_response.status)
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut response = Response::new(Body::from(function_response.body));
            *response.status_mut() = status;
            for (name, value) in function_response.headers {
                if let (Ok(name), Ok(value)) = (
                    HeaderName::from_bytes(name.as_bytes()),
                    HeaderValue::from_str(&value),
                ) {
                    response.headers_mut().append(name, value);
                }
            }
            info!(%request_id, function = %route_match.function, status = status.as_u16(), "function request completed");
            response
        }
        Err(error) => {
            let (status, message) = match error {
                InvocationError::Busy => (StatusCode::SERVICE_UNAVAILABLE, "runtime is busy"),
                InvocationError::RequestTooLarge => {
                    (StatusCode::PAYLOAD_TOO_LARGE, "request body too large")
                }
                InvocationError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "function timed out"),
                InvocationError::Worker(message) => {
                    warn!(%request_id, %message, "function worker failed");
                    (StatusCode::BAD_GATEWAY, "function worker failed")
                }
                InvocationError::Function(error) => {
                    warn!(%request_id, error = %error.message, "function returned an error");
                    let status = match error.kind {
                        fun_core::FunctionErrorKind::InvalidRequest => StatusCode::BAD_REQUEST,
                        fun_core::FunctionErrorKind::User
                        | fun_core::FunctionErrorKind::Internal => {
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    };
                    (status, "function failed")
                }
                InvocationError::NotFound => (StatusCode::NOT_FOUND, "function was not found"),
            };
            response(status, message)
        }
    }
}

fn response(status: StatusCode, message: &str) -> Response<Body> {
    (status, message.to_owned()).into_response()
}
