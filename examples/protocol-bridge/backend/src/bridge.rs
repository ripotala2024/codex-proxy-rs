use cpr_protocol_bridge_core::{
    anthropic::{
        MessagesAccumulator, MessagesModelCapabilities, MessagesToResponsesStream,
        TranslationError, responses_to_messages_request, validate_request,
    },
    genai::{GenAiToMessagesStream, messages_to_genai_request},
};
use gateway_plugin_sdk::{
    ErrorCode, PluginFault,
    call::{
        host::{
            MODEL_EXECUTE_STREAM, MODEL_STREAM_CLOSE, MODEL_STREAM_READ, ModelEventBatch,
            ModelExecuteRequest, ModelOperation, ModelStreamCloseRequest, ModelStreamReadRequest,
            ModelStreamReadResult, ModelStreamResult,
        },
        model::{ExecutionEvent, WirePayload},
    },
    client::{
        ComposedPlugin, HttpBody, HttpCall, HttpFrame, HttpResponse, MiddlewareCall,
        MiddlewareResult, PluginBuilder,
    },
};
use serde_json::{Map, Value, json};

const SOURCE_MESSAGES: &str = "messages";
const SOURCE_RESPONSES: &str = "responses";
const TARGET_GENAI: &str = "genai";
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

pub fn plugin() -> Result<ComposedPlugin, gateway_plugin_sdk::client::AuthorError> {
    PluginBuilder::from_json(include_bytes!("../../plugin.json"))?
        .middleware::<MiddlewareCall, _, _>(handle_call)?
        .build()
}

async fn handle_call(call: MiddlewareCall) -> Result<MiddlewareResult, PluginFault> {
    match call {
        MiddlewareCall::Http(call) => handle_http(*call).await.map(MiddlewareResult::Http),
        other => other.forward().await,
    }
}

async fn handle_http(mut call: HttpCall) -> Result<HttpResponse, PluginFault> {
    let Some(source) = source_for_uri(&call.request.uri) else {
        return call.next.run(call.request).await;
    };
    if call.request.method != "POST" {
        return call.next.run(call.request).await;
    }

    let body = collect_body(&mut call.request.body, MAX_BODY_BYTES).await?;
    let request = parse_object(&body)?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| invalid_request("model must be a non-empty string"))?
        .to_owned();
    if request
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
    {
        return Err(invalid_request(
            "cross-protocol previous_response_id is not supported yet",
        ));
    }

    let authorization = authorization_header(&call.request.headers)?;
    let client = call
        .host
        .verify_client(gateway_plugin_sdk::call::data::VerifyClientRequest { authorization })
        .await?;

    let messages = match source {
        SOURCE_MESSAGES => {
            validate_request(&request).map_err(bridge_error)?;
            request.clone()
        }
        SOURCE_RESPONSES => {
            responses_to_messages_request(&request, &MessagesModelCapabilities::default())
                .map_err(bridge_error)?
        }
        _ => unreachable!(),
    };
    let target = messages_to_genai_request(&messages).map_err(bridge_error)?;
    let target_body = serde_json::to_vec(&target)
        .map_err(|_| invalid_request("target request encoding failed"))?;
    let stream = execute_genai(&call, client.client_key_id, model.clone(), target_body).await?;

    let output = translate_stream(&call.host, stream, source, &request, model).await?;
    let requested_stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if requested_stream {
        Ok(sse_response(output))
    } else {
        Ok(json_response(output, source, &request))
    }
}

fn source_for_uri(uri: &str) -> Option<&'static str> {
    let path = uri.split('?').next().unwrap_or(uri);
    match path {
        "/v1/messages" => Some(SOURCE_MESSAGES),
        "/v1/responses" => Some(SOURCE_RESPONSES),
        _ => None,
    }
}

fn parse_object(body: &[u8]) -> Result<Map<String, Value>, PluginFault> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| invalid_request("request body must be JSON"))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_request("request body must be a JSON object"))
}

