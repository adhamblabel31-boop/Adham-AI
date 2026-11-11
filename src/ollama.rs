use iced::futures::{SinkExt, Stream, StreamExt};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(super) const BASE_URL: &str = "http://127.0.0.1:11434";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OllamaError {
    Unavailable,
    RequestTimeout,
    TransportFailure,
    HttpFailure(u16),
    ModelNotFound,
    NoModels,
    InvalidResponse,
    EmptyResponse,
    StreamInterrupted,
}

impl OllamaError {
    pub(super) fn user_message(&self) -> &'static str {
        match self {
            Self::Unavailable => {
                "Cannot connect to local Ollama. Check that Ollama is running and try again."
            }
            Self::RequestTimeout => {
                "Ollama stopped sending data for too long. Partial text was kept; you can retry."
            }
            Self::TransportFailure => {
                "The connection to Ollama failed. Partial text was kept; you can retry."
            }
            Self::HttpFailure(_) => "Ollama could not complete that request. Please try again.",
            Self::ModelNotFound => {
                "The selected model is unavailable. Refresh the model list and choose an installed model."
            }
            Self::NoModels => "No models are available from the local Ollama server.",
            Self::InvalidResponse => "Ollama returned a response this app could not read.",
            Self::EmptyResponse => {
                "The selected model returned an empty response. Please try again."
            }
            Self::StreamInterrupted => {
                "The Ollama connection ended before generation completed. Partial text was kept; you can retry."
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChatEvent {
    ThinkingActivity,
    Chunk(String),
    Complete,
    Failed(OllamaError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ChatMessage {
    pub(super) role: String,
    pub(super) content: String,
}

#[derive(Clone)]
pub(super) struct OllamaClient {
    http: Client,
}

impl OllamaClient {
    pub(super) fn new() -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("valid static Ollama HTTP client configuration");

        Self { http }
    }

    pub(super) async fn list_models(self) -> Result<Vec<String>, OllamaError> {
        let endpoint = format!("{BASE_URL}/api/tags");
        let response = self.http.get(&endpoint).send().await.map_err(|error| {
            eprintln!("Ollama GET /api/tags transport failure: {error}");
            request_error(&error)
        })?;
        let bytes = read_success_body(response, "/api/tags").await?;
        parse_models(&bytes)
    }

    pub(super) async fn model_supports_thinking(self, model: String) -> Result<bool, OllamaError> {
        let response = self
            .http
            .post(format!("{BASE_URL}/api/show"))
            .json(&ShowRequest {
                model: &model,
                verbose: false,
            })
            .send()
            .await
            .map_err(|error| {
                eprintln!("Ollama POST /api/show transport failure: {error}");
                request_error(&error)
            })?;
        let bytes = read_success_body(response, "/api/show").await?;
        parse_model_thinking_capability(&bytes)
    }

    pub(super) fn chat_stream(
        self,
        model: String,
        messages: Vec<ChatMessage>,
        think: bool,
    ) -> impl Stream<Item = ChatEvent> + 'static {
        iced::stream::channel(1, async move |mut sender| {
            let response = match self
                .http
                .post(format!("{BASE_URL}/api/chat"))
                .json(&ChatRequest {
                    model: &model,
                    messages: &messages,
                    stream: true,
                    think: think.then_some(true),
                })
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    eprintln!("Ollama POST /api/chat transport failure: {error}");
                    let _ = sender.send(ChatEvent::Failed(request_error(&error))).await;
                    return;
                }
            };

            let status = response.status();
            if !status.is_success() {
                let result = match response.bytes().await {
                    Ok(bytes) => parse_http_error(status, &bytes, "/api/chat"),
                    Err(error) => {
                        eprintln!("Ollama POST /api/chat error body read failure: {error}");
                        if error.is_timeout() {
                            OllamaError::RequestTimeout
                        } else {
                            OllamaError::HttpFailure(status.as_u16())
                        }
                    }
                };
                let _ = sender.send(ChatEvent::Failed(result)).await;
                return;
            }

            let mut body = response.bytes_stream();
            let mut pending = Vec::new();
            while let Some(next) = body.next().await {
                let bytes = match next {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        eprintln!("Ollama POST /api/chat stream interrupted: {error}");
                        let _ = sender.send(ChatEvent::Failed(stream_error(&error))).await;
                        return;
                    }
                };

                pending.extend_from_slice(&bytes);
                while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                    let line: Vec<_> = pending.drain(..=newline).collect();
                    match parse_stream_chunk(&line[..line.len() - 1]) {
                        Ok(Some(chunk)) => {
                            let done = chunk.done;
                            if !send_parsed_chunk(&mut sender, chunk).await {
                                return;
                            }
                            if done {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = sender.send(ChatEvent::Failed(error)).await;
                            return;
                        }
                    }
                }
            }

            if !pending.is_empty() {
                match parse_stream_chunk(&pending) {
                    Ok(Some(chunk)) => {
                        let done = chunk.done;
                        if !send_parsed_chunk(&mut sender, chunk).await {
                            return;
                        }
                        if done {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = sender.send(ChatEvent::Failed(error)).await;
                        return;
                    }
                }
            }

            eprintln!("Ollama POST /api/chat stream ended without a final chunk");
            let _ = sender
                .send(ChatEvent::Failed(OllamaError::StreamInterrupted))
                .await;
        })
    }
}

async fn send_parsed_chunk(
    sender: &mut iced::futures::channel::mpsc::Sender<ChatEvent>,
    chunk: ParsedStreamChunk,
) -> bool {
    if chunk.thinking_activity && sender.send(ChatEvent::ThinkingActivity).await.is_err() {
        return false;
    }
    if let Some(content) = chunk.content
        && !content.is_empty()
        && sender.send(ChatEvent::Chunk(content)).await.is_err()
    {
        return false;
    }

    if chunk.done {
        return sender.send(ChatEvent::Complete).await.is_ok();
    }

    true
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
}

#[derive(Serialize)]
struct ShowRequest<'a> {
    model: &'a str,
    verbose: bool,
}

