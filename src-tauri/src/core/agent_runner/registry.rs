//! The agent adapters this build knows, looked up by their stable key.

use super::{AgentAdapter, ClaudeCodeAdapter};

/// Every adapter, in the order a picker should list them.
pub fn all_adapters() -> Vec<Box<dyn AgentAdapter>> {
    vec![Box::new(ClaudeCodeAdapter)]
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

    #[test]
    fn adapter_keys_are_unique() {
        let mut keys: Vec<_> = all_adapters().iter().map(|a| a.key()).collect();
        let count = keys.len();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), count);
    }
}
