use std::{
    fmt,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{json, Map, Value};

use crate::provider::{ModelError, ModelMessageRole, ModelProvider, ModelRequest, ModelResponse};

pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434/api";
pub const PROVIDER_RESPONSE_BYTE_CAP: usize = 1024 * 1024;

const DEFAULT_STRUCTURED_TEMPERATURE: f32 = 0.0;
const STRUCTURED_TEMPERATURE_CEILING: f32 = 0.2;

#[derive(Debug, Clone, PartialEq)]
pub struct OllamaConfig {
    pub base_url: String,
    pub structured_temperature: f32,
    pub structured_temperature_ceiling: f32,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_OLLAMA_BASE_URL.to_string(),
            structured_temperature: DEFAULT_STRUCTURED_TEMPERATURE,
            structured_temperature_ceiling: STRUCTURED_TEMPERATURE_CEILING,
        }
    }
}

pub struct OllamaProvider {
    config: OllamaConfig,
    http_client: Arc<dyn OllamaHttpClient>,
}

impl OllamaProvider {
    pub fn new(config: OllamaConfig) -> Self {
        Self {
            config,
            http_client: Arc::new(StdOllamaHttpClient::default()),
        }
    }

    pub fn with_http_client(config: OllamaConfig, http_client: Arc<dyn OllamaHttpClient>) -> Self {
        Self {
            config,
            http_client,
        }
    }

    pub fn build_chat_request(
        &self,
        request: &ModelRequest,
    ) -> Result<OllamaHttpRequest, ModelError> {
        let url = ollama_chat_url(&self.config.base_url)?;
        let temperature = self.effective_temperature(request)?;
        let mut messages = Vec::new();
        let system = match &request.response_schema {
            Some(schema) => {
                let schema_instruction =
                    format!("Respond with JSON matching this exact schema:\n{schema}");
                if request.system.trim().is_empty() {
                    schema_instruction
                } else {
                    format!("{}\n\n{schema_instruction}", request.system)
                }
            }
            None => request.system.clone(),
        };

        if !system.trim().is_empty() {
            messages.push(json!({
                "role": "system",
                "content": system,
            }));
        }

        for message in &request.messages {
            messages.push(json!({
                "role": ollama_role(message.role),
                "content": message.content,
            }));
        }

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("stream".to_string(), Value::Bool(false));
        body.insert(
            "options".to_string(),
            json!({
                "temperature": f64::from(temperature),
            }),
        );
        if let Some(schema) = &request.response_schema {
            body.insert("format".to_string(), schema.clone());
        }

        Ok(OllamaHttpRequest::new(
            "POST".to_string(),
            url,
            Value::Object(body),
        ))
    }

    fn effective_temperature(&self, request: &ModelRequest) -> Result<f32, ModelError> {
        if !request.temperature.is_finite() {
            return Err(ollama_provider_error(
                "model request temperature must be finite",
                false,
                &self.config.base_url,
                &request.model,
            ));
        }
        if !self.config.structured_temperature.is_finite()
            || !self.config.structured_temperature_ceiling.is_finite()
        {
            return Err(ollama_provider_error(
                "Ollama structured temperature config must be finite",
                false,
                &self.config.base_url,
                &request.model,
            ));
        }

        if request.response_schema.is_some()
            && request.temperature > self.config.structured_temperature_ceiling
        {
            Ok(self.config.structured_temperature)
        } else {
            Ok(request.temperature)
        }
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new(OllamaConfig::default())
    }
}

impl ModelProvider for OllamaProvider {
    fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let http_request = self.build_chat_request(&request)?;
        let started = Instant::now();
        let http_response = self
            .http_client
            .send(http_request, request.timeout_ms)
            .map_err(|error| http_error_to_model_error(error, &self.config.base_url, &request))?;
        let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

        parse_chat_response(http_response, latency_ms, &self.config.base_url, &request)
    }
}

