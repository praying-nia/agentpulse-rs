//! Strict, version-pinned Codex App Server protocol processing.

use std::collections::BTreeMap;

use jsonschema::{Draft, Validator};
use serde_json::{Value, json};

use crate::{CodexProviderBuildError, CodexProviderSourceError};

pub(crate) const BUNDLED_SCHEMA: &str =
    include_str!("../schemas/codex_app_server_protocol.schemas.json");

#[derive(Clone)]
pub(crate) struct ProtocolSchema {
    client_request: Validator,
    client_notification: Validator,
    server_notification: Validator,
    server_request: Validator,
    jsonrpc_response: Validator,
    jsonrpc_error: Validator,
    initialize_response: Validator,
    thread_resume_response: Validator,
    model_list_response: Validator,
    permission_profile_list_response: Validator,
    thread_list_response: Validator,
    thread_items_list_response: Validator,
    thread_start_response: Validator,
    thread_fork_response: Validator,
    turn_start_response: Validator,
    turn_steer_response: Validator,
    empty_response: Validator,
    review_start_response: Validator,
    command_approval_response: Validator,
    file_approval_response: Validator,
    user_input_response: Validator,
}

impl ProtocolSchema {
    pub(crate) fn compile() -> Result<Self, CodexProviderBuildError> {
        let schema: Value = serde_json::from_str(BUNDLED_SCHEMA).map_err(|error| {
            CodexProviderBuildError::Schema {
                message: error.to_string(),
            }
        })?;

        Ok(Self {
            client_request: compile_ref(&schema, "#/definitions/ClientRequest")?,
            client_notification: compile_ref(&schema, "#/definitions/ClientNotification")?,
            server_notification: compile_ref(&schema, "#/definitions/ServerNotification")?,
            server_request: compile_ref(&schema, "#/definitions/ServerRequest")?,
            jsonrpc_response: compile_ref(&schema, "#/definitions/JSONRPCResponse")?,
            jsonrpc_error: compile_ref(&schema, "#/definitions/JSONRPCError")?,
            initialize_response: compile_ref(&schema, "#/definitions/InitializeResponse")?,
            thread_resume_response: compile_ref(&schema, "#/definitions/v2/ThreadResumeResponse")?,
            model_list_response: compile_ref(&schema, "#/definitions/v2/ModelListResponse")?,
            permission_profile_list_response: compile_ref(
                &schema,
                "#/definitions/v2/PermissionProfileListResponse",
            )?,
            thread_list_response: compile_ref(&schema, "#/definitions/v2/ThreadListResponse")?,
            thread_items_list_response: compile_ref(
                &schema,
                "#/definitions/v2/ThreadItemsListResponse",
            )?,
            thread_start_response: compile_ref(&schema, "#/definitions/v2/ThreadStartResponse")?,
            thread_fork_response: compile_ref(&schema, "#/definitions/v2/ThreadForkResponse")?,
            turn_start_response: compile_ref(&schema, "#/definitions/v2/TurnStartResponse")?,
            turn_steer_response: compile_ref(&schema, "#/definitions/v2/TurnSteerResponse")?,
            empty_response: compile_ref(&schema, "#/definitions/v2/TurnInterruptResponse")?,
            review_start_response: compile_ref(&schema, "#/definitions/v2/ReviewStartResponse")?,
            command_approval_response: compile_ref(
                &schema,
                "#/definitions/CommandExecutionRequestApprovalResponse",
            )?,
            file_approval_response: compile_ref(
                &schema,
                "#/definitions/FileChangeRequestApprovalResponse",
            )?,
            user_input_response: compile_ref(
                &schema,
                "#/definitions/ToolRequestUserInputResponse",
            )?,
        })
    }
}

fn compile_ref(
    schema: &Value,
    reference: &'static str,
) -> Result<Validator, CodexProviderBuildError> {
    let definitions =
        schema
            .get("definitions")
            .cloned()
            .ok_or_else(|| CodexProviderBuildError::Schema {
                message: "bundle has no definitions object".to_owned(),
            })?;
    let root = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$ref": reference,
        "definitions": definitions,
    });
    jsonschema::options()
        .with_draft(Draft::Draft7)
        .build(&root)
        .map_err(|error| CodexProviderBuildError::Schema {
            message: format!("{reference}: {error}"),
        })
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RequestId {
    Number(i64),
    String(String),
}

