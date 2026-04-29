---
name: update-task-context
description: Update the shared task context file with current progress
user-invocable: true
---

The path to the shared task context file is provided in the session's startup system reminder, on the line beginning with "A shared task context file exists at ".

Read that file, then rewrite it as a clean, consolidated version that incorporates your current progress and knowledge.

Rules:
- The first line MUST be a markdown heading
- Maintain a clear summary of the task goal, what has been done, and what is known
- Include anything useful for other agents picking up this task
- Remove outdated info, keep it concise
- Do NOT include commentary or meta-text about the update itself