#[derive(Deserialize)]
struct ShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    name: String,
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    message: Option<StreamMessage>,
    #[serde(default)]
    done: bool,
    error: Option<String>,
}

#[derive(Deserialize)]
struct StreamMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: Option<String>,
}

#[derive(Debug)]
struct ParsedStreamChunk {
    content: Option<String>,
    thinking_activity: bool,
    done: bool,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: Option<String>,
}

async fn read_success_body(
    response: Response,
    endpoint: &'static str,
) -> Result<Vec<u8>, OllamaError> {
    let status = response.status();
    let bytes = response.bytes().await.map_err(|error| {
        eprintln!("Ollama GET {endpoint} response read failure: {error}");
        response_error(&error)
    })?;

    if !status.is_success() {
        return Err(parse_http_error(status, &bytes, endpoint));
    }

    Ok(bytes.to_vec())
}

fn parse_models(bytes: &[u8]) -> Result<Vec<String>, OllamaError> {
    let response: TagsResponse = serde_json::from_slice(bytes).map_err(|error| {
        eprintln!("Ollama /api/tags returned malformed JSON: {error}");
        OllamaError::InvalidResponse
    })?;

    let models: Vec<_> = response
        .models
        .into_iter()
        .map(|entry| entry.name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();

    if models.is_empty() {
        return Err(OllamaError::NoModels);
    }

    Ok(models)
}

fn parse_stream_chunk(bytes: &[u8]) -> Result<Option<ParsedStreamChunk>, OllamaError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }

    let chunk: StreamChunk = serde_json::from_slice(bytes).map_err(|error| {
        eprintln!("Ollama /api/chat stream returned malformed JSON: {error}");
        OllamaError::InvalidResponse
    })?;

    if let Some(detail) = chunk.error {
        eprintln!("Ollama /api/chat stream error: {detail}");
        return Err(if is_model_not_found(&detail) {
            OllamaError::ModelNotFound
        } else {
            OllamaError::HttpFailure(StatusCode::INTERNAL_SERVER_ERROR.as_u16())
        });
    }

    let (content, thinking_activity) = chunk.message.map_or((None, false), |message| {
        (
            (!message.content.is_empty()).then_some(message.content),
            message
                .thinking
                .is_some_and(|thinking| !thinking.is_empty()),
        )
    });
    if content.is_none() && !thinking_activity && !chunk.done {
        eprintln!("Ollama /api/chat stream chunk had neither content nor completion");
        return Err(OllamaError::InvalidResponse);
    }

    Ok(Some(ParsedStreamChunk {
        content,
        thinking_activity,
        done: chunk.done,
    }))
}

fn parse_model_thinking_capability(bytes: &[u8]) -> Result<bool, OllamaError> {
    let response: ShowResponse = serde_json::from_slice(bytes).map_err(|error| {
        eprintln!("Ollama /api/show returned malformed JSON: {error}");
        OllamaError::InvalidResponse
    })?;
    Ok(response
        .capabilities
        .iter()
        .any(|capability| capability == "thinking"))
}

fn parse_http_error(status: StatusCode, bytes: &[u8], endpoint: &str) -> OllamaError {
    let detail = serde_json::from_slice::<ErrorResponse>(bytes)
        .ok()
        .and_then(|response| response.error);
    eprintln!("Ollama {endpoint} returned HTTP {status}: {detail:?}");

    if status == StatusCode::NOT_FOUND && detail.as_deref().is_some_and(is_model_not_found) {
        OllamaError::ModelNotFound
    } else {
        OllamaError::HttpFailure(status.as_u16())
    }
}

fn request_error(error: &reqwest::Error) -> OllamaError {
    if error.is_timeout() {
        OllamaError::RequestTimeout
    } else if error.is_connect() {
        OllamaError::Unavailable
    } else {
        OllamaError::TransportFailure
    }
}

fn stream_error(error: &reqwest::Error) -> OllamaError {
    if error.is_timeout() {
        OllamaError::RequestTimeout
    } else {
        OllamaError::StreamInterrupted
    }
}

fn response_error(error: &reqwest::Error) -> OllamaError {
    if error.is_timeout() {
        OllamaError::RequestTimeout
    } else {
        OllamaError::TransportFailure
    }
}

