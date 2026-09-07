
//! ResourceHandler: the `ntulearn://courses/{course_id}` briefing template.
//! Parity with the Python resource template + read_resource.

use std::sync::Arc;

use serde_json::{json, Value};
use ultrafast_mcp::{
    types::resources::{ListResourceTemplatesRequest, ListResourceTemplatesResponse},
    types::roots::{Root, RootOperation},
    ListResourcesRequest, ListResourcesResponse, MCPResult, ReadResourceRequest,
    ReadResourceResponse, ResourceContent, ResourceHandler, ResourceTemplate,
};

use crate::AppState;

pub struct NtuResourceHandler {
    pub state: Arc<AppState>,
}

const TEMPLATE: &str = "ntulearn://courses/{course_id}";

impl NtuResourceHandler {
    async fn build_briefing(&self, course_id: &str) -> MCPResult<String> {
        let state = &self.state;
        let course = state
            .client
            .get_json(
                &format!("/learn/api/public/v1/courses/{course_id}"),
                &[("fields", "id,courseId,name,description,allowGuests,availability.created,termId")],
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let contents = state
            .client
            .get_json(
                &format!("/learn/api/public/v1/courses/{course_id}/contents"),
                &[("limit", "1"), ("fields", "id,title")],
                Some(std::time::Duration::from_secs(30)),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let content_count = contents
            .get("results")
            .and_then(|r| r.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        let briefing = json!({
            "course_id": course_id,
            "title": course.get("name").and_then(Value::as_str).unwrap_or("?"),
            "description": course.get("description").and_then(Value::as_str),
            "available": course.get("availability").and_then(|a| a.get("available")).and_then(Value::as_str),
            "top_level_content_items": content_count,
            "note": "Full briefing parity lands with ntulearn_summarize_course (rust port milestone 2).",
        });
        Ok(serde_json::to_string_pretty(&briefing).unwrap_or_default())
    }
}

#[async_trait::async_trait]
impl ResourceHandler for NtuResourceHandler {
    async fn read_resource(&self, request: ReadResourceRequest) -> MCPResult<ReadResourceResponse> {
        let uri = request.uri;
        if let Some(id) = uri.strip_prefix("ntulearn://courses/") {
            let briefing = self.build_briefing(id).await?;
            Ok(ReadResourceResponse {
                contents: vec![ResourceContent::text(uri.clone(), briefing)],
            })
        } else {
            Err(anyhow::anyhow!("Unknown resource: {uri}").into())
        }
    }

    async fn list_resources(&self, _request: ListResourcesRequest) -> MCPResult<ListResourcesResponse> {
        Ok(ListResourcesResponse { resources: vec![], next_cursor: None })
    }

    async fn list_resource_templates(
        &self,
        _request: ListResourceTemplatesRequest,
    ) -> MCPResult<ListResourceTemplatesResponse> {
        Ok(ListResourceTemplatesResponse {
            resource_templates: vec![ResourceTemplate {
                uri_template: TEMPLATE.to_string(),
                name: "ntulearn-course-briefing".to_string(),
                description: Some(
                    "JSON course briefing for a single NTULearn course (content rebuilt from the \
                     ntulearn_summarize_course tool). Replace {course_id} with a Blackboard course \
                     ID such as _12345_1."
                        .to_string(),
                ),
                mime_type: Some("application/json".to_string()),
            }],
            next_cursor: None,
        })
    }

    async fn validate_resource_access(
        &self,
        _uri: &str,
        _operation: RootOperation,
        _roots: &[Root],
    ) -> MCPResult<()> {
        Ok(())
    }
}
