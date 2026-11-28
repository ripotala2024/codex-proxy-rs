use cpr_protocol_bridge_core::{
    anthropic::{MessagesModelCapabilities, responses_to_messages_request},
    genai::messages_to_genai_request,
};
use serde_json::json;

#[test]
fn messages_request_maps_to_genai() {
    let request = json!({
        "model": "gemini-2.5-pro",
        "max_tokens": 128,
        "messages": [{"role": "user", "content": "hello"}]
    });
    let messages = request.as_object().expect("object");
    let genai = messages_to_genai_request(messages).expect("supported request");
    assert_eq!(genai["contents"][0]["role"], "user");
    assert_eq!(genai["contents"][0]["parts"][0]["text"], "hello");
}

#[test]
fn responses_request_maps_to_messages() {
    let request = json!({
        "model": "gemini-2.5-pro",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "hello"}]}],
        "max_output_tokens": 128
    });
    let messages = responses_to_messages_request(
        request.as_object().expect("object"),
        &MessagesModelCapabilities::default(),
    )
    .expect("supported request");
    assert_eq!(messages["messages"][0]["role"], "user");
    assert_eq!(messages["messages"][0]["content"][0]["text"], "hello");
}
