//! Structural contracts for the runner's nine allowed tools.

use std::sync::LazyLock;

use rmcp::model::{CallToolResult, Tool};
use serde_json::{json, Map, Value};

use super::{
    error::RunnerError,
    types::{AllowedTool, ToolRequest},
};

pub(crate) const PAYLOAD_CAP: usize = 1_048_576;
pub(crate) const ALLOWED: [AllowedTool; 9] = [
    AllowedTool::Search,
    AllowedTool::GetConcept,
    AllowedTool::History,
    AllowedTool::Claim,
    AllowedTool::Renew,
    AllowedTool::ReadRevision,
    AllowedTool::Submit,
    AllowedTool::Close,
    AllowedTool::GetRun,
];

// Captured from OkfServer::tools(), not an invented protocol version/hash.
static CONTRACTS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/investigator/contract/tools.json"
    ))
    .expect("checked-in tool contracts")
});

/// Require each allowed tool exactly once and compare structural input/output contracts.
/// Descriptions, schema annotations and local definition names are not identities.
/// Extra server tools are ignored and never become dispatchable.
pub fn validate_tools(tools: &[Tool]) -> Result<(), RunnerError> {
    bounded(tools)?;
    for allowed in ALLOWED {
        let mut matches = tools.iter().filter(|t| t.name == allowed.name());
        let actual = matches.next().ok_or(RunnerError::Contract)?;
        if matches.next().is_some() {
            return Err(RunnerError::Contract);
        }
        let expected = expected(allowed);
        for (actual, expected) in [
            (
                Value::Object((*actual.input_schema).clone()),
                Value::Object((*expected.input_schema).clone()),
            ),
            (
                Value::Object(
                    (**actual.output_schema.as_ref().ok_or(RunnerError::Contract)?).clone(),
                ),
                Value::Object(
                    (**expected
                        .output_schema
                        .as_ref()
                        .ok_or(RunnerError::Contract)?)
                    .clone(),
                ),
            ),
        ] {
            if normalize(&actual, &actual, 0)? != normalize(&expected, &expected, 0)? {
                return Err(RunnerError::Contract);
            }
        }
    }
    Ok(())
}

fn expected(tool: AllowedTool) -> &'static Tool {
    CONTRACTS
        .iter()
        .find(|t| t.name == tool.name())
        .expect("nine checked-in contracts")
}

/// This is a decoded/serialized size check, not a preallocation memory bound.
pub(crate) fn bounded(value: &(impl serde::Serialize + ?Sized)) -> Result<(), RunnerError> {
    if serde_json::to_vec(value)
        .map_err(|_| RunnerError::Contract)?
        .len()
        > PAYLOAD_CAP
    {
        return Err(RunnerError::Contract);
    }
    Ok(())
}

// Only the finite schema dialect emitted by slice 1 is supported. Unknown
// constraints fail closed. Property names are data, not schema keywords.
fn normalize(schema: &Value, root: &Value, depth: usize) -> Result<Value, RunnerError> {
    let mut remaining = PAYLOAD_CAP;
    expand(schema, root, depth, &mut remaining)
}