pub trait OllamaHttpClient: Send + Sync {
    fn send(
        &self,
        request: OllamaHttpRequest,
        timeout_ms: u64,
    ) -> Result<OllamaHttpResponse, OllamaHttpError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct OllamaHttpRequest {
    method: String,
    url: String,
    body: Value,
}

impl OllamaHttpRequest {
    pub fn new(method: String, url: String, body: Value) -> Self {
        Self { method, url, body }
    }

    pub fn method(&self) -> &str {
        &self.method
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn body(&self) -> &Value {
        &self.body
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaHttpResponse {
    status_code: u16,
    body: String,
}

impl OllamaHttpResponse {
    pub fn new(status_code: u16, body: String) -> Self {
        Self { status_code, body }
    }

    pub fn status_code(&self) -> u16 {
        self.status_code
    }

    pub fn body(&self) -> &str {
        &self.body
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OllamaHttpError {
    ConnectionRefused(String),
    Timeout(String),
    Transport(String),
    InvalidAddress(String),
    ResponseTooLarge { limit_bytes: usize },
}

impl fmt::Display for OllamaHttpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionRefused(message)
            | Self::Timeout(message)
            | Self::Transport(message)
            | Self::InvalidAddress(message) => formatter.write_str(message),
            Self::ResponseTooLarge { limit_bytes } => write!(
                formatter,
                "Ollama raw HTTP response exceeded the {limit_bytes}-byte limit"
            ),
        }
    }
}

struct StdOllamaHttpClient {
    resolver: Arc<dyn OllamaAddressResolver>,
    connector: Arc<dyn OllamaConnector>,
}

impl StdOllamaHttpClient {
    fn with_resolver(resolver: Arc<dyn OllamaAddressResolver>) -> Self {
        Self {
            resolver,
            connector: Arc::new(StdOllamaConnector),
        }
    }

    #[cfg(test)]
    fn with_transport(
        resolver: Arc<dyn OllamaAddressResolver>,
        connector: Arc<dyn OllamaConnector>,
    ) -> Self {
        Self {
            resolver,
            connector,
        }
    }
}

impl Default for StdOllamaHttpClient {
    fn default() -> Self {
        Self::with_resolver(Arc::new(StdOllamaAddressResolver))
    }
}

trait OllamaAddressResolver: Send + Sync {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, OllamaHttpError>;
}

struct StdOllamaAddressResolver;

impl OllamaAddressResolver for StdOllamaAddressResolver {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, OllamaHttpError> {
        local_socket_addresses(host, port)
    }
}

fn local_socket_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, OllamaHttpError> {
    if host.eq_ignore_ascii_case("localhost") {
        return Ok(vec![
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        ]);
    }
    host.parse::<IpAddr>()
        .map(|address| vec![SocketAddr::new(address, port)])
        .map_err(|_| {
            OllamaHttpError::InvalidAddress(format!(
                "Ollama host '{host}' is not allowed; use localhost or a numeric IPv4/IPv6 address so the request deadline does not depend on blocking system DNS"
            ))
        })
}

trait OllamaConnector: Send + Sync {
    fn connect(&self, address: &SocketAddr, timeout: Duration)
        -> Result<TcpStream, std::io::Error>;
}

struct StdOllamaConnector;

impl OllamaConnector for StdOllamaConnector {
    fn connect(
        &self,
        address: &SocketAddr,
        timeout: Duration,
    ) -> Result<TcpStream, std::io::Error> {
        TcpStream::connect_timeout(address, timeout)
    }
}

struct RequestDeadline {
    expires_at: Instant,
}

impl RequestDeadline {
    fn new(timeout_ms: u64) -> Result<Self, OllamaHttpError> {
        let timeout = Duration::from_millis(timeout_ms.max(1));
        let expires_at = Instant::now().checked_add(timeout).ok_or_else(|| {
            OllamaHttpError::Transport("Ollama request timeout is too large".to_string())
        })?;
        Ok(Self { expires_at })
    }

    fn remaining(&self, phase: &str) -> Result<Duration, OllamaHttpError> {
        self.expires_at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                OllamaHttpError::Timeout(format!("Ollama request deadline expired during {phase}"))
            })
    }
}

impl OllamaHttpClient for StdOllamaHttpClient {
    fn send(
        &self,
        request: OllamaHttpRequest,
        timeout_ms: u64,
    ) -> Result<OllamaHttpResponse, OllamaHttpError> {
        let deadline = RequestDeadline::new(timeout_ms)?;
        let url = validate_local_ollama_endpoint(request.url())?;
        deadline.remaining("address resolution")?;
        let socket_addrs = self.resolver.resolve(&url.host, url.port)?;
        deadline.remaining("address resolution")?;
        let body = serde_json::to_vec(request.body()).map_err(|error| {
            OllamaHttpError::Transport(format!("could not encode JSON: {error}"))
        })?;
        let wire_request = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nAccept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            request.method(),
            url.path,
            url.host_header,
            body.len(),
        );