fn is_model_not_found(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("model") && message.contains("not found")
}

#[cfg(test)]
mod tests {
    use super::{
        ChatMessage, ChatRequest, OllamaError, parse_http_error, parse_model_thinking_capability,
        parse_models, parse_stream_chunk,
    };
    use reqwest::StatusCode;

    #[test]
    fn parses_available_model_names() {
        assert_eq!(
            parse_models(br#"{"models":[{"name":"qwen3:4b"},{"name":"llama3:8b"}]}"#),
            Ok(vec!["qwen3:4b".into(), "llama3:8b".into()])
        );
    }

    #[test]
    fn rejects_empty_and_malformed_model_lists() {
        assert_eq!(
            parse_models(br#"{"models":[]}"#),
            Err(OllamaError::NoModels)
        );
        assert_eq!(parse_models(b"not json"), Err(OllamaError::InvalidResponse));
    }

    #[test]
    fn parses_stream_content_and_completion_chunks() {
        let chunk = parse_stream_chunk(
            br#"{"message":{"role":"assistant","content":"Hello"},"done":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chunk.content.as_deref(), Some("Hello"));
        assert!(!chunk.thinking_activity);
        assert!(!chunk.done);

        let final_chunk = parse_stream_chunk(br#"{"done":true}"#).unwrap().unwrap();
        assert!(final_chunk.content.is_none());
        assert!(!final_chunk.thinking_activity);
        assert!(final_chunk.done);
        assert!(parse_stream_chunk(b" \r\n").unwrap().is_none());
    }

    #[test]
    fn separates_thinking_activity_from_user_facing_answer_content() {
        let thinking = parse_stream_chunk(
            br#"{"message":{"role":"assistant","thinking":"private reasoning","content":""},"done":false}"#,
        )
        .unwrap()
        .unwrap();
        assert!(thinking.content.is_none());
        assert!(thinking.thinking_activity);

        let answer = parse_stream_chunk(
            br#"{"message":{"role":"assistant","thinking":"more private reasoning","content":"The answer."},"done":false}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(answer.content.as_deref(), Some("The answer."));
        assert!(answer.thinking_activity);
    }

    #[test]
    fn detects_thinking_only_from_reported_model_capabilities() {
        assert!(
            parse_model_thinking_capability(
                br#"{"capabilities":["completion","tools","thinking"]}"#
            )
            .unwrap()
        );
        assert!(
            !parse_model_thinking_capability(br#"{"capabilities":["completion","tools"]}"#)
                .unwrap()
        );
        assert_eq!(
            parse_model_thinking_capability(b"not json"),
            Err(OllamaError::InvalidResponse)
        );
    }

    #[test]
    fn rejects_malformed_or_incomplete_stream_chunks() {
        assert_eq!(
            parse_stream_chunk(b"{not json").unwrap_err(),
            OllamaError::InvalidResponse
        );
        assert_eq!(
            parse_stream_chunk(br#"{"done":false}"#).unwrap_err(),
            OllamaError::InvalidResponse
        );
        assert_eq!(
            parse_stream_chunk(br#"{"error":"model missing not found"}"#).unwrap_err(),
            OllamaError::ModelNotFound
        );
    }

    #[test]
    fn maps_http_errors_without_exposing_server_text_to_ui() {
        assert_eq!(
            parse_http_error(
                StatusCode::NOT_FOUND,
                br#"{"error":"model 'missing:latest' not found"}"#,
                "/api/chat",
            ),
            OllamaError::ModelNotFound
        );
        assert_eq!(
            parse_http_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                br#"{"error":"runner failed"}"#,
                "/api/chat",
            ),
            OllamaError::HttpFailure(500)
        );
        assert!(
            OllamaError::ModelNotFound
                .user_message()
                .contains("Refresh the model list")
        );
    }

    #[test]
    fn request_failure_categories_have_distinct_user_messages() {
        assert_ne!(
            OllamaError::Unavailable.user_message(),
            OllamaError::RequestTimeout.user_message()
        );
        assert_ne!(
            OllamaError::RequestTimeout.user_message(),
            OllamaError::StreamInterrupted.user_message()
        );
        assert_ne!(
            OllamaError::TransportFailure.user_message(),
            OllamaError::InvalidResponse.user_message()
        );
    }

    #[test]
    fn chat_request_uses_ollama_streaming_message_format() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Hello".into(),
        }];
        let body = serde_json::to_value(ChatRequest {
            model: "qwen3:4b",
            messages: &messages,
            stream: true,
            think: None,
        })
        .unwrap();

        assert_eq!(body["model"], "qwen3:4b");
        assert_eq!(body["stream"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Hello");
        assert!(body.get("think").is_none());
    }

    #[test]
    fn deep_think_request_uses_ollamas_supported_think_flag() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Think carefully".into(),
        }];
        let body = serde_json::to_value(ChatRequest {
            model: "qwen3:4b",
            messages: &messages,
            stream: true,
            think: Some(true),
        })
        .unwrap();
        assert_eq!(body["model"], "qwen3:4b");
        assert_eq!(body["think"], true);
    }
}
