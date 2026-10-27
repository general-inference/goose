//! Per-agent annotations and conservative, optionally persisted smart-approval cache.
use super::{decide, store::PermissionManager, Decision, PermissionLevel};
use goose_provider_types::{conversation::message::ToolRequest, goose_mode::GooseMode};
use rmcp::model::Tool;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
};

pub struct PermissionPolicy {
    rules: Option<Arc<PermissionManager>>,
    readonly: RwLock<HashSet<String>>,
    cached: Mutex<HashMap<String, PermissionLevel>>,
}

impl PermissionPolicy {
    pub fn new(rules: Option<Arc<PermissionManager>>) -> Self {
        Self {
            rules,
            readonly: RwLock::default(),
            cached: Mutex::default(),
        }
    }

    pub fn apply_tool_annotations(&self, tools: &[Tool]) {
        *self.readonly.write().unwrap() = tools
            .iter()
            .filter(|tool| {
                tool.annotations
                    .as_ref()
                    .is_some_and(|a| a.read_only_hint == Some(true))
            })
            .map(|tool| tool.name.to_string())
            .collect();
        if let Some(rules) = &self.rules {
            rules.apply_tool_annotations(tools);
        }
        let mut cached = self.cached.lock().unwrap();
        for tool in tools.iter().filter(|tool| {
            tool.annotations
                .as_ref()
                .is_some_and(|a| a.read_only_hint == Some(false))
        }) {
            cached.insert(tool.name.to_string(), PermissionLevel::AskBefore);
        }
    }

    pub fn is_readonly_annotated_tool(&self, name: &str) -> bool {
        self.readonly.read().unwrap().contains(name)
    }

    /// Sensitive calls ignore read-only annotations. Explicit user rules retain
    /// their precedence; a classifier must never override a user denial.
    pub fn decide(&self, mode: GooseMode, name: &str, sensitive: bool) -> Decision {
        let user = self
            .rules
            .as_ref()
            .and_then(|rules| rules.get_user_permission(name));
        let cached = self
            .rules
            .as_ref()
            .and_then(|rules| rules.get_smart_approve_permission(name))
            .or_else(|| self.cached.lock().unwrap().get(name).cloned());
        decide(
            mode,
            user.as_ref(),
            !sensitive && self.is_readonly_annotated_tool(name),
            sensitive,
            cached.as_ref(),
        )
    }

    /// Resolve only candidates previously marked Judge, matching by request ID,
    /// never tool name. Only negative decisions are cached across invocations.
    /// An empty result (including classifier failure/timeout) requires approval.
    pub fn resolve_classification(
        &self,
        candidates: &[&ToolRequest],
        readonly_ids: &[String],
    ) -> Vec<Decision> {
        candidates
            .iter()
            .map(|request| {
                let Ok(call) = &request.tool_call else {
                    return Decision::Ask;
                };
                if readonly_ids.contains(&request.id) {
                    return Decision::Allow;
                }
                self.cached
                    .lock()
                    .unwrap()
                    .insert(call.name.to_string(), PermissionLevel::AskBefore);
                if let Some(rules) = &self.rules {
                    rules.update_smart_approve_permission(&call.name, PermissionLevel::AskBefore);
                }
                Decision::Ask
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{CallToolRequestParams, ToolAnnotations};
    fn request(id: &str) -> ToolRequest {
        ToolRequest {
            id: id.into(),
            tool_call: Ok(CallToolRequestParams::new("multi")),
            metadata: None,
            tool_meta: None,
        }
    }
    #[test]
    fn batch_classification_only_grants_exact_requests_and_caches_negative_decisions() {
        let policy = PermissionPolicy::new(None);
        let read = request("read");
        let write = request("write");
        assert_eq!(
            policy.resolve_classification(&[&read], &["read".into()]),
            vec![Decision::Allow]
        );
        assert_eq!(
            policy.decide(GooseMode::SmartApprove, "multi", false),
            Decision::Judge
        );
        assert_eq!(
            policy.resolve_classification(&[&read, &write], &["read".into(), "unknown".into()]),
            vec![Decision::Allow, Decision::Ask]
        );
        assert_eq!(
            policy.decide(GooseMode::SmartApprove, "multi", false),
            Decision::Ask
        );
    }
    #[test]
    fn annotations_are_agent_local_and_sensitive_calls_require_approval() {
        let policy = PermissionPolicy::new(None);
        let tool = Tool::new("multi", "test", serde_json::Map::new())
            .annotate(ToolAnnotations::new().read_only(true));
        policy.apply_tool_annotations(&[tool]);
        assert_eq!(
            policy.decide(GooseMode::SmartApprove, "multi", false),
            Decision::Allow
        );
        assert_eq!(
            policy.decide(GooseMode::SmartApprove, "multi", true),
            Decision::Ask
        );
        policy.apply_tool_annotations(&[]);
        assert_eq!(
            policy.decide(GooseMode::SmartApprove, "multi", false),
            Decision::Judge
        );
        assert_eq!(
            policy.resolve_classification(&[&request("timeout")], &[]),
            vec![Decision::Ask]
        );
    }
}