        let mut last_connect_error = None;
        for socket_addr in socket_addrs {
            let remaining = deadline.remaining("connection")?;
            let stream = match self.connector.connect(&socket_addr, remaining) {
                Ok(stream) => stream,
                Err(error) => {
                    last_connect_error = Some(map_connect_error(error));
                    continue;
                }
            };

            return send_http_request(stream, &wire_request, &body, &deadline);
        }

        Err(last_connect_error.unwrap_or_else(|| {
            OllamaHttpError::Transport(format!("could not resolve Ollama host '{}'", url.host))
        }))
    }
}

fn send_http_request(
    mut stream: TcpStream,
    wire_request: &str,
    body: &[u8],
    deadline: &RequestDeadline,
) -> Result<OllamaHttpResponse, OllamaHttpError> {
    write_with_deadline(
        &mut stream,
        wire_request.as_bytes(),
        deadline,
        "request headers",
    )?;
    write_with_deadline(&mut stream, body, deadline, "request body")?;

    let mut response = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let remaining_capacity = PROVIDER_RESPONSE_BYTE_CAP.saturating_sub(response.len());
        let read_capacity = if remaining_capacity == 0 {
            1
        } else {
            remaining_capacity.min(buffer.len())
        };
        stream
            .set_read_timeout(Some(deadline.remaining("response read")?))
            .map_err(|error| map_io_error("could not set read timeout", error))?;
        let read = match stream.read(&mut buffer[..read_capacity]) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(map_io_error("could not read Ollama response", error)),
        };
        if read == 0 {
            break;
        }
        if response.len() == PROVIDER_RESPONSE_BYTE_CAP {
            return Err(OllamaHttpError::ResponseTooLarge {
                limit_bytes: PROVIDER_RESPONSE_BYTE_CAP,
            });
        }
        response.extend_from_slice(&buffer[..read]);
    }
    deadline.remaining("response parsing")?;
    parse_http_response(&response)
}

fn write_with_deadline(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: &RequestDeadline,
    phase: &str,
) -> Result<(), OllamaHttpError> {
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(deadline.remaining(phase)?))
            .map_err(|error| map_io_error("could not set write timeout", error))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(OllamaHttpError::Transport(format!(
                    "could not write Ollama {phase}: socket wrote zero bytes"
                )))
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(map_io_error(
                    &format!("could not write Ollama {phase}"),
                    error,
                ))
            }
        }
    }
    Ok(())
}