impl RequestId {
    pub(crate) fn from_value(value: &Value) -> Result<Self, CodexProviderSourceError> {
        if let Some(value) = value.as_i64() {
            Ok(Self::Number(value))
        } else if let Some(value) = value.as_str() {
            Ok(Self::String(value.to_owned()))
        } else {
            Err(CodexProviderSourceError::protocol(
                "request ID is neither an int64 nor a string",
            ))
        }
    }

    pub(crate) fn into_value(self) -> Value {
        match self {
            Self::Number(value) => Value::from(value),
            Self::String(value) => Value::from(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExpectedResponse {
    Initialize,
    ThreadResume,
    ModelList,
    PermissionProfileList,
    ThreadList,
    ThreadItemsList,
    ThreadStart,
    ThreadFork,
    TurnStart,
    TurnSteer,
    TurnInterrupt,
    ThreadCompact,
    ReviewStart,
    ThreadSetName,
}

impl ExpectedResponse {
    pub(crate) fn method(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::ThreadResume => "thread/resume",
            Self::ModelList => "model/list",
            Self::PermissionProfileList => "permissionProfile/list",
            Self::ThreadList => "thread/list",
            Self::ThreadItemsList => "thread/items/list",
            Self::ThreadStart => "thread/start",
            Self::ThreadFork => "thread/fork",
            Self::TurnStart => "turn/start",
            Self::TurnSteer => "turn/steer",
            Self::TurnInterrupt => "turn/interrupt",
            Self::ThreadCompact => "thread/compact/start",
            Self::ReviewStart => "review/start",
            Self::ThreadSetName => "thread/name/set",
        }
    }
}

#[derive(Debug)]
pub(crate) enum ServerFrame {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    Response {
        id: RequestId,
        expected: ExpectedResponse,
        result: Value,
    },
    Error {
        id: RequestId,
        expected: ExpectedResponse,
        code: i64,
        message: String,
    },
}

#[derive(Debug)]
pub(crate) enum ObservedServerFrame {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    PassThrough,
}

#[derive(Clone)]
pub(crate) struct ProtocolEngine {
    schema: ProtocolSchema,
    pending: BTreeMap<RequestId, ExpectedResponse>,
    next_request_id: i64,
}

impl ProtocolEngine {
    pub(crate) fn new(schema: ProtocolSchema) -> Self {
        Self {
            schema,
            pending: BTreeMap::new(),
            next_request_id: 1,
        }
    }

    pub(crate) fn initialize_request(
        &mut self,
    ) -> Result<(RequestId, String), CodexProviderSourceError> {
        let id = self.allocate(ExpectedResponse::Initialize);
        let value = json!({
            "id": id.clone().into_value(),
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "agentpulse",
                    "title": "AgentPulse Codex Provider",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {"experimentalApi": true}
            }
        });
        self.validate_client_request(&value)?;
        Ok((id, serialize(&value)?))
    }

    pub(crate) fn initialized_notification(&self) -> Result<String, CodexProviderSourceError> {
        let value = json!({"method": "initialized"});
        validate(
            &self.schema.client_notification,
            &value,
            "client notification",
        )?;
        serialize(&value)
    }

    pub(crate) fn thread_resume_request(
        &mut self,
        thread_id: &str,
    ) -> Result<(RequestId, String), CodexProviderSourceError> {
        let id = self.allocate(ExpectedResponse::ThreadResume);
        let value = json!({
            "id": id.clone().into_value(),
            "method": "thread/resume",
            "params": {"threadId": thread_id}
        });
        self.validate_client_request(&value)?;
        Ok((id, serialize(&value)?))
    }

    pub(crate) fn request(
        &mut self,
        expected: ExpectedResponse,
        params: Value,
    ) -> Result<(RequestId, String), CodexProviderSourceError> {
        let id = self.allocate(expected);
        let value = json!({
            "id": id.clone().into_value(),
            "method": expected.method(),
            "params": params,
        });
        if let Err(error) = self.validate_client_request(&value) {
            self.cancel_pending(&id);
            return Err(error);
        }
        Ok((id, serialize(&value)?))
    }

    pub(crate) fn unsupported_request_response(
        &self,
        id: RequestId,
        method: &str,
    ) -> Result<String, CodexProviderSourceError> {
        let value = json!({
            "id": id.into_value(),
            "error": {
                "code": -32601,
                "message": format!("AgentPulse does not implement {method}")
            }
        });
        validate(&self.schema.jsonrpc_error, &value, "client error response")?;
        serialize(&value)
    }

    pub(crate) fn interaction_response(
        &self,
        id: RequestId,
        method: &str,
        result: Value,
    ) -> Result<String, CodexProviderSourceError> {
        let validator = match method {
            "item/commandExecution/requestApproval" => &self.schema.command_approval_response,
            "item/fileChange/requestApproval" => &self.schema.file_approval_response,
            "item/tool/requestUserInput" => &self.schema.user_input_response,
            _ => {
                return Err(CodexProviderSourceError::protocol(format!(
                    "cannot encode a response for unsupported interaction method {method}"
                )));
            }
        };
        validate(validator, &result, "interaction response result")?;
        let value = json!({
            "id": id.into_value(),
            "result": result,
        });
        validate(
            &self.schema.jsonrpc_response,
            &value,
            "client approval response",
        )?;
        serialize(&value)
    }

    pub(crate) fn parse_server_text(
        &mut self,
        text: &str,
    ) -> Result<ServerFrame, CodexProviderSourceError> {
        let value: Value = serde_json::from_str(text).map_err(|error| {
            CodexProviderSourceError::protocol(format!(
                "invalid JSON at line {}, column {}",
                error.line(),
                error.column()
            ))
        })?;
        self.parse_server_value(value)
    }

    pub(crate) fn parse_observed_server_text(
        &self,
        text: &str,
    ) -> Result<ObservedServerFrame, CodexProviderSourceError> {
        let value: Value = serde_json::from_str(text).map_err(|error| {
            CodexProviderSourceError::protocol(format!(
                "invalid JSON at line {}, column {}",
                error.line(),
                error.column()
            ))
        })?;
        let object = value.as_object().ok_or_else(|| {
            CodexProviderSourceError::protocol("protocol frame must be a JSON object")
        })?;
        let has_method = object.contains_key("method");
        let has_id = object.contains_key("id");
        let has_result = object.contains_key("result");
        let has_error = object.contains_key("error");

        if has_method {
            if has_result || has_error {
                return Err(CodexProviderSourceError::protocol(
                    "method frame cannot also contain result or error",
                ));
            }
            let method = object
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| CodexProviderSourceError::protocol("method must be a string"))?
                .to_owned();
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            if has_id {
                validate(&self.schema.server_request, &value, "server request")?;
                Ok(ObservedServerFrame::Request {
                    id: RequestId::from_value(&value["id"])?,
                    method,
                    params,
                })
            } else {
                validate(
                    &self.schema.server_notification,
                    &value,
                    "server notification",
                )?;
                Ok(ObservedServerFrame::Notification { method, params })
            }
        } else if has_id && has_result != has_error {
            if has_result {
                validate(&self.schema.jsonrpc_response, &value, "server response")?;
            } else {
                validate(&self.schema.jsonrpc_error, &value, "server error response")?;
            }
            Ok(ObservedServerFrame::PassThrough)
        } else {
            Err(CodexProviderSourceError::protocol(
                "frame is not one unambiguous request, notification, response, or error",
            ))
        }
    }

    fn parse_server_value(
        &mut self,
        value: Value,
    ) -> Result<ServerFrame, CodexProviderSourceError> {
        let object = value.as_object().ok_or_else(|| {
            CodexProviderSourceError::protocol("protocol frame must be a JSON object")
        })?;
        let has_method = object.contains_key("method");
        let has_id = object.contains_key("id");
        let has_result = object.contains_key("result");
        let has_error = object.contains_key("error");

        if has_method {
            if has_result || has_error {
                return Err(CodexProviderSourceError::protocol(
                    "method frame cannot also contain result or error",
                ));
            }
            let method = object
                .get("method")
                .and_then(Value::as_str)
                .ok_or_else(|| CodexProviderSourceError::protocol("method must be a string"))?
                .to_owned();
            if has_id {
                validate(&self.schema.server_request, &value, "server request")?;
                let id = RequestId::from_value(&value["id"])?;
                Ok(ServerFrame::Request {
                    id,
                    method,
                    params: value.get("params").cloned().unwrap_or(Value::Null),
                })
            } else {
                validate(
                    &self.schema.server_notification,
                    &value,
                    "server notification",
                )?;
                Ok(ServerFrame::Notification {
                    method,
                    params: value.get("params").cloned().unwrap_or(Value::Null),
                })
            }
        } else if has_id && has_result != has_error {
            let id = RequestId::from_value(&value["id"])?;
            let expected = self.pending.remove(&id).ok_or_else(|| {
                CodexProviderSourceError::protocol("response ID does not match a pending request")
            })?;
            if has_result {
                validate(&self.schema.jsonrpc_response, &value, "server response")?;
                let result = value.get("result").cloned().ok_or_else(|| {
                    CodexProviderSourceError::protocol("successful response has no result")
                })?;
                let validator = match expected {
                    ExpectedResponse::Initialize => &self.schema.initialize_response,
                    ExpectedResponse::ThreadResume => &self.schema.thread_resume_response,
                    ExpectedResponse::ModelList => &self.schema.model_list_response,
                    ExpectedResponse::PermissionProfileList => {
                        &self.schema.permission_profile_list_response
                    }
                    ExpectedResponse::ThreadList => &self.schema.thread_list_response,
                    ExpectedResponse::ThreadItemsList => &self.schema.thread_items_list_response,
                    ExpectedResponse::ThreadStart => &self.schema.thread_start_response,
                    ExpectedResponse::ThreadFork => &self.schema.thread_fork_response,
                    ExpectedResponse::TurnStart => &self.schema.turn_start_response,
                    ExpectedResponse::TurnSteer => &self.schema.turn_steer_response,
                    ExpectedResponse::TurnInterrupt
                    | ExpectedResponse::ThreadCompact
                    | ExpectedResponse::ThreadSetName => &self.schema.empty_response,
                    ExpectedResponse::ReviewStart => &self.schema.review_start_response,
                };
                validate(validator, &result, expected.method())?;
                Ok(ServerFrame::Response {
                    id,
                    expected,
                    result,
                })
            } else {
                validate(&self.schema.jsonrpc_error, &value, "server error response")?;
                let error = value
                    .get("error")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        CodexProviderSourceError::protocol("error response has no error object")
                    })?;
                let code = error.get("code").and_then(Value::as_i64).ok_or_else(|| {
                    CodexProviderSourceError::protocol("error response code must be an int64")
                })?;
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        CodexProviderSourceError::protocol(
                            "error response message must be a string",
                        )
                    })?
                    .chars()
                    .take(512)
                    .collect();
                Ok(ServerFrame::Error {
                    id,
                    expected,
                    code,
                    message,
                })
            }
        } else {
            Err(CodexProviderSourceError::protocol(
                "frame is not one unambiguous request, notification, response, or error",
            ))
        }
    }

    fn validate_client_request(&self, value: &Value) -> Result<(), CodexProviderSourceError> {
        validate(&self.schema.client_request, value, "client request")
    }

    fn allocate(&mut self, expected: ExpectedResponse) -> RequestId {
        let id = RequestId::Number(self.next_request_id);
        self.next_request_id += 1;
        self.pending.insert(id.clone(), expected);
        id
    }

    pub(crate) fn cancel_pending(&mut self, id: &RequestId) {
        self.pending.remove(id);
    }
}

