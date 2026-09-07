
//! PromptHandler: the two built-in templates (weekly brief + assignment triage),
//! ported 1:1 from the Python server.

use ultrafast_mcp::{
    types::prompts::PromptMessage,
    GetPromptRequest, GetPromptResponse, ListPromptsRequest, ListPromptsResponse, MCPResult,
    Prompt, PromptArgument, PromptContent, PromptHandler, PromptRole,
};

fn iso_offset(days: i64) -> String {
    let now = chrono::Utc::now() + chrono::Duration::days(days);
    now.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn prompt_course_arg(courses: Option<&str>) -> String {
    let Some(c) = courses else { return String::new() };
    let ids: Vec<String> = c.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if ids.is_empty() { return String::new(); }
    let ids_json = serde_json::Value::Array(ids.into_iter().map(serde_json::Value::String).collect());
    format!(", course_ids={}", ids_json.to_string())
}

fn user_msg(text: String) -> PromptMessage {
    PromptMessage {
        role: PromptRole::User,
        content: PromptContent::text(text),
    }
}

pub struct NtuPromptHandler;

#[async_trait::async_trait]
impl PromptHandler for NtuPromptHandler {
    async fn list_prompts(&self, _req: ListPromptsRequest) -> MCPResult<ListPromptsResponse> {
        Ok(ListPromptsResponse {
            prompts: vec![
                Prompt::new("ntulearn-weekly-brief".to_string())
                    .with_description(
                        "Generate a weekly digest of announcements and upcoming due dates across \
                         your courses. Chains ntulearn_get_announcements and ntulearn_get_upcoming \
                         with a computed since/until window.".to_string(),
                    )
                    .with_arguments(vec![
                        PromptArgument::new("courses".to_string())
                            .with_description(
                                "Optional comma-separated course IDs to scope to. Omit for all \
                                 enrolled courses.".to_string(),
                            )
                            .required(false),
                        PromptArgument::new("days".to_string())
                            .with_description(
                                "Days back/forward in the window (default 7).".to_string(),
                            )
                            .required(false),
                    ]),
                Prompt::new("ntulearn-assignment-triage".to_string())
                    .with_description(
                        "Prioritise your upcoming assignment due dates over the next N days. \
                         Chains ntulearn_get_upcoming (type='GradebookColumn') plus a grades lookup \
                         to suggest what to tackle first.".to_string(),
                    )
                    .with_arguments(vec![
                        PromptArgument::new("courses".to_string())
                            .with_description(
                                "Optional comma-separated course IDs to scope to. Omit for all \
                                 enrolled courses.".to_string(),
                            )
                            .required(false),
                        PromptArgument::new("days".to_string())
                            .with_description(
                                "How far ahead to look for due dates (default 14).".to_string(),
                            )
                            .required(false),
                    ]),
            ],
            next_cursor: None,
        })
    }

    async fn get_prompt(&self, request: GetPromptRequest) -> MCPResult<GetPromptResponse> {
        let args = request.arguments.unwrap_or_default();
        let courses = args.get("courses").and_then(|v| v.as_str());
        match request.name.as_str() {
            "ntulearn-weekly-brief" => {
                let days = args.get("days").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(7);
                if days < 1 {
                    return Err(anyhow::anyhow!("days must be >= 1").into());
                }
                let since = iso_offset(-days);
                let until = iso_offset(0);
                let course_arg = prompt_course_arg(courses);
                let text = format!(
                    "Produce a weekly NTULearn brief covering the last/looking {days} days. Run \
                     these tools and combine the results:\n\
                     1. ntulearn_get_announcements(since='{since}'{course_arg}) — recent \
                     announcements.\n\
                     2. ntulearn_get_upcoming(since='{since}', until='{until}'{course_arg}) — \
                     upcoming calendar items and due dates.\n\
                     Summarise: what's new in each course, what's due, and any deadlines the user \
                     should watch in the next few days."
                );
                Ok(GetPromptResponse {
                    description: Some("Weekly NTULearn brief".to_string()),
                    messages: vec![user_msg(text)],
                })
            }
            "ntulearn-assignment-triage" => {
                let days = args.get("days").and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()).unwrap_or(14);
                if days < 1 {
                    return Err(anyhow::anyhow!("days must be >= 1").into());
                }
                let until = iso_offset(days);
                let course_arg = prompt_course_arg(courses);
                let ids: Vec<String> = courses.unwrap_or("").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                let gradebook_arg = if ids.is_empty() {
                    String::new()
                } else {
                    let j = serde_json::Value::Array(ids.into_iter().map(serde_json::Value::String).collect());
                    format!("course_ids={}", j.to_string())
                };
                let text = format!(
                    "Running an assignment triage for the next {days} days. Run these tools and \
                     combine the results:\n\
                     1. ntulearn_get_upcoming(type='GradebookColumn', until='{until}'{course_arg}) \
                     — assignment due dates.\n\
                     2. ntulearn_get_gradebook({gradebook_arg}) — current scores for context.\n\
                     For each due assignment, list: course, title, due date, how much time is left, \
                     and a recommended order of attack (closest deadline first, biggest weight first)."
                );
                Ok(GetPromptResponse {
                    description: Some("NTULearn assignment triage".to_string()),
                    messages: vec![user_msg(text)],
                })
            }
            other => Err(anyhow::anyhow!("Unknown prompt: {other}").into()),
        }
    }
}