fn parse_chat_response(
    response: OllamaHttpResponse,
    latency_ms: u64,
    base_url: &str,
    request: &ModelRequest,
) -> Result<ModelResponse, ModelError> {
    if response.status_code() < 200 || response.status_code() >= 300 {
        return Err(http_status_error(response, base_url, request));
    }

    let raw: Value = serde_json::from_str(response.body()).map_err(|error| {
        ollama_provider_error(
            format!(
                "Ollama returned a non-JSON response from /api/chat: {error}. Verify --base-url points to the Ollama API."
            ),
            false,
            base_url,
            &request.model,
        )
    })?;

    if let Some(provider_error) = raw.get("error").and_then(Value::as_str) {
        return Err(ollama_provider_error(
            format!("Ollama returned an error: {provider_error}"),
            false,
            base_url,
            &request.model,
        ));
    }

    let content = raw
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ollama_provider_error(
                "Ollama response did not include message.content",
                false,
                base_url,
                &request.model,
            )
        })?
        .to_string();

    if request.response_schema.is_some() && serde_json::from_str::<Value>(&content).is_err() {
        return Err(ModelError::provider(
            "Ollama returned non-JSON model content for a structured request; verify the model supports structured outputs and retry.",
            false,
            json!({
                "provider": "ollama",
                "base_url": base_url,
                "model": request.model,
                "content_preview": content.chars().take(200).collect::<String>(),
            }),
        ));
    }

    Ok(ModelResponse {
        content,
        latency_ms,
        raw_provider_metadata: raw,
    })
}

fn http_status_error(
    response: OllamaHttpResponse,
    base_url: &str,
    request: &ModelRequest,
) -> ModelError {
    let provider_message = ollama_error_message(response.body())
        .unwrap_or_else(|| response.body().chars().take(300).collect::<String>());
    let metadata = json!({
        "provider": "ollama",
        "base_url": base_url,
        "model": request.model,
        "status_code": response.status_code(),
        "provider_message": provider_message,
    });

    if looks_like_missing_model(&provider_message) {
        return ModelError::provider(
            format!(
                "Ollama model is not installed: '{}'. Run `ollama pull {}` and retry. Provider response: {}",
                request.model, request.model, provider_message
            ),
            false,
            metadata,
        );
    }

    ModelError::provider(
        http_status_message(response.status_code(), &provider_message),
        response.status_code() >= 500,
        metadata,
    )
}

fn http_status_message(status_code: u16, provider_message: &str) -> String {
    if status_code == 404 {
        return format!(
            "Ollama /api/chat returned HTTP 404. Verify the base URL points to the Ollama API root, for example {DEFAULT_OLLAMA_BASE_URL}. Provider response: {provider_message}"
        );
    }

    format!("Ollama /api/chat returned HTTP {status_code}. Provider response: {provider_message}")
}

fn http_error_to_model_error(
    error: OllamaHttpError,
    base_url: &str,
    request: &ModelRequest,
) -> ModelError {
    match error {
        OllamaHttpError::ConnectionRefused(message) => ModelError::provider(
            format!(
                "Ollama is not reachable at {base_url}. Start it with `ollama serve`, verify the base URL, then retry. Transport error: {message}"
            ),
            true,
            json!({
                "provider": "ollama",
                "base_url": base_url,
                "model": request.model,
            }),
        ),
        OllamaHttpError::Timeout(message) => ModelError::timeout(
            format!(
                "Ollama request timed out after {} ms. Use --timeout-ms to allow more time or try a smaller model. Transport error: {message}",
                request.timeout_ms
            ),
            request.timeout_ms,
            json!({
                "provider": "ollama",
                "base_url": base_url,
                "model": request.model,
            }),
        ),
        OllamaHttpError::Transport(message) => ollama_provider_error(
            format!("Ollama transport failed: {message}"),
            true,
            base_url,
            &request.model,
        ),
        OllamaHttpError::InvalidAddress(message) => ollama_provider_error(
            format!("Ollama address is invalid: {message}"),
            false,
            base_url,
            &request.model,
        ),
        OllamaHttpError::ResponseTooLarge { limit_bytes } => ModelError::provider(
            format!("Ollama raw HTTP response exceeded the {limit_bytes}-byte limit"),
            false,
            json!({
                "provider": "ollama",
                "base_url": base_url,
                "model": request.model,
                "limit_bytes": limit_bytes,
            }),
        ),
    }
}

