use super::*;

pub(super) fn decode_forward_response(data: &Value) -> Result<Vec<ForwardNode>> {
    let nodes = data.get("messages").or_else(|| data.get("message"))
        .and_then(Value::as_array)
        .ok_or_else(|| Error::action("get_forward_msg response is missing its message array"))?;
    nodes.iter().map(|node| {
        let body = node.get("data").unwrap_or(node);
        let sender = body.get("sender").unwrap_or(body);
        let user = Uin(data_i64(sender, "user_id")
            .or_else(|| data_i64(body, "user_id"))
            .or_else(|| data_i64(body, "uin")).unwrap_or(0));
        let name = data_str(sender, "card").filter(|s| !s.is_empty())
            .or_else(|| data_str(sender, "nickname"))
            .or_else(|| data_str(body, "nickname"))
            .or_else(|| data_str(body, "name")).unwrap_or_default();
        let value = body.get("content").or_else(|| body.get("message"))
            .filter(|v| v.is_array() || v.is_string())
            .ok_or_else(|| Error::action("get_forward_msg node is missing its content"))?;
        let content = crate::decode::decode_message_value(value, Peer::group(0));
        if content.is_empty() && value.as_array().is_some_and(|v| !v.is_empty()) {
            return Err(Error::action("get_forward_msg node content could not be decoded"));
        }
        Ok(ForwardNode { user, name, content, time: body.get("time").and_then(Value::as_i64) })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_response_napcat_aliases_and_sender() {
        let response: RespJson = serde_json::from_value(json!({
            "status":"ok", "retcode":0, "message":"", "wording":"", "msg":"",
            "data":{"messages":[{"sender":{"user_id":123,"nickname":"author"},
                "message":[{"type":"text","data":{"text":"retained text"}}]}]}
        })).unwrap();
        let nodes = decode_forward_response(&map_response("get_forward_msg", response).unwrap()).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].user, Uin(123));
        assert_eq!(nodes[0].name, "author");
        assert!(matches!(&nodes[0].content[0], Segment::Text(text) if text == "retained text"));
    }

    #[test]
    fn forward_response_legacy_nodes_and_error_aliases() {
        let nodes = decode_forward_response(&json!({"message":[{"type":"node","data":{
            "uin":"456","name":"legacy","content":[{"type":"text","data":{"text":"legacy text"}}]
        }}]})).unwrap();
        assert_eq!(nodes[0].user, Uin(456));
        assert_eq!(nodes[0].name, "legacy");
        let response: RespJson = serde_json::from_value(json!({
            "status":"failed","retcode":1400,"message":"","wording":"lookup failed"
        })).unwrap();
        assert_eq!(response.message.as_deref(), Some("lookup failed"));
        assert!(map_response("get_forward_msg", response).is_err());
        assert!(decode_forward_response(&Value::Null).is_err());
        assert!(decode_forward_response(&json!({"messages":[{}]})).is_err());
    }
}