fn authorization_header(
    headers: &[gateway_plugin_sdk::call::middleware::MiddlewareHeader],
) -> Result<String, PluginFault> {
    let header = headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("authorization"))
        .ok_or_else(|| invalid_request("authorization header is required"))?;
    String::from_utf8(header.value.clone())
        .map_err(|_| invalid_request("authorization header is not valid UTF-8"))
}

async fn collect_body(body: &mut HttpBody, maximum: usize) -> Result<Vec<u8>, PluginFault> {
    let mut collected = Vec::new();
    while let Some(frame) = body.read().await? {
        match frame {
            HttpFrame::Data(bytes) => {
                if collected.len().saturating_add(bytes.len()) > maximum {
                    return Err(invalid_request("request body is too large"));
                }
                collected.extend(bytes);
            }
            HttpFrame::Trailers(_) => {}
        }
    }
    Ok(collected)
}

async fn execute_genai(
    call: &HttpCall,
    client_key_id: String,
    model: String,
    body: Vec<u8>,
) -> Result<ModelStreamResult, PluginFault> {
    let request = ModelExecuteRequest {
        client_key_id: Some(client_key_id),
        model,
        protocol: TARGET_GENAI.to_owned(),
        operation: ModelOperation::Generate,
        provider: None,
        account_id: None,
        previous_response_id: None,
    };
    let reply = call
        .host
        .call(
            MODEL_EXECUTE_STREAM,
            serde_json::to_value(request)
                .map_err(|_| invalid_request("model request encoding failed"))?,
            body,
        )
        .await
        .map_err(gateway_plugin_sdk::client::SessionError::into_plugin_fault)?;
    if !reply.payload.is_empty() {
        return Err(protocol_fault(
            "model stream callback returned unexpected payload",
        ));
    }
    serde_json::from_value(reply.result).map_err(|_| protocol_fault("invalid model stream handle"))
}

async fn translate_stream(
    host: &gateway_plugin_sdk::client::HostClient,
    stream: ModelStreamResult,
    source: &str,
    original_request: &Map<String, Value>,
    model: String,
) -> Result<Vec<Value>, PluginFault> {
    let mut genai = GenAiToMessagesStream::new(model);
    let mut responses =
        (source == SOURCE_RESPONSES).then(|| MessagesToResponsesStream::new(original_request));
    let mut collected = Vec::new();

    loop {
        let reply = read_model_stream(host, &stream.stream).await?;
        if !reply.payload.is_empty() {
            let batch = ModelEventBatch::decode(&reply.payload)
                .map_err(|_| protocol_fault("invalid model event batch"))?;
            for event in batch.events {
                let Some(data) = event_data(&event) else {
                    continue;
                };
                append_converted(&mut collected, &mut genai, responses.as_mut(), &data)?;
            }
        }
        if reply.end {
            let final_events = genai.finish().map_err(bridge_error)?;
            for event in final_events {
                append_response_event(&mut collected, responses.as_mut(), event)?;
            }
            break;
        }
    }

    let _ = close_model_stream(host, &stream.stream).await;
    Ok(collected)
}

async fn read_model_stream(
    host: &gateway_plugin_sdk::client::HostClient,
    stream: &str,
) -> Result<ModelStreamReadResultWithPayload, PluginFault> {
    let request = ModelStreamReadRequest {
        stream: stream.to_owned(),
        maximum_bytes: u32::try_from(MAX_EVENT_BYTES)
            .map_err(|_| protocol_fault("event size limit is invalid"))?,
    };
    let reply = host
        .call(
            MODEL_STREAM_READ,
            serde_json::to_value(request)
                .map_err(|_| protocol_fault("stream read request encoding failed"))?,
            Vec::new(),
        )
        .await
        .map_err(gateway_plugin_sdk::client::SessionError::into_plugin_fault)?;
    let result: ModelStreamReadResult = serde_json::from_value(reply.result)
        .map_err(|_| protocol_fault("invalid model stream read result"))?;
    if reply.payload.len() > MAX_EVENT_BYTES {
        return Err(protocol_fault("model event batch is too large"));
    }
    Ok(ModelStreamReadResultWithPayload {
        payload: reply.payload,
        end: result.end,
    })
}