fn ollama_chat_url(base_url: &str) -> Result<String, ModelError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    if !trimmed.starts_with("http://") || trimmed.len() == "http://".len() {
        return Err(ModelError::provider(
            format!(
                "unsupported Ollama base URL '{base_url}'; expected an http:// URL such as {DEFAULT_OLLAMA_BASE_URL}"
            ),
            false,
            json!({
                "provider": "ollama",
                "base_url": base_url,
            }),
        ));
    }

    let chat_url = format!("{trimmed}/chat");
    validate_local_ollama_endpoint(&chat_url).map_err(|error| {
        ModelError::provider(
            format!("invalid Ollama endpoint: {error}"),
            false,
            json!({
                "provider": "ollama",
                "base_url": base_url,
            }),
        )
    })?;
    Ok(chat_url)
}

fn ollama_provider_error(
    message: impl Into<String>,
    retryable: bool,
    base_url: &str,
    model: &str,
) -> ModelError {
    ModelError::provider(
        message,
        retryable,
        json!({
            "provider": "ollama",
            "base_url": base_url,
            "model": model,
        }),
    )
}

fn ollama_role(role: ModelMessageRole) -> &'static str {
    match role {
        ModelMessageRole::User => "user",
        ModelMessageRole::Assistant => "assistant",
    }
}

fn ollama_error_message(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn looks_like_missing_model(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("model") && (lowered.contains("not found") || lowered.contains("pull"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedHttpUrl {
    host: String,
    host_header: String,
    port: u16,
    path: String,
}

fn parse_http_url(url: &str) -> Result<ParsedHttpUrl, OllamaHttpError> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        OllamaHttpError::InvalidAddress(format!(
            "unsupported Ollama base URL '{url}'; only http:// URLs are supported"
        ))
    })?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(OllamaHttpError::InvalidAddress(
            "Ollama URL must include a host".to_string(),
        ));
    }

    let (host, port, bracketed) = if let Some(authority) = authority.strip_prefix('[') {
        let closing = authority.find(']').ok_or_else(|| {
            OllamaHttpError::InvalidAddress(
                "invalid Ollama IPv6 URL authority; missing closing ']'".to_string(),
            )
        })?;
        let host = &authority[..closing];
        let remainder = &authority[closing + 1..];
        let port = if remainder.is_empty() {
            80
        } else {
            remainder
                .strip_prefix(':')
                .ok_or_else(|| {
                    OllamaHttpError::InvalidAddress(
                        "invalid Ollama IPv6 URL authority after ']'".to_string(),
                    )
                })?
                .parse::<u16>()
                .map_err(|error| {
                    OllamaHttpError::InvalidAddress(format!(
                        "invalid Ollama URL port in '{remainder}': {error}"
                    ))
                })?
        };
        (host.to_string(), port, true)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => {
                let parsed_port = port.parse::<u16>().map_err(|error| {
                    OllamaHttpError::InvalidAddress(format!(
                        "invalid Ollama URL port '{port}': {error}"
                    ))
                })?;
                (host.to_string(), parsed_port, false)
            }
            Some(_) => {
                return Err(OllamaHttpError::InvalidAddress(
                    "numeric IPv6 Ollama URLs must enclose the address in '[' and ']'".to_string(),
                ))
            }
            None => (authority.to_string(), 80, false),
        }
    };
    if host.is_empty() {
        return Err(OllamaHttpError::InvalidAddress(
            "Ollama URL must include a host".to_string(),
        ));
    }

    Ok(ParsedHttpUrl {
        host_header: if bracketed {
            if port == 80 {
                format!("[{host}]")
            } else {
                format!("[{host}]:{port}")
            }
        } else if port == 80 {
            host.clone()
        } else {
            format!("{host}:{port}")
        },
        host,
        port,
        path: path.to_string(),
    })
}

fn validate_local_ollama_endpoint(url: &str) -> Result<ParsedHttpUrl, OllamaHttpError> {
    let parsed = parse_http_url(url)?;
    local_socket_addresses(&parsed.host, parsed.port)?;
    Ok(parsed)
}

