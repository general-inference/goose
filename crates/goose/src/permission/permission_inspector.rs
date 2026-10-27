use crate::agents::platform_extensions::MANAGE_EXTENSIONS_TOOL_NAME_COMPLETE;
use crate::agents::provider_manager::ProviderManager;
#[cfg(test)]
use crate::config::permission::PermissionLevel;
use crate::config::{GooseMode, PermissionManager};
use crate::conversation::message::{Message, ToolRequest};
use crate::permission::permission_judge::{detect_read_only_requests, PermissionCheckResult};
use crate::tool_inspection::{InspectionAction, InspectionResult, ToolInspector};
use anyhow::Result;
use async_trait::async_trait;
use goose_agent::permission::{policy::PermissionPolicy, Decision};
use rmcp::model::Tool;
use std::sync::Arc;

/// Permission Inspector that handles tool permission checking
pub struct PermissionInspector {
    pub permission_manager: Arc<PermissionManager>,
    providers: Arc<ProviderManager>,
    session_manager: Arc<crate::session::SessionManager>,
    policy: PermissionPolicy,
}

impl PermissionInspector {
    pub fn new(
        permission_manager: Arc<PermissionManager>,
        providers: Arc<ProviderManager>,
        session_manager: Arc<crate::session::SessionManager>,
    ) -> Self {
        Self {
            policy: PermissionPolicy::new(Some(permission_manager.shared())),
            permission_manager,
            providers,
            session_manager,
        }
    }

    // readonly_tools is per-agent to avoid concurrent session clobbering; write-annotated
    // tools are cached globally via PermissionManager.
    pub fn apply_tool_annotations(&self, tools: &[Tool]) {
        self.policy.apply_tool_annotations(tools);
    }

    pub fn is_readonly_annotated_tool(&self, tool_name: &str) -> bool {
        self.policy.is_readonly_annotated_tool(tool_name)
    }

    /// Process inspection results into permission decisions
    /// This method takes all inspection results and converts them into a PermissionCheckResult
    /// that can be used by the agent to determine which tools to approve, deny, or ask for approval
    pub fn process_inspection_results(
        &self,
        remaining_requests: &[ToolRequest],
        inspection_results: &[InspectionResult],
    ) -> PermissionCheckResult {
        use crate::tool_inspection::apply_inspection_results_to_permissions;

        // Start with permission inspector's decisions as the baseline
        let mut permission_check_result = PermissionCheckResult {
            approved: vec![],
            needs_approval: vec![],
            denied: vec![],
        };

        // Apply permission inspector results first (baseline behavior)
        let permission_results: Vec<_> = inspection_results
            .iter()
            .filter(|result| result.inspector_name == "permission")
            .collect();

        for request in remaining_requests {
            // Find the permission decision for this request
            if let Some(permission_result) = permission_results
                .iter()
                .find(|result| result.tool_request_id == request.id)
            {
                match permission_result.action {
                    InspectionAction::Allow => {
                        permission_check_result.approved.push(request.clone());
                    }
                    InspectionAction::Deny => {
                        permission_check_result.denied.push(request.clone());
                    }
                    InspectionAction::RequireApproval(_) => {
                        permission_check_result.needs_approval.push(request.clone());
                    }
                }
            } else {
                // If no permission result found, default to needs approval for safety
                permission_check_result.needs_approval.push(request.clone());
            }
        }

        // Apply security and other inspector results as overrides
        let non_permission_results: Vec<_> = inspection_results
            .iter()
            .filter(|result| result.inspector_name != "permission")
            .cloned()
            .collect();

        if !non_permission_results.is_empty() {
            permission_check_result = apply_inspection_results_to_permissions(
                permission_check_result,
                &non_permission_results,
            );
        }

        permission_check_result
    }
}

