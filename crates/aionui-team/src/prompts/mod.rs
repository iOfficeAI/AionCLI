pub mod lead;
pub mod team_guide;

pub use team_guide::{TEAM_GUIDE_PROMPT_TEMPLATE, build_team_guide_prompt};
pub mod teammate;

use crate::types::{
    MailboxMessage, MailboxMessageType, TaskStatus, TeamAgent, TeamTask, TeammateRole,
};

pub fn build_lead_prompt(team_name: &str, members: &[TeamAgent]) -> String {
    let mut prompt = String::with_capacity(2048);

    prompt.push_str(&format!(
        "You are the Lead Agent of team \"{team_name}\". \
         Your role is to decompose user requests into tasks, \
         delegate work to teammates, and track progress to completion.\n\n"
    ));

    prompt.push_str("## Team Members\n\n");
    if members.is_empty() {
        prompt.push_str("No teammates available. You must handle all work yourself.\n\n");
    } else {
        for m in members {
            let role_label = match m.role {
                TeammateRole::Lead => "Lead (you)",
                TeammateRole::Teammate => "Teammate",
            };
            prompt.push_str(&format!(
                "- **{}** (slot: `{}`, role: {}, backend: {}, model: {})\n",
                m.name, m.slot_id, role_label, m.backend, m.model,
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str("## Available Tools\n\n");
    prompt.push_str(
        "- `team_send_message(to, message)` — Send a message to a teammate by slotId, \
         or broadcast to all with to=\"*\".\n\
         - `team_spawn_agent(name, backend)` — Dynamically create a new teammate \
         (allowed backends: claude, codex). Lead only.\n\
         - `team_task_create(subject, description?, owner?, blockedBy?)` — \
         Create a task on the task board.\n\
         - `team_task_update(taskId, status?, description?, owner?, blockedBy?)` — \
         Update a task. Set status to \"completed\" when done.\n\
         - `team_task_list()` — List all tasks with their status and dependencies.\n\
         - `team_members()` — List all team members with roles and status.\n\
         - `team_rename_agent(slotId, newName)` — Rename a teammate.\n\
         - `team_shutdown_agent(slotId, reason?)` — Request a teammate to shut down. \
         Lead only.\n\n",
    );

    prompt.push_str("## Workflow Guidelines\n\n");
    prompt.push_str(
        "1. Break down user requests into discrete tasks using `team_task_create`.\n\
         2. Assign tasks to teammates with the `owner` field.\n\
         3. Set up dependencies with `blockedBy` when tasks must run in order.\n\
         4. Send instructions to teammates via `team_send_message`.\n\
         5. Monitor progress — teammates send idle notifications when done.\n\
         6. Mark tasks completed as work is finished.\n\
         7. When all work is complete, send a final summary to the user.\n",
    );

    prompt
}

pub fn build_teammate_prompt(agent: &TeamAgent, team_name: &str) -> String {
    let mut prompt = String::with_capacity(1024);

    prompt.push_str(&format!(
        "You are **{}**, a Teammate Agent in team \"{}\". \
         Your slot ID is `{}`.\n\n",
        agent.name, team_name, agent.slot_id,
    ));

    prompt.push_str("## Your Role\n\n");
    prompt.push_str(
        "You execute tasks assigned by the Lead Agent. Focus on completing your \
         assigned work thoroughly and reporting back.\n\n",
    );

    prompt.push_str("## Communication Protocol\n\n");
    prompt.push_str(
        "- Use `team_send_message` to report progress or ask questions to the Lead.\n\
         - Use `team_task_update` to update task status as you work \
         (pending → in_progress → completed).\n\
         - When your assigned work is done, send an idle notification. \
         The system will notify the Lead.\n\
         - If you receive a `shutdown_request`, finish any critical work, \
         then respond with \"shutdown_approved\" or \"shutdown_rejected: <reason>\".\n",
    );

    prompt
}

pub fn build_wake_payload(
    agent: &TeamAgent,
    tasks: &[TeamTask],
    unread_messages: &[MailboxMessage],
) -> String {
    let mut payload = String::with_capacity(2048);

    if !unread_messages.is_empty() {
        payload.push_str("## New Messages\n\n");
        for msg in unread_messages {
            let type_label = match msg.msg_type {
                MailboxMessageType::Message => "message",
                MailboxMessageType::IdleNotification => "idle_notification",
                MailboxMessageType::ShutdownRequest => "shutdown_request",
            };
            payload.push_str(&format!(
                "- From `{}` [{}]: {}\n",
                msg.from_agent_id, type_label, msg.content,
            ));
            if let Some(ref summary) = msg.summary {
                payload.push_str(&format!("  Summary: {summary}\n"));
            }
        }
        payload.push('\n');
    } else {
        payload.push_str("## New Messages\n\nNo new messages.\n\n");
    }

    if !tasks.is_empty() {
        payload.push_str("## Current Task Board\n\n");
        payload.push_str("| ID | Subject | Status | Owner | Blocked By |\n");
        payload.push_str("|---|---|---|---|---|\n");
        for task in tasks {
            let status = match task.status {
                TaskStatus::Pending => "pending",
                TaskStatus::InProgress => "in_progress",
                TaskStatus::Completed => "completed",
                TaskStatus::Deleted => "deleted",
            };
            let owner = task.owner.as_deref().unwrap_or("-");
            let blocked = if task.blocked_by.is_empty() {
                "-".to_owned()
            } else {
                task.blocked_by.join(", ")
            };
            let short_id = if task.id.len() > 8 {
                &task.id[..8]
            } else {
                &task.id
            };
            payload.push_str(&format!(
                "| {short_id}… | {} | {status} | {owner} | {blocked} |\n",
                task.subject,
            ));
        }
        payload.push('\n');
    } else {
        payload.push_str("## Current Task Board\n\nNo tasks on the board.\n\n");
    }

    payload.push_str(&format!(
        "You are **{}** (role: {}). Proceed with your work.\n",
        agent.name, agent.role,
    ));

    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_lead() -> TeamAgent {
        TeamAgent {
            slot_id: "lead-1".into(),
            name: "Lead".into(),
            role: TeammateRole::Lead,
            conversation_id: "conv-1".into(),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
            status: None,
            conversation_type: None,
            cli_path: None,
        }
    }

    fn make_teammate(slot_id: &str, name: &str) -> TeamAgent {
        TeamAgent {
            slot_id: slot_id.into(),
            name: name.into(),
            role: TeammateRole::Teammate,
            conversation_id: format!("conv-{slot_id}"),
            backend: "acp".into(),
            model: "claude".into(),
            custom_agent_id: None,
            status: None,
            conversation_type: None,
            cli_path: None,
        }
    }

    fn make_task(id: &str, subject: &str, status: TaskStatus) -> TeamTask {
        TeamTask {
            id: id.into(),
            team_id: "t1".into(),
            subject: subject.into(),
            description: None,
            status,
            owner: Some("worker-1".into()),
            blocked_by: vec![],
            blocks: vec![],
            metadata: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn make_message(from: &str, content: &str, msg_type: MailboxMessageType) -> MailboxMessage {
        MailboxMessage {
            id: "msg-1".into(),
            team_id: "t1".into(),
            to_agent_id: "lead-1".into(),
            from_agent_id: from.into(),
            msg_type,
            content: content.into(),
            summary: None,
            read: false,
            created_at: 0,
        }
    }

    // -- Lead prompt ----------------------------------------------------------

    #[test]
    fn lead_prompt_contains_team_name() {
        let prompt = build_lead_prompt("Alpha", &[]);
        assert!(prompt.contains("\"Alpha\""));
    }

    #[test]
    fn lead_prompt_contains_member_list() {
        let members = vec![make_lead(), make_teammate("w1", "Worker1")];
        let prompt = build_lead_prompt("Alpha", &members);

        assert!(prompt.contains("**Lead**"));
        assert!(prompt.contains("slot: `lead-1`"));
        assert!(prompt.contains("**Worker1**"));
        assert!(prompt.contains("slot: `w1`"));
        assert!(prompt.contains("Lead (you)"));
        assert!(prompt.contains("Teammate"));
    }

    #[test]
    fn lead_prompt_contains_tool_descriptions() {
        let prompt = build_lead_prompt("Alpha", &[]);

        assert!(prompt.contains("team_send_message"));
        assert!(prompt.contains("team_spawn_agent"));
        assert!(prompt.contains("team_task_create"));
        assert!(prompt.contains("team_task_update"));
        assert!(prompt.contains("team_task_list"));
        assert!(prompt.contains("team_members"));
        assert!(prompt.contains("team_rename_agent"));
        assert!(prompt.contains("team_shutdown_agent"));
    }

    #[test]
    fn lead_prompt_contains_workflow_guidelines() {
        let prompt = build_lead_prompt("Alpha", &[]);
        assert!(prompt.contains("Workflow Guidelines"));
        assert!(prompt.contains("Break down user requests"));
    }

    #[test]
    fn lead_prompt_no_members_shows_solo_message() {
        let prompt = build_lead_prompt("Solo", &[]);
        assert!(prompt.contains("No teammates available"));
    }

    // -- Teammate prompt ------------------------------------------------------

    #[test]
    fn teammate_prompt_contains_agent_identity() {
        let agent = make_teammate("w1", "Worker1");
        let prompt = build_teammate_prompt(&agent, "Alpha");

        assert!(prompt.contains("**Worker1**"));
        assert!(prompt.contains("\"Alpha\""));
        assert!(prompt.contains("`w1`"));
    }

    #[test]
    fn teammate_prompt_contains_communication_protocol() {
        let agent = make_teammate("w1", "Worker1");
        let prompt = build_teammate_prompt(&agent, "Alpha");

        assert!(prompt.contains("team_send_message"));
        assert!(prompt.contains("team_task_update"));
        assert!(prompt.contains("idle notification"));
        assert!(prompt.contains("shutdown_request"));
        assert!(prompt.contains("shutdown_approved"));
    }

    #[test]
    fn teammate_prompt_contains_team_name() {
        let agent = make_teammate("w1", "W");
        let prompt = build_teammate_prompt(&agent, "Beta Team");
        assert!(prompt.contains("\"Beta Team\""));
    }

    // -- Wake payload ---------------------------------------------------------

    #[test]
    fn wake_payload_with_messages() {
        let agent = make_lead();
        let msgs = vec![make_message(
            "w1",
            "Task A done",
            MailboxMessageType::Message,
        )];
        let payload = build_wake_payload(&agent, &[], &msgs);

        assert!(payload.contains("New Messages"));
        assert!(payload.contains("`w1`"));
        assert!(payload.contains("[message]"));
        assert!(payload.contains("Task A done"));
    }

    #[test]
    fn wake_payload_with_idle_notification() {
        let agent = make_lead();
        let mut msg = make_message("w1", "idle", MailboxMessageType::IdleNotification);
        msg.summary = Some("Finished feature X".into());
        let payload = build_wake_payload(&agent, &[], &[msg]);

        assert!(payload.contains("[idle_notification]"));
        assert!(payload.contains("Summary: Finished feature X"));
    }

    #[test]
    fn wake_payload_with_shutdown_request() {
        let agent = make_teammate("w1", "W");
        let msg = make_message(
            "lead-1",
            "No longer needed",
            MailboxMessageType::ShutdownRequest,
        );
        let payload = build_wake_payload(&agent, &[], &[msg]);

        assert!(payload.contains("[shutdown_request]"));
        assert!(payload.contains("No longer needed"));
    }

    #[test]
    fn wake_payload_with_tasks() {
        let agent = make_lead();
        let tasks = vec![
            make_task(
                "aaaaaaaa-1234-5678-9abc-def012345678",
                "Implement X",
                TaskStatus::InProgress,
            ),
            make_task(
                "bbbbbbbb-1234-5678-9abc-def012345678",
                "Test Y",
                TaskStatus::Pending,
            ),
        ];
        let payload = build_wake_payload(&agent, &tasks, &[]);

        assert!(payload.contains("Current Task Board"));
        assert!(payload.contains("Implement X"));
        assert!(payload.contains("in_progress"));
        assert!(payload.contains("Test Y"));
        assert!(payload.contains("pending"));
        assert!(payload.contains("aaaaaaaa…"));
    }

    #[test]
    fn wake_payload_with_task_dependencies() {
        let agent = make_lead();
        let mut task = make_task(
            "cccccccc-1234-5678-9abc-def012345678",
            "Deploy",
            TaskStatus::Pending,
        );
        task.blocked_by = vec!["task-a".into(), "task-b".into()];
        let payload = build_wake_payload(&agent, &[task], &[]);

        assert!(payload.contains("task-a, task-b"));
    }

    #[test]
    fn wake_payload_empty() {
        let agent = make_lead();
        let payload = build_wake_payload(&agent, &[], &[]);

        assert!(payload.contains("No new messages"));
        assert!(payload.contains("No tasks on the board"));
        assert!(payload.contains("**Lead**"));
    }

    #[test]
    fn wake_payload_contains_agent_identity() {
        let agent = make_teammate("w1", "Worker1");
        let payload = build_wake_payload(&agent, &[], &[]);

        assert!(payload.contains("**Worker1**"));
        assert!(payload.contains("teammate"));
    }

    #[test]
    fn wake_payload_short_task_id_no_truncation() {
        let agent = make_lead();
        let task = make_task("short", "Short ID Task", TaskStatus::Pending);
        let payload = build_wake_payload(&agent, &[task], &[]);
        assert!(payload.contains("short…"));
    }
}