fn parse_http_response(response: &[u8]) -> Result<OllamaHttpResponse, OllamaHttpError> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| {
            OllamaHttpError::Transport(
                "Ollama returned an invalid HTTP response without headers".to_string(),
            )
        })?;
    let headers = std::str::from_utf8(&response[..header_end]).map_err(|error| {
        OllamaHttpError::Transport(format!("Ollama returned non-UTF-8 HTTP headers: {error}"))
    })?;
    let status_code = parse_status_code(headers)?;
    let body_bytes = &response[header_end + 4..];
    let body = if response_is_chunked(headers) {
        decode_chunked_body(body_bytes)?
    } else {
        body_bytes.to_vec()
    };
    let body = String::from_utf8(body).map_err(|error| {
        OllamaHttpError::Transport(format!("Ollama returned non-UTF-8 response body: {error}"))
    })?;

    Ok(OllamaHttpResponse::new(status_code, body))
}

fn parse_status_code(headers: &str) -> Result<u16, OllamaHttpError> {
    let status_line = headers.lines().next().ok_or_else(|| {
        OllamaHttpError::Transport("Ollama returned an empty HTTP response".to_string())
    })?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| {
            OllamaHttpError::Transport(format!(
                "Ollama returned an invalid HTTP status line: {status_line}"
            ))
        })?
        .parse::<u16>()
        .map_err(|error| {
            OllamaHttpError::Transport(format!(
                "Ollama returned an invalid HTTP status code: {error}"
            ))
        })?;
    Ok(status_code)
}

fn response_is_chunked(headers: &str) -> bool {
    headers.lines().any(|line| {
        let lowered = line.to_ascii_lowercase();
        lowered.starts_with("transfer-encoding:") && lowered.contains("chunked")
    })
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>, OllamaHttpError> {
    let mut decoded = Vec::new();
    let mut cursor = 0;

    loop {
        let line_end = find_crlf(body, cursor).ok_or_else(|| {
            OllamaHttpError::Transport("Ollama returned an invalid chunked response".to_string())
        })?;
        let size_line = std::str::from_utf8(&body[cursor..line_end]).map_err(|error| {
            OllamaHttpError::Transport(format!("Ollama returned an invalid chunk size: {error}"))
        })?;
        let size_hex = size_line.split(';').next().unwrap_or(size_line).trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|error| {
            OllamaHttpError::Transport(format!("Ollama returned an invalid chunk size: {error}"))
        })?;
        cursor = line_end.checked_add(2).ok_or_else(|| {
            OllamaHttpError::Transport("Ollama chunk cursor overflowed".to_string())
        })?;

        if size == 0 {
            let suffix_end = cursor.checked_add(2).ok_or_else(|| {
                OllamaHttpError::Transport("Ollama chunk suffix overflowed".to_string())
            })?;
            if body.get(cursor..suffix_end) != Some(b"\r\n") {
                return Err(OllamaHttpError::Transport(
                    "Ollama returned an invalid final chunk terminator".to_string(),
                ));
            }
            break;
        }
        let chunk_end = cursor.checked_add(size).ok_or_else(|| {
            OllamaHttpError::Transport("Ollama chunk size overflowed".to_string())
        })?;
        let suffix_end = chunk_end.checked_add(2).ok_or_else(|| {
            OllamaHttpError::Transport("Ollama chunk suffix overflowed".to_string())
        })?;
        if suffix_end > body.len() {
            return Err(OllamaHttpError::Transport(
                "Ollama returned a truncated chunked response".to_string(),
            ));
        }
        let chunk = body.get(cursor..chunk_end).ok_or_else(|| {
            OllamaHttpError::Transport("Ollama returned an invalid chunk range".to_string())
        })?;
        decoded.extend_from_slice(chunk);
        if body.get(chunk_end..suffix_end) != Some(b"\r\n") {
            return Err(OllamaHttpError::Transport(
                "Ollama returned an invalid chunk terminator".to_string(),
            ));
        }
        cursor = suffix_end;
    }

    Ok(decoded)
}