#[async_trait]
impl ToolInspector for PermissionInspector {
    fn name(&self) -> &'static str {
        "permission"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn inspect(
        &self,
        session_id: &str,
        tool_requests: &[ToolRequest],
        _messages: &[Message],
        goose_mode: GooseMode,
    ) -> Result<Vec<InspectionResult>> {
        let mut results = Vec::new();
        let mut llm_detect_candidates: Vec<&ToolRequest> = Vec::new();

        for request in tool_requests {
            if let Ok(tool_call) = &request.tool_call {
                let tool_name = &tool_call.name;

                let action = match self.policy.decide(
                    goose_mode,
                    tool_name,
                    tool_name == MANAGE_EXTENSIONS_TOOL_NAME_COMPLETE,
                ) {
                    goose_agent::permission::Decision::Skip => continue,
                    goose_agent::permission::Decision::Allow => InspectionAction::Allow,
                    goose_agent::permission::Decision::Deny => InspectionAction::Deny,
                    goose_agent::permission::Decision::Ask => InspectionAction::RequireApproval(
                        (tool_name == MANAGE_EXTENSIONS_TOOL_NAME_COMPLETE).then(|| {
                            "Extension management requires approval for security".to_string()
                        }),
                    ),
                    goose_agent::permission::Decision::Judge => {
                        llm_detect_candidates.push(request);
                        continue;
                    }
                };

                let reason = match &action {
                    InspectionAction::Allow => {
                        if goose_mode == GooseMode::Auto {
                            "Auto mode - all tools approved".to_string()
                        } else if self.is_readonly_annotated_tool(tool_name) {
                            "Tool annotated as read-only".to_string()
                        } else {
                            "User permission allows this tool".to_string()
                        }
                    }
                    InspectionAction::Deny => "User permission denies this tool".to_string(),
                    InspectionAction::RequireApproval(_) => {
                        if tool_name == MANAGE_EXTENSIONS_TOOL_NAME_COMPLETE {
                            "Extension management requires user approval".to_string()
                        } else {
                            "Tool requires user approval".to_string()
                        }
                    }
                };

                results.push(InspectionResult {
                    tool_request_id: request.id.clone(),
                    action,
                    reason,
                    confidence: 1.0, // Permission decisions are definitive
                    inspector_name: self.name().to_string(),
                    finding_id: None,
                });
            }
        }

        // LLM-based read-only detection for deferred SmartApprove candidates
        if !llm_detect_candidates.is_empty() {
            let provider = match self.session_manager.get_session(session_id, false).await {
                Ok(session) => self.providers.provider_for(&session).await.ok(),
                Err(_) => None,
            };
            let detected_request_ids: Vec<String> = match provider {
                Some(provider) => {
                    detect_read_only_requests(
                        provider,
                        &self.session_manager,
                        session_id,
                        llm_detect_candidates.to_vec(),
                    )
                    .await
                }
                None => Default::default(),
            };

            for (candidate, decision) in llm_detect_candidates.iter().zip(
                self.policy
                    .resolve_classification(&llm_detect_candidates, &detected_request_ids),
            ) {
                let is_readonly = decision == Decision::Allow;

                results.push(InspectionResult {
                    tool_request_id: candidate.id.clone(),
                    action: if is_readonly {
                        InspectionAction::Allow
                    } else {
                        InspectionAction::RequireApproval(None)
                    },
                    reason: if is_readonly {
                        "LLM detected as read-only".to_string()
                    } else {
                        "Tool requires user approval".to_string()
                    },
                    confidence: 1.0, // Permission decisions are definitive
                    inspector_name: self.name().to_string(),
                    finding_id: None,
                });
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::CallToolRequestParams;
    use rmcp::object;
    use std::sync::Arc;
    use test_case::test_case;

    async fn inspect_tool(
        mode: GooseMode,
        smart_approved: bool,
        user_permission: Option<PermissionLevel>,
        smart_approve_cache: Option<PermissionLevel>,
    ) -> (InspectionAction, Option<PermissionLevel>) {
        let pm = Arc::new(PermissionManager::new(tempfile::tempdir().unwrap().keep()));
        if let Some(level) = user_permission {
            pm.update_user_permission("tool", level);
        }
        if let Some(level) = smart_approve_cache {
            pm.update_smart_approve_permission("tool", level);
        }
        let session_manager = Arc::new(crate::session::SessionManager::new(
            tempfile::tempdir().unwrap().keep(),
        ));
        let inspector =
            PermissionInspector::new(Arc::clone(&pm), Default::default(), session_manager);
        if smart_approved {
            inspector.apply_tool_annotations(&[Tool::new("tool", "test", serde_json::Map::new())
                .annotate(rmcp::model::ToolAnnotations::new().read_only(true))]);
        }
        let req = ToolRequest {
            id: "req".into(),
            tool_call: Ok(CallToolRequestParams::new("tool").with_arguments(object!({}))),
            metadata: None,
            tool_meta: None,
        };
        let mut results = inspector
            .inspect(goose_test_support::TEST_SESSION_ID, &[req], &[], mode)
            .await
            .unwrap();

        (
            results.remove(0).action,
            pm.get_smart_approve_permission("tool"),
        )
    }

    #[test_case(GooseMode::Auto, false, None, InspectionAction::Allow; "auto_allows")]
    #[test_case(GooseMode::SmartApprove, true, None, InspectionAction::Allow; "smart_approve_annotation_allows")]
    #[test_case(GooseMode::SmartApprove, false, Some(PermissionLevel::AlwaysAllow), InspectionAction::RequireApproval(None); "smart_approve_ignores_legacy_cached_allow")]
    #[test_case(GooseMode::SmartApprove, false, Some(PermissionLevel::AskBefore), InspectionAction::RequireApproval(None); "smart_approve_cached_ask")]
    #[test_case(GooseMode::SmartApprove, false, None, InspectionAction::RequireApproval(None); "smart_approve_unknown_defers")]
    #[test_case(GooseMode::Approve, false, None, InspectionAction::RequireApproval(None); "approve_requires_approval")]
    #[test_case(GooseMode::Approve, false, Some(PermissionLevel::AlwaysAllow), InspectionAction::RequireApproval(None); "approve_ignores_cache")]
    #[test_case(GooseMode::Approve, true, None, InspectionAction::RequireApproval(None); "approve_ignores_annotation")]
    #[tokio::test]
    async fn test_inspect_action(
        mode: GooseMode,
        smart_approved: bool,
        cache: Option<PermissionLevel>,
        expected: InspectionAction,
    ) {
        let (action, _) = inspect_tool(mode, smart_approved, None, cache).await;
        assert_eq!(action, expected);
    }

    #[test_case(PermissionLevel::AlwaysAllow, InspectionAction::Allow; "explicit_allow")]
    #[test_case(PermissionLevel::AskBefore, InspectionAction::RequireApproval(None); "explicit_ask")]
    #[test_case(PermissionLevel::NeverAllow, InspectionAction::Deny; "explicit_deny")]
    #[tokio::test]
    async fn smart_approve_preserves_user_permission_over_legacy_cache(
        user_permission: PermissionLevel,
        expected: InspectionAction,
    ) {
        let (action, cache) = inspect_tool(
            GooseMode::SmartApprove,
            false,
            Some(user_permission),
            Some(PermissionLevel::AlwaysAllow),
        )
        .await;

        assert_eq!(action, expected);
        assert_eq!(cache, Some(PermissionLevel::AlwaysAllow));
    }

    #[tokio::test]
    async fn smart_approve_rejudges_legacy_cached_allow() {
        let (action, cache) = inspect_tool(
            GooseMode::SmartApprove,
            false,
            None,
            Some(PermissionLevel::AlwaysAllow),
        )
        .await;

        assert_eq!(action, InspectionAction::RequireApproval(None));
        assert_eq!(cache, Some(PermissionLevel::AskBefore));
    }

    #[test]
    fn smart_approve_only_caches_negative_name_wide_decisions() {
        let pm = Arc::new(PermissionManager::new(tempfile::tempdir().unwrap().keep()));
        let req = ToolRequest {
            id: "read-request".into(),
            tool_call: Ok(
                CallToolRequestParams::new("multipurpose").with_arguments(object!({
                    "command": "view status",
                })),
            ),
            metadata: None,
            tool_meta: None,
        };

        let policy = PermissionPolicy::new(Some(pm.shared()));
        policy.resolve_classification(&[&req], std::slice::from_ref(&req.id));
        assert_eq!(pm.get_smart_approve_permission("multipurpose"), None);

        policy.resolve_classification(&[&req], &[]);
        assert_eq!(
            pm.get_smart_approve_permission("multipurpose"),
            Some(PermissionLevel::AskBefore)
        );
    }
}