fn expand(
    schema: &Value,
    root: &Value,
    depth: usize,
    remaining: &mut usize,
) -> Result<Value, RunnerError> {
    // Charge each visited subtree, including repeated refs, before cloning it.
    // Depth alone does not stop a small DAG from expanding exponentially.
    let size = serde_json::to_vec(schema)
        .map_err(|_| RunnerError::Contract)?
        .len();
    *remaining = remaining.checked_sub(size).ok_or(RunnerError::Contract)?;
    if depth > 64 {
        return Err(RunnerError::Contract);
    }
    if schema.is_boolean() {
        return Ok(schema.clone());
    }
    let fields = schema.as_object().ok_or(RunnerError::Contract)?;
    if let Some(reference) = fields.get("$ref") {
        if fields
            .keys()
            .any(|k| !["$ref", "description", "title", "$comment"].contains(&k.as_str()))
        {
            return Err(RunnerError::Contract);
        }
        let pointer = reference
            .as_str()
            .and_then(|r| r.strip_prefix('#'))
            .filter(|p| p.starts_with('/'))
            .ok_or(RunnerError::Contract)?;
        return expand(
            root.pointer(pointer).ok_or(RunnerError::Contract)?,
            root,
            depth + 1,
            remaining,
        );
    }
    let mut result = Map::new();
    for (key, value) in fields {
        let normalized = match key.as_str() {
            "$defs" | "$schema" | "title" | "description" | "$comment" | "default" => continue,
            "properties" => {
                let mut properties = Map::new();
                for (name, schema) in value.as_object().ok_or(RunnerError::Contract)? {
                    properties.insert(name.clone(), expand(schema, root, depth + 1, remaining)?);
                }
                Value::Object(properties)
            }
            "items" | "additionalProperties" => expand(value, root, depth + 1, remaining)?,
            "anyOf" => Value::Array(
                value
                    .as_array()
                    .ok_or(RunnerError::Contract)?
                    .iter()
                    .map(|s| expand(s, root, depth + 1, remaining))
                    .collect::<Result<_, _>>()?,
            ),
            "required" | "enum" | "type" if value.is_array() => {
                let mut values = value.as_array().ok_or(RunnerError::Contract)?.clone();
                values.sort_by_key(Value::to_string);
                Value::Array(values)
            }
            "type" | "format" | "minimum" | "maximum" => value.clone(),
            _ => return Err(RunnerError::Contract),
        };
        result.insert(key.clone(), normalized);
    }
    Ok(Value::Object(result))
}

pub(crate) fn arguments(
    request: ToolRequest,
    catalog: &str,
) -> Result<Map<String, Value>, RunnerError> {
    bounded(&request.args)?;
    let mut args = request
        .args
        .as_object()
        .ok_or(RunnerError::Contract)?
        .clone();
    let (key, selection) = if request.tool == AllowedTool::Search {
        ("catalogs", json!([catalog]))
    } else {
        ("catalog", json!(catalog))
    };
    if args.get(key).is_some_and(|v| v != &selection) {
        return Err(RunnerError::Contract);
    }
    args.insert(key.into(), selection);
    // Published evidence only; no changeset overlay is a permitted runner input.
    if args.get("changeset").is_some_and(|v| !v.is_null()) {
        return Err(RunnerError::Contract);
    }
    if request.tool == AllowedTool::Submit
        && args
            .get("submission")
            .and_then(|s| s.get("assessment"))
            .and_then(Value::as_str)
            == Some("supported_current")
    {
        return Err(RunnerError::InvalidProposal);
    }
    let root = Value::Object((*expected(request.tool).input_schema).clone());
    let schema = normalize(&root, &root, 0)?;
    check(&Value::Object(args.clone()), &schema, catalog)?;
    bounded(&args)?;
    Ok(args)
}

pub(crate) fn result(
    tool: AllowedTool,
    response: CallToolResult,
    catalog: &str,
) -> Result<Value, RunnerError> {
    // No text fallback, including error text. isError wins over plausible data.
    if response.is_error == Some(true) {
        return Err(RunnerError::Permission);
    }
    bounded(&response)?;
    let value = response.structured_content.ok_or(RunnerError::Contract)?;
    let root = Value::Object(
        (**expected(tool)
            .output_schema
            .as_ref()
            .ok_or(RunnerError::Contract)?)
        .clone(),
    );
    check(&value, &normalize(&root, &root, 0)?, catalog)?;
    Ok(value)
}

// The step-driven collector also accepts offline/fake replies. Validate those
// against the same server DTO schemas as the authenticated transport.
pub(crate) fn collection_result(
    tool: AllowedTool,
    value: &Value,
    catalog: &str,
) -> Result<(), RunnerError> {
    if !matches!(
        tool,
        AllowedTool::GetConcept
            | AllowedTool::History
            | AllowedTool::Search
            | AllowedTool::ReadRevision
    ) {
        return Err(RunnerError::Contract);
    }
    bounded(value)?;
    let root = Value::Object(
        (**expected(tool)
            .output_schema
            .as_ref()
            .ok_or(RunnerError::Contract)?)
        .clone(),
    );
    check(value, &normalize(&root, &root, 0)?, catalog)
}

fn matches_type(value: &Value, kind: &str) -> bool {
    match kind {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        _ => false,
    }
}

