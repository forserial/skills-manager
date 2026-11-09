//! The agent adapters this build knows, looked up by their stable key.

use super::codex::CodexAdapter;
use super::{AgentAdapter, ClaudeCodeAdapter};

/// Every adapter, in the order a picker should list them. Codex is
/// experimental (never run against a real install; see sm-ye2).
pub fn all_adapters() -> Vec<Box<dyn AgentAdapter>> {
    vec![Box::new(ClaudeCodeAdapter), Box::new(CodexAdapter)]
}

/// The adapter whose [`AgentAdapter::key`] is `key`, if there is one.
pub fn adapter_for(key: &str) -> Option<Box<dyn AgentAdapter>> {
    all_adapters().into_iter().find(|a| a.key() == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_claude_code_by_key_and_rejects_unknown_keys() {
        assert_eq!(adapter_for("claude_code").unwrap().key(), "claude_code");
        assert!(adapter_for("no_such_agent").is_none());
    }

    /// Codex's strict mode wants every object closed and every property
    /// required. Check the transform holds for the real v1 contract schema,
    /// not only the copy in the Codex adapter's own tests.
    #[test]
    fn strict_codex_schema_of_the_v1_contract_closes_every_object() {
        fn check(node: &serde_json::Value, path: &str) {
            if let Some(map) = node.as_object() {
                if let Some(props) = map.get("properties").and_then(|p| p.as_object()) {
                    assert_eq!(
                        map.get("additionalProperties"),
                        Some(&serde_json::Value::Bool(false)),
                        "{path} is not closed"
                    );
                    let required: Vec<&str> = map["required"]
                        .as_array()
                        .unwrap_or_else(|| panic!("{path} has no required list"))
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect();
                    for key in props.keys() {
                        assert!(
                            required.contains(&key.as_str()),
                            "{path}.{key} not required"
                        );
                    }
                }
                for (key, child) in map {
                    check(child, &format!("{path}/{key}"));
                }
            } else if let Some(items) = node.as_array() {
                for (i, child) in items.iter().enumerate() {
                    check(child, &format!("{path}[{i}]"));
                }
            }
        }
        let strict = super::super::codex::strict_schema(&crate::core::evaluation::result_schema());
        check(&strict, "$");
    }

    #[test]
    fn codex_is_registered_and_marked_experimental() {
        let codex = adapter_for("codex").expect("codex adapter registered");
        assert!(codex.experimental());
        assert!(!adapter_for("claude_code").unwrap().experimental());
    }

    #[test]
    fn adapter_keys_are_unique() {
        let mut keys: Vec<_> = all_adapters().iter().map(|a| a.key()).collect();
        let count = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }
}
