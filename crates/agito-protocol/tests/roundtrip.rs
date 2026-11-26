use agito_protocol::cdp::{dom, page, runtime, target};
use agito_protocol::{Command, Event};
use serde_json::json;

#[test]
fn command_metadata() {
    assert_eq!(page::NavigateParams::METHOD, "Page.navigate");
    assert_eq!(
        <page::FrameNavigatedEvent as Event>::NAME,
        "Page.frameNavigated"
    );
}

#[test]
fn optional_fields_are_omitted() {
    let params = page::NavigateParams {
        url: "https://example.com".into(),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_value(&params).unwrap(),
        json!({ "url": "https://example.com" })
    );
}

#[test]
fn empty_params_serialize_as_object() {
    assert_eq!(
        serde_json::to_value(page::EnableParams::default()).unwrap(),
        json!({})
    );
}

#[test]
fn enums_use_wire_names() {
    let v: runtime::RemoteObjectType = serde_json::from_value(json!("bigint")).unwrap();
    assert_eq!(v, runtime::RemoteObjectType::Bigint);
    let unknown: runtime::RemoteObjectType = serde_json::from_value(json!("future-type")).unwrap();
    assert_eq!(unknown, runtime::RemoteObjectType::UnrecognizedValue);
}

#[test]
fn unknown_fields_are_ignored_and_recursive_types_parse() {
    let node: dom::Node = serde_json::from_value(json!({
        "nodeId": 1, "backendNodeId": 2, "nodeType": 9, "nodeName": "#document",
        "localName": "", "nodeValue": "", "somethingNew": true,
        "contentDocument": { "nodeId": 3, "backendNodeId": 4, "nodeType": 9,
            "nodeName": "#document", "localName": "", "nodeValue": "" }
    }))
    .unwrap();
    assert_eq!(node.content_document.unwrap().backend_node_id, 4);
}

#[test]
fn create_target_response() {
    let r: <target::CreateTargetParams as Command>::Response =
        serde_json::from_value(json!({ "targetId": "T1" })).unwrap();
    assert_eq!(r.target_id, "T1");
}
