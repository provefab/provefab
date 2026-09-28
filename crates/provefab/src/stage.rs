//! Structured stage results (spec §3.2). Rust owns these types; workers get
//! their JSON Schema and must return a value that matches.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the plan stage returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanOutput {
    /// Two or three sentences: what will change and why.
    pub summary: String,
    /// Ordered implementation steps, each small enough to verify.
    pub steps: Vec<String>,
    /// Repository-relative paths expected to change.
    pub files: Vec<String>,
    /// Anything that could make the change unsafe or larger than the issue suggests.
    pub risks: Vec<String>,
    /// Bugfix only: a shell command, run from the repository root, that fails
    /// now and will pass once the bug is fixed (spec §3.4 item 1). Null otherwise.
    pub repro_command: Option<String>,
}

/// What the review stage returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewOutput {
    pub verdict: ReviewVerdict,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReviewVerdict {
    Approve,
    Changes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub file: String,
    pub line: Option<u32>,
    pub severity: Severity,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Blocking,
    Minor,
}

/// JSON Schema for `T`, usable by every worker: no `$schema` key, no `format`
/// annotations (schemars emits `uint32` and similar, which strict validators
/// reject as unknown formats), and strict objects (`additionalProperties: false`,
/// every property required; optional fields stay nullable), which OpenAI
/// structured outputs require for Codex `--output-schema`.
pub fn output_schema<T: JsonSchema>() -> Value {
    let mut v = serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("$schema");
    }
    strip_formats(&mut v);
    make_strict(&mut v);
    v
}

fn make_strict(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if let Some(props) = map.get("properties").and_then(Value::as_object) {
                let required: Vec<Value> = props.keys().cloned().map(Value::String).collect();
                map.insert("required".into(), Value::Array(required));
                map.insert("additionalProperties".into(), Value::Bool(false));
            }
            map.values_mut().for_each(make_strict);
        }
        Value::Array(items) => items.iter_mut().for_each(make_strict),
        _ => {}
    }
}

fn strip_formats(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("format");
            map.values_mut().for_each(strip_formats);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_formats),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    out.push(k.clone());
                    keys(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| keys(v, out)),
            _ => {}
        }
    }

    #[test]
    fn schemas_have_no_meta_or_format_keys() {
        for schema in [
            output_schema::<PlanOutput>(),
            output_schema::<ReviewOutput>(),
        ] {
            let mut k = Vec::new();
            keys(&schema, &mut k);
            assert!(
                !k.contains(&"$schema".to_string()) && !k.contains(&"format".to_string()),
                "{schema}"
            );
            assert_eq!(schema["type"], "object");
        }
    }

    #[test]
    fn review_schema_constrains_the_verdict() {
        let s = output_schema::<ReviewOutput>().to_string();
        assert!(
            s.contains(r#""approve""#) && s.contains(r#""changes""#),
            "{s}"
        );
        let r: ReviewOutput = serde_json::from_value(json!({
            "verdict": "changes",
            "findings": [{"file": "src/a.rs", "line": 3, "severity": "blocking", "text": "off by one"}]
        }))
        .unwrap();
        assert_eq!(r.verdict, ReviewVerdict::Changes);
        assert!(
            serde_json::from_value::<ReviewOutput>(json!({"verdict": "lgtm", "findings": []}))
                .is_err()
        );
    }

    #[test]
    fn plan_requires_every_field() {
        let s = output_schema::<PlanOutput>();
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(required.len(), 5, "{s}");
    }

    fn objects(v: &Value, out: &mut Vec<Value>) {
        match v {
            Value::Object(m) => {
                if m.get("type") == Some(&json!("object")) || m.contains_key("properties") {
                    out.push(v.clone());
                }
                m.values().for_each(|v| objects(v, out));
            }
            Value::Array(a) => a.iter().for_each(|v| objects(v, out)),
            _ => {}
        }
    }

    /// OpenAI structured outputs (Codex `--output-schema`) reject a schema unless every
    /// object sets `additionalProperties: false` and lists all its properties as required.
    #[test]
    fn schemas_are_strict_for_openai_structured_outputs() {
        for schema in [
            output_schema::<PlanOutput>(),
            output_schema::<ReviewOutput>(),
        ] {
            let mut objs = Vec::new();
            objects(&schema, &mut objs);
            assert!(!objs.is_empty(), "{schema}");
            for o in objs {
                assert_eq!(o["additionalProperties"], json!(false), "{o}");
                let mut props: Vec<&str> = o["properties"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                let mut req: Vec<&str> = o["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .collect();
                props.sort();
                req.sort();
                assert_eq!(props, req, "{o}");
            }
        }
    }
}