fn find_crlf(body: &[u8], start: usize) -> Option<usize> {
    body.get(start..)?
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|position| start + position)
}

fn map_connect_error(error: std::io::Error) -> OllamaHttpError {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => {
            OllamaHttpError::ConnectionRefused(error.to_string())
        }
        std::io::ErrorKind::TimedOut => OllamaHttpError::Timeout(error.to_string()),
        _ => OllamaHttpError::Transport(format!("could not connect to Ollama: {error}")),
    }
}

fn map_io_error(context: &str, error: std::io::Error) -> OllamaHttpError {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => {
            OllamaHttpError::ConnectionRefused(error.to_string())
        }
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
            OllamaHttpError::Timeout(error.to_string())
        }
        _ => OllamaHttpError::Transport(format!("{context}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, TcpListener},
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn std_http_client_tries_later_resolved_addresses_after_first_refuses() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .expect("bind ipv4 test listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            read_http_request(&mut stream);
            let body = r#"{"message":{"role":"assistant","content":"ok"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let client = StdOllamaHttpClient::with_resolver(Arc::new(StaticResolver {
            addresses: vec![
                SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, port, 0, 0)),
                SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)),
            ],
        }));
        let request = OllamaHttpRequest::new(
            "POST".to_string(),
            format!("http://localhost:{port}/api/chat"),
            json!({ "model": "test", "messages": [], "stream": false }),
        );

        let response = client.send(request, 2_000).expect("fallback response");

        assert_eq!(response.status_code(), 200);
        assert!(response.body().contains("\"content\":\"ok\""));
        server.join().expect("server thread");
    }

    #[test]
    fn std_http_client_slow_trickle_cannot_extend_the_request_deadline() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .expect("bind slow response listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            read_http_request(&mut stream);
            let body = r#"{"message":{"role":"assistant","content":"ok"}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            for byte in response.bytes() {
                if stream.write_all(&[byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        let client = StdOllamaHttpClient::with_resolver(Arc::new(StaticResolver {
            addresses: vec![address],
        }));
        let request = OllamaHttpRequest::new(
            "POST".to_string(),
            format!("http://localhost:{}/api/chat", address.port()),
            json!({ "model": "test", "messages": [], "stream": false }),
        );

        let started = Instant::now();
        let error = client
            .send(request, 75)
            .expect_err("trickled response must not reset the absolute deadline");
        let elapsed = started.elapsed();

        assert!(matches!(error, OllamaHttpError::Timeout(_)));
        assert!(
            elapsed < Duration::from_millis(400),
            "75 ms request deadline took {elapsed:?}"
        );
        server.join().expect("server thread");
    }

    #[test]
    fn std_http_client_rejects_response_before_accumulating_past_provider_evidence_limit() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .expect("bind oversized response listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            read_http_request(&mut stream);
            let body = format!(
                r#"{{"message":{{"role":"assistant","content":"{}"}}}}"#,
                "x".repeat(PROVIDER_RESPONSE_BYTE_CAP)
            );
            let response_headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response_headers.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        });
        let client = StdOllamaHttpClient::with_resolver(Arc::new(StaticResolver {
            addresses: vec![address],
        }));
        let request = OllamaHttpRequest::new(
            "POST".to_string(),
            format!("http://localhost:{}/api/chat", address.port()),
            json!({ "model": "test", "messages": [], "stream": false }),
        );

        let error = client
            .send(request, 2_000)
            .expect_err("oversized response must fail at the transport boundary");

        assert_eq!(
            error,
            OllamaHttpError::ResponseTooLarge {
                limit_bytes: PROVIDER_RESPONSE_BYTE_CAP
            }
        );
        server.join().expect("server thread");
    }

    #[test]
    fn std_resolver_accepts_only_localhost_and_numeric_addresses_without_dns() {
        let resolver = StdOllamaAddressResolver;

        assert_eq!(resolver.resolve("localhost", 11434).unwrap().len(), 2);
        assert_eq!(
            resolver.resolve("127.0.0.1", 11434).unwrap(),
            vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 11434)]
        );
        assert_eq!(
            resolver.resolve("::1", 11434).unwrap(),
            vec![SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 11434)]
        );
        let error = resolver
            .resolve("ollama.internal.example", 11434)
            .unwrap_err();
        assert!(matches!(error, OllamaHttpError::InvalidAddress(_)));
        assert!(error.to_string().contains("localhost or a numeric"));

        let parsed = parse_http_url("http://[::1]:11434/api/chat").unwrap();
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.host_header, "[::1]:11434");
    }

    #[test]
    fn chunked_response_with_maximum_or_truncated_size_never_panics_or_allocates() {
        for chunked_body in [
            b"ffffffffffffffff\r\n".as_slice(),
            b"7fffffffffffffff\r\nx\r\n".as_slice(),
            b"4\r\nab\r\n".as_slice(),
        ] {
            let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            response.extend_from_slice(chunked_body);

            let error = parse_http_response(&response).unwrap_err();

            assert!(matches!(error, OllamaHttpError::Transport(_)));
        }
    }

    #[test]
    fn std_http_client_shares_one_connect_deadline_across_all_addresses() {
        let timeouts = Arc::new(Mutex::new(Vec::new()));
        let connector = Arc::new(BudgetConnector {
            timeouts: Arc::clone(&timeouts),
        });
        let client = StdOllamaHttpClient::with_transport(
            Arc::new(StaticResolver {
                addresses: vec![
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1),
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 2),
                ],
            }),
            connector,
        );
        let request = OllamaHttpRequest::new(
            "POST".to_string(),
            "http://localhost:11434/api/chat".to_string(),
            json!({ "model": "test", "messages": [], "stream": false }),
        );

        let started = Instant::now();
        let error = client.send(request, 100).unwrap_err();
        let elapsed = started.elapsed();
        let timeouts = timeouts.lock().unwrap();

        assert!(matches!(error, OllamaHttpError::Timeout(_)));
        assert_eq!(timeouts.len(), 2);
        assert!(timeouts[1] < timeouts[0], "timeouts were {timeouts:?}");
        assert!(
            elapsed < Duration::from_millis(180),
            "shared 100 ms deadline took {elapsed:?}"
        );
    }

    fn read_http_request(stream: &mut std::net::TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0; 256];
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            assert!(read > 0, "client closed before sending request");
            request.extend_from_slice(&buffer[..read]);
            if request_is_complete(&request) {
                return;
            }
        }
    }

    fn request_is_complete(request: &[u8]) -> bool {
        let Some(header_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
        else {
            return false;
        };
        let headers = std::str::from_utf8(&request[..header_end]).expect("request headers utf8");
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .expect("content length")
            .parse::<usize>()
            .expect("content length number");

        request.len() >= header_end + content_length
    }

    struct StaticResolver {
        addresses: Vec<SocketAddr>,
    }

    impl OllamaAddressResolver for StaticResolver {
        fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<SocketAddr>, OllamaHttpError> {
            Ok(self.addresses.clone())
        }
    }

    struct BudgetConnector {
        timeouts: Arc<Mutex<Vec<Duration>>>,
    }

    impl OllamaConnector for BudgetConnector {
        fn connect(
            &self,
            _address: &SocketAddr,
            timeout: Duration,
        ) -> Result<TcpStream, std::io::Error> {
            let attempt = {
                let mut timeouts = self.timeouts.lock().unwrap();
                timeouts.push(timeout);
                timeouts.len()
            };
            if attempt == 1 {
                thread::sleep(Duration::from_millis(60));
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "first address refused",
                ))
            } else {
                thread::sleep(timeout);
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "second address timed out",
                ))
            }
        }
    }
}