async fn close_model_stream(
    host: &gateway_plugin_sdk::client::HostClient,
    stream: &str,
) -> Result<(), PluginFault> {
    let request = ModelStreamCloseRequest {
        stream: stream.to_owned(),
    };
    let reply = host
        .call(
            MODEL_STREAM_CLOSE,
            serde_json::to_value(request)
                .map_err(|_| protocol_fault("stream close request encoding failed"))?,
            Vec::new(),
        )
        .await
        .map_err(gateway_plugin_sdk::client::SessionError::into_plugin_fault)?;
    if !reply.payload.is_empty() {
        return Err(protocol_fault(
            "model stream close callback returned unexpected payload",
        ));
    }
    Ok(())
}

fn append_converted(
    collected: &mut Vec<Value>,
    genai: &mut GenAiToMessagesStream,
    mut responses: Option<&mut MessagesToResponsesStream>,
    data: &Value,
) -> Result<(), PluginFault> {
    let message_events = genai.push(data).map_err(bridge_error)?;
    for event in message_events {
        append_response_event(collected, responses.as_deref_mut(), event)?;
    }
    Ok(())
}

fn append_response_event(
    collected: &mut Vec<Value>,
    responses: Option<&mut MessagesToResponsesStream>,
    event: Value,
) -> Result<(), PluginFault> {
    if let Some(responses) = responses {
        collected.extend(responses.push(&event).map_err(bridge_error)?);
    } else {
        collected.push(event);
    }
    Ok(())
}

struct ModelStreamReadResultWithPayload {
    payload: Vec<u8>,
    end: bool,
}

fn event_data(event: &ExecutionEvent) -> Option<Value> {
    let wire = event.wire.as_ref()?;
    match &wire.payload {
        WirePayload::Json { data, .. } => Some(data.clone()),
        WirePayload::RawJson { body } => serde_json::from_slice(body).ok(),
        _ => None,
    }
}

fn sse_response(events: Vec<Value>) -> HttpResponse {
    let mut body = Vec::new();
    for event in events {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        body.extend_from_slice(format!("event: {event_type}\ndata: {}\n\n", event).as_bytes());
    }
    let mut response = HttpResponse::new(200, HttpBody::from_bytes(body));
    response
        .headers
        .push(header("content-type", "text/event-stream"));
    response.headers.push(header("cache-control", "no-cache"));
    response
}

fn json_response(
    events: Vec<Value>,
    source: &str,
    original_request: &Map<String, Value>,
) -> HttpResponse {
    let value = if source == SOURCE_RESPONSES {
        let mut converter = MessagesToResponsesStream::new(original_request);
        let mut message = Value::Null;
        let mut accumulator = MessagesAccumulator::default();
        for event in events {
            if let Ok(Some(value)) = accumulator.push(&event) {
                message = value;
            }
            let _ = converter.push(&event);
        }
        converter
            .complete_json_event(&message)
            .unwrap_or_else(|_| json!({}))
    } else {
        let mut accumulator = MessagesAccumulator::default();
        let mut message = Value::Null;
        for event in events {
            if let Ok(Some(value)) = accumulator.push(&event) {
                message = value;
            }
        }
        message
    };
    let mut response = HttpResponse::new(200, HttpBody::from_bytes(value.to_string().into_bytes()));
    response
        .headers
        .push(header("content-type", "application/json"));
    response
}

fn header(name: &str, value: &str) -> gateway_plugin_sdk::call::middleware::MiddlewareHeader {
    gateway_plugin_sdk::call::middleware::MiddlewareHeader {
        name: name.to_owned(),
        value: value.as_bytes().to_vec(),
    }
}

fn invalid_request(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::InvalidInput, message)
}

fn protocol_fault(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::Fault, message)
}

fn bridge_error(error: TranslationError) -> PluginFault {
    let code = match error {
        TranslationError::InvalidRequest(_) => ErrorCode::InvalidInput,
        TranslationError::InvalidEvent(_) | TranslationError::IncompleteStream => ErrorCode::Fault,
    };
    PluginFault::new(code, error.to_string())
}