fn validate(
    validator: &Validator,
    value: &Value,
    category: &'static str,
) -> Result<(), CodexProviderSourceError> {
    if let Err(error) = validator.validate(value) {
        Err(CodexProviderSourceError::protocol(format!(
            "schema rejected {category} at {}",
            error.instance_path()
        )))
    } else {
        Ok(())
    }
}

fn serialize(value: &Value) -> Result<String, CodexProviderSourceError> {
    serde_json::to_string(value).map_err(|error| {
        CodexProviderSourceError::protocol(format!("JSON encoding failed: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    type TestResult = Result<(), Box<dyn Error>>;

    fn engine() -> Result<ProtocolEngine, CodexProviderBuildError> {
        ProtocolSchema::compile().map(ProtocolEngine::new)
    }

    #[test]
    fn validates_outbound_handshake_and_correlates_initialize() -> TestResult {
        let mut engine = engine()?;
        let (_, request) = engine.initialize_request()?;
        assert!(request.contains("\"method\":\"initialize\""));
        assert!(request.contains("\"experimentalApi\":true"));

        let frame = engine
            .parse_server_text(
                r#"{"id":1,"result":{"codexHome":"/tmp/codex","platformFamily":"unix","platformOs":"linux","userAgent":"codex_cli_rs/0.150.1"}}"#,
            )?;
        assert!(matches!(
            frame,
            ServerFrame::Response {
                expected: ExpectedResponse::Initialize,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn rejects_unknown_response_and_invalid_notification() -> TestResult {
        let mut engine = engine()?;
        let unknown = engine.parse_server_text(r#"{"id":99,"result":{}}"#);
        assert!(unknown.is_err());

        let invalid = engine
            .parse_server_text(r#"{"method":"thread/status/changed","params":{"threadId":"x"}}"#);
        assert!(invalid.is_err());
        Ok(())
    }

    #[test]
    fn observer_validates_unowned_responses_without_correlating_them() -> TestResult {
        let engine = engine()?;
        assert!(matches!(
            engine.parse_observed_server_text(r#"{"id":99,"result":{"value":true}}"#)?,
            ObservedServerFrame::PassThrough
        ));
        assert!(matches!(
            engine.parse_observed_server_text(
                r#"{"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"threadId":"019976a4-00f0-7312-b36c-d01f9c5c06f6","turnId":"019976a4-00f1-76c0-b845-e1509dc4e3de","itemId":"019976a4-00f2-741b-870f-21b4fb983746","startedAtMs":1000,"reason":null}}"#,
            )?,
            ObservedServerFrame::Request { .. }
        ));
        Ok(())
    }

    #[test]
    fn validates_approval_requests_and_every_supported_response_shape() -> TestResult {
        let mut engine = engine()?;
        let request = engine
            .parse_server_text(
                r#"{"id":"approval-1","method":"item/commandExecution/requestApproval","params":{"threadId":"019976a4-00f0-7312-b36c-d01f9c5c06f6","turnId":"019976a4-00f1-76c0-b845-e1509dc4e3de","itemId":"019976a4-00f2-741b-870f-21b4fb983746","startedAtMs":1000,"reason":null}}"#,
            )?;
        let (id, params) = match request {
            ServerFrame::Request { id, params, .. } => (id, params),
            _ => return Err("expected server request".into()),
        };
        assert_eq!(params["startedAtMs"], 1_000);
        let response = engine.interaction_response(
            id,
            "item/commandExecution/requestApproval",
            json!({
                "decision": {
                    "acceptWithExecpolicyAmendment": {
                        "execpolicy_amendment": ["cargo", "test"]
                    }
                }
            }),
        )?;
        assert!(response.contains("acceptWithExecpolicyAmendment"));
        let network = engine.interaction_response(
            RequestId::String("approval-2".to_owned()),
            "item/commandExecution/requestApproval",
            json!({
                "decision": {
                    "applyNetworkPolicyAmendment": {
                        "network_policy_amendment": {"action": "allow", "host": "example.com"}
                    }
                }
            }),
        )?;
        assert!(network.contains("applyNetworkPolicyAmendment"));
        for decision in ["accept", "acceptForSession", "decline", "cancel"] {
            let response = engine.interaction_response(
                RequestId::String(format!("command-{decision}")),
                "item/commandExecution/requestApproval",
                json!({"decision": decision}),
            )?;
            assert!(response.contains(decision));
            let response = engine.interaction_response(
                RequestId::String(format!("file-{decision}")),
                "item/fileChange/requestApproval",
                json!({"decision": decision}),
            )?;
            assert!(response.contains(decision));
        }
        Ok(())
    }

    #[test]
    fn validates_the_bounded_remote_control_request_set() -> TestResult {
        let mut engine = engine()?;
        let requests = [
            (ExpectedResponse::ModelList, json!({"limit": 50})),
            (
                ExpectedResponse::PermissionProfileList,
                json!({"limit": 50}),
            ),
            (
                ExpectedResponse::ThreadList,
                json!({"limit": 50, "sortKey": "updated_at", "sortDirection": "desc"}),
            ),
            (
                ExpectedResponse::ThreadItemsList,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6", "limit": 100, "sortDirection": "asc"}),
            ),
            (
                ExpectedResponse::ThreadStart,
                json!({"cwd": "/workspace", "sessionStartSource": "clear"}),
            ),
            (
                ExpectedResponse::ThreadFork,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6"}),
            ),
            (
                ExpectedResponse::TurnStart,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6", "input": [{"type": "text", "text": "continue"}]}),
            ),
            (
                ExpectedResponse::TurnSteer,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6", "expectedTurnId": "019976a4-00f1-76c0-b845-e1509dc4e3de", "input": [{"type": "text", "text": "adjust"}]}),
            ),
            (
                ExpectedResponse::TurnInterrupt,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6", "turnId": "019976a4-00f1-76c0-b845-e1509dc4e3de"}),
            ),
            (
                ExpectedResponse::ThreadCompact,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6"}),
            ),
            (
                ExpectedResponse::ReviewStart,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6", "target": {"type": "uncommittedChanges"}, "delivery": "inline"}),
            ),
            (
                ExpectedResponse::ThreadSetName,
                json!({"threadId": "019976a4-00f0-7312-b36c-d01f9c5c06f6", "name": "New name"}),
            ),
        ];
        for (expected, params) in requests {
            let (id, text) = engine.request(expected, params)?;
            assert!(text.contains(expected.method()));
            engine.cancel_pending(&id);
        }
        Ok(())
    }
}