// Traverse protocol fields only. An unconstrained Value (frontmatter/error data)
// and quoted text are not protocol envelopes, even if they contain "catalog".
fn check(value: &Value, schema: &Value, catalog: &str) -> Result<(), RunnerError> {
    let bad = || RunnerError::Contract;
    if schema == &Value::Bool(true) {
        return Ok(());
    }
    if schema == &Value::Bool(false) {
        return Err(bad());
    }
    if let Some(types) = schema.get("type") {
        let valid = match types {
            Value::String(t) => matches_type(value, t),
            Value::Array(ts) => ts
                .iter()
                .any(|t| t.as_str().is_some_and(|t| matches_type(value, t))),
            _ => false,
        };
        if !valid {
            return Err(bad());
        }
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        if !choices.contains(value) {
            return Err(bad());
        }
    }
    if let Some(choices) = schema.get("anyOf").and_then(Value::as_array) {
        if !choices.iter().any(|s| check(value, s, catalog).is_ok()) {
            return Err(bad());
        }
    }
    if let Some(number) = value.as_f64() {
        if schema
            .get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|n| number < n)
            || schema
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|n| number > n)
        {
            return Err(bad());
        }
        match schema.get("format").and_then(Value::as_str) {
            Some("int32") if value.as_i64().is_none_or(|n| i32::try_from(n).is_err()) => {
                return Err(bad())
            }
            Some("uint32") if value.as_u64().is_none_or(|n| u32::try_from(n).is_err()) => {
                return Err(bad())
            }
            Some("uint64" | "uint") if value.as_u64().is_none() => return Err(bad()),
            _ => {}
        }
    }
    if let Some(fields) = value.as_object() {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for name in required {
                    if !fields.contains_key(name.as_str().ok_or_else(bad)?) {
                        return Err(bad());
                    }
                }
            }
            for (name, child) in fields {
                let field_schema = properties.get(name).ok_or_else(bad)?;
                if name == "catalog" && child != catalog {
                    return Err(bad());
                }
                if name == "catalogs" && child != &json!([catalog]) {
                    return Err(bad());
                }
                if name == "revision" && child.as_i64().is_none_or(|n| n <= 0) {
                    return Err(bad());
                }
                check(child, field_schema, catalog)?;
            }
        }
    }
    if let (Some(items), Some(item_schema)) = (value.as_array(), schema.get("items")) {
        for item in items {
            check(item, item_schema, catalog)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::OkfServer;

    #[test]
    fn investigator_contract_rejects_missing_or_changed_tools() {
        let tools = OkfServer::tools().list_all();
        assert!(validate_tools(&tools).is_ok());
        let mut missing = tools.clone();
        missing.retain(|t| t.name != "maintenance_submit");
        assert!(validate_tools(&missing).is_err());
        let mut changed = tools.clone();
        let renew = changed
            .iter_mut()
            .find(|t| t.name == "maintenance_renew")
            .unwrap();
        std::sync::Arc::make_mut(&mut renew.input_schema)["required"] =
            serde_json::json!(["catalog", "work_id", "idempotency_key"]);
        assert!(validate_tools(&changed).is_err());
    }

    #[test]
    fn investigator_contract_mismatch_fixtures_and_refs() {
        let tools = OkfServer::tools().list_all();
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/investigator/contract/mismatches.json"
        ))
        .unwrap();
        for case in cases.as_array().unwrap() {
            let mut wire = serde_json::to_value(&tools).unwrap();
            let tool = wire
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|t| t["name"] == case["tool"])
                .unwrap();
            *tool[case["schema"].as_str().unwrap()]
                .pointer_mut(case["pointer"].as_str().unwrap())
                .unwrap() = case["value"].clone();
            assert!(validate_tools(&serde_json::from_value::<Vec<Tool>>(wire).unwrap()).is_err());
        }
        let mut wire = serde_json::to_value(&tools).unwrap();
        let tool = wire
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|t| t["name"] == "maintenance_renew")
            .unwrap();
        tool["inputSchema"]["$defs"] = serde_json::json!({"Token":{"type":"string"}});
        tool["inputSchema"]["properties"]["token"] = serde_json::json!({"$ref":"#/$defs/Token"});
        assert!(
            validate_tools(&serde_json::from_value::<Vec<Tool>>(wire.clone()).unwrap()).is_ok()
        );
        let tool = wire
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|t| t["name"] == "maintenance_renew")
            .unwrap();
        tool["inputSchema"]["$defs"]["Token"] = serde_json::json!({"$ref":"#/$defs/Token"});
        assert!(validate_tools(&serde_json::from_value::<Vec<Tool>>(wire).unwrap()).is_err());
    }

    #[test]
    fn investigator_contract_requests_fail_closed_without_panics() {
        for tool in ALLOWED {
            assert!(arguments(
                ToolRequest {
                    tool,
                    args: json!({})
                },
                "pg"
            )
            .is_err());
            assert!(arguments(
                ToolRequest {
                    tool,
                    args: json!(null)
                },
                "pg"
            )
            .is_err());
        }
        let base = json!({"work_id":"w","token":"t","idempotency_key":"i","submission":{
            "outcome":"inconclusive","assessment":"unverifiable","summary":"catalog: other",
            "evidence":[{"pin":{"path":"/x.md","revision":1},"excerpt":"catalog: other"}],"findings":[]
        }});
        assert!(arguments(
            ToolRequest {
                tool: AllowedTool::Submit,
                args: base.clone()
            },
            "pg"
        )
        .is_ok());
        for (pointer, value) in [
            ("/submission/assessment", json!("supported_current")),
            ("/submission/evidence/0/pin/revision", json!(0)),
        ] {
            let mut bad = base.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(arguments(
                ToolRequest {
                    tool: AllowedTool::Submit,
                    args: bad
                },
                "pg"
            )
            .is_err());
        }
        for pointer in [
            "",
            "/submission",
            "/submission/evidence/0",
            "/submission/evidence/0/pin",
        ] {
            for field in ["catalog", "actor", "at"] {
                let mut bad = base.clone();
                bad.pointer_mut(pointer)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(field.into(), json!("foreign"));
                assert!(arguments(
                    ToolRequest {
                        tool: AllowedTool::Submit,
                        args: bad
                    },
                    "pg"
                )
                .is_err());
            }
        }
    }

    #[test]
    fn investigator_contract_ref_expansion_has_a_total_budget() {
        let mut schema =
            json!({"$defs":{}, "type":"object", "properties":{"root":{"$ref":"#/$defs/N14"}}});
        schema["$defs"]["N0"] = json!({"type":"string"});
        for n in 1..=14 {
            let reference = format!("#/$defs/N{}", n - 1);
            schema["$defs"][format!("N{n}")] = json!({"type":"object", "properties":{"left":{"$ref":reference},"right":{"$ref":reference}}});
        }
        assert!(matches!(
            normalize(&schema, &schema, 0),
            Err(RunnerError::Contract)
        ));
    }

    #[test]
    fn investigator_contract_matches_delivered_maintenance_fixture() {
        let delivered: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/maintenance-schemas.json"
        ))
        .unwrap();
        for tool in CONTRACTS
            .iter()
            .filter(|t| t.name.starts_with("maintenance_"))
        {
            for (name, schema) in [
                ("input", Value::Object((*tool.input_schema).clone())),
                (
                    "output",
                    Value::Object((**tool.output_schema.as_ref().unwrap()).clone()),
                ),
            ] {
                let previous = &delivered[tool.name.as_ref()][name];
                assert_eq!(
                    normalize(&schema, &schema, 0).unwrap(),
                    normalize(previous, previous, 0).unwrap()
                );
            }
        }
    }

    #[test]
    #[ignore = "explicit fixture capture from actual slice-1 tool router"]
    fn investigator_contract_capture_fixture() {
        let mut tools = OkfServer::tools().list_all();
        tools.retain(|t| {
            [
                "search",
                "get_concept",
                "history",
                "maintenance_claim",
                "maintenance_renew",
                "maintenance_read_revision",
                "maintenance_submit",
                "maintenance_close",
                "maintenance_get_run",
            ]
            .contains(&t.name.as_ref())
        });
        std::fs::create_dir_all("tests/fixtures/investigator/contract").unwrap();
        std::fs::write(
            "tests/fixtures/investigator/contract/tools.json",
            serde_json::to_string_pretty(&tools).unwrap(),
        )
        .unwrap();
    }
}
