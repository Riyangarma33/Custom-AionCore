use aionui_api_types::{SpecTask, SpecWave, TaskStatus};
use crate::specs::types::SpecError;

/// Parse a `tasks.md` markdown file into structured execution waves and tasks.
pub fn parse_tasks_document(content: &str) -> Vec<SpecWave> {
    let mut waves: Vec<SpecWave> = Vec::new();
    let mut current_wave: Option<SpecWave> = None;
    let mut current_task: Option<SpecTask> = None;
    let mut auto_task_counter = 0usize;
    let mut in_code_fence = false;

    let lines: Vec<&str> = content.lines().collect();

    for (line_idx, line) in lines.iter().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = line.trim();

        // Track markdown code blocks to avoid parsing code examples as tasks
        if trimmed.starts_with("```") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        // Check for wave headings: e.g. "### Wave 1: Foundation", "## Wave 2 - Core", "# Wave 3"
        if let Some((wave_num, wave_title)) = parse_wave_heading(trimmed) {
            // Flush existing task and wave
            if let Some(task) = current_task.take() {
                if let Some(w) = current_wave.as_mut() {
                    w.tasks.push(task);
                }
            }
            if let Some(w) = current_wave.take() {
                waves.push(w);
            }

            current_wave = Some(SpecWave {
                number: wave_num,
                title: wave_title,
                tasks: Vec::new(),
            });
            continue;
        }

        // Check for task checkbox: e.g. "- [ ] 1. Do something (Refs: REQ-1)"
        if let Some((indent, status, task_body)) = parse_checkbox_line(line) {
            // Flush previous task to current wave
            if let Some(prev_task) = current_task.take() {
                if let Some(w) = current_wave.as_mut() {
                    w.tasks.push(prev_task);
                } else {
                    current_wave = Some(SpecWave {
                        number: 1,
                        title: "Wave 1: Foundation".to_string(),
                        tasks: vec![prev_task],
                    });
                }
            }

            auto_task_counter += 1;
            let current_wave_num = current_wave.as_ref().map(|w| w.number).unwrap_or(1);
            let (task_id, parent_id, title, refs) = parse_task_body(&task_body, current_wave_num, auto_task_counter);

            current_task = Some(SpecTask {
                id: task_id,
                line_number: line_num,
                status,
                title,
                description: None,
                wave: Some(current_wave_num),
                parent_id,
                refs,
                indent,
            });
            continue;
        }

        // Indented description or notes belonging to the current task
        if let Some(task) = current_task.as_mut() {
            if !trimmed.is_empty() && (line.starts_with("  ") || line.starts_with('\t')) {
                let note = trimmed.to_string();
                match &mut task.description {
                    Some(desc) => {
                        desc.push('\n');
                        desc.push_str(&note);
                    }
                    None => {
                        task.description = Some(note);
                    }
                }
            }
        }
    }

    // Flush final task and wave
    if let Some(task) = current_task.take() {
        if let Some(w) = current_wave.as_mut() {
            w.tasks.push(task);
        } else {
            current_wave = Some(SpecWave {
                number: 1,
                title: "Wave 1: Foundation".to_string(),
                tasks: vec![task],
            });
        }
    }
    if let Some(w) = current_wave.take() {
        waves.push(w);
    }

    // Reconcile parent_id based on task hierarchy if not already set
    for wave in &mut waves {
        reconcile_hierarchy(&mut wave.tasks);
    }

    waves
}

/// Mutates a task's checkbox status in-place inside `raw_content`.
///
/// Guarantees 100% byte-identical formatting preservation across the rest of the
/// file: indentation, surrounding text, linebreaks (\r\n vs \n), comments, and notes
/// are untouched.
pub fn update_task_status(
    raw_content: &str,
    task_id: &str,
    new_status: TaskStatus,
) -> Result<(String, SpecTask), SpecError> {
    let waves = parse_tasks_document(raw_content);
    let target_task = waves
        .iter()
        .flat_map(|w| &w.tasks)
        .find(|t| t.id == task_id || t.id.trim_end_matches('.') == task_id.trim_end_matches('.'))
        .cloned()
        .ok_or_else(|| SpecError::TaskNotFound {
            id: task_id.to_string(),
        })?;

    let target_line = target_task.line_number; // 1-based line number

    let new_char = match new_status {
        TaskStatus::Todo => ' ',
        TaskStatus::InProgress => '-',
        TaskStatus::Completed => 'x',
    };

    // Find the exact line in raw_content while strictly preserving line breaks
    let mut current_line = 1usize;
    let mut line_start = 0usize;
    let mut target_line_span: Option<(usize, usize)> = None;

    let bytes = raw_content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            if current_line == target_line {
                target_line_span = Some((line_start, i));
                break;
            }
            current_line += 1;
            line_start = i + 1;
        }
        i += 1;
    }
    if target_line_span.is_none() && current_line == target_line {
        target_line_span = Some((line_start, bytes.len()));
    }

    let (start_idx, end_idx) = target_line_span.ok_or_else(|| SpecError::TaskNotFound {
        id: task_id.to_string(),
    })?;

    let line_str = &raw_content[start_idx..end_idx];

    // Find the checkbox brackets [ ] on this line
    let open_bracket_offset = line_str.find('[').ok_or_else(|| SpecError::TaskNotFound {
        id: task_id.to_string(),
    })?;
    let close_bracket_offset = line_str[open_bracket_offset..]
        .find(']')
        .map(|idx| open_bracket_offset + idx)
        .ok_or_else(|| SpecError::TaskNotFound {
            id: task_id.to_string(),
        })?;

    // Checkbox must have at least one character inside brackets, e.g. [ ], [-], [x]
    if close_bracket_offset != open_bracket_offset + 2 {
        return Err(SpecError::TaskNotFound {
            id: task_id.to_string(),
        });
    }

    let char_byte_pos = start_idx + open_bracket_offset + 1;

    let mut new_content = raw_content.to_string();
    new_content.replace_range(char_byte_pos..char_byte_pos + 1, &new_char.to_string());

    let mut updated_task = target_task;
    updated_task.status = new_status;

    Ok((new_content, updated_task))
}

// ── Private helpers ────────────────────────────────────────────────────────

fn parse_wave_heading(line: &str) -> Option<(usize, String)> {
    if !line.starts_with('#') {
        return None;
    }
    let heading_text = line.trim_start_matches('#').trim();
    if !heading_text.to_lowercase().starts_with("wave") {
        return None;
    }

    let remainder = heading_text[4..].trim();
    // remainder might be "1: Foundation", "1 - Setup", or just "1"
    let mut parts = remainder.splitn(2, |c: char| c == ':' || c == '-' || c == '—');
    let num_str = parts.next()?.trim();
    let wave_num: usize = num_str.parse().ok()?;

    let title_part = parts.next().map(|s| s.trim()).unwrap_or("");
    let wave_title = if title_part.is_empty() {
        format!("Wave {wave_num}")
    } else {
        format!("Wave {wave_num}: {title_part}")
    };

    Some((wave_num, wave_title))
}

fn parse_checkbox_line(line: &str) -> Option<(usize, TaskStatus, String)> {
    let indent = line.chars().take_while(|c| c.is_whitespace()).count();
    let trimmed = line.trim_start();

    // Must start with list marker: '-', '*', or '+'
    if !(trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ")) {
        return None;
    }

    let after_marker = trimmed[2..].trim_start();
    if !after_marker.starts_with('[') {
        return None;
    }

    let close_idx = after_marker.find(']')?;
    let marker_inside = after_marker[1..close_idx].trim();

    let status = match marker_inside {
        "" | " " => TaskStatus::Todo,
        "-" | "/" => TaskStatus::InProgress,
        "x" | "X" => TaskStatus::Completed,
        _ => return None,
    };

    let task_body = after_marker[close_idx + 1..].trim().to_string();
    Some((indent, status, task_body))
}

fn parse_task_body(
    body: &str,
    wave_num: usize,
    counter: usize,
) -> (String, Option<String>, String, Vec<String>) {
    let mut text = body.trim().to_string();

    // Extract refs: e.g. "(Refs: REQ-1, REQ-2)" or "[Refs: REQ-1]"
    let mut refs = Vec::new();
    if let Some(start) = text.rfind("(Refs:") {
        if let Some(end) = text[start..].find(')') {
            let ref_str = &text[start + 6..start + end];
            for r in ref_str.split(',') {
                let trimmed = r.trim();
                if !trimmed.is_empty() {
                    refs.push(trimmed.to_string());
                }
            }
            text = format!("{}{}", &text[..start], &text[start + end + 1..])
                .trim()
                .to_string();
        }
    } else if let Some(start) = text.rfind("[Refs:") {
        if let Some(end) = text[start..].find(']') {
            let ref_str = &text[start + 6..start + end];
            for r in ref_str.split(',') {
                let trimmed = r.trim();
                if !trimmed.is_empty() {
                    refs.push(trimmed.to_string());
                }
            }
            text = format!("{}{}", &text[..start], &text[start + end + 1..])
                .trim()
                .to_string();
        }
    }

    // Try to extract hierarchical number prefix: "1.", "1.1", "1.1.", "1.2.3"
    let prefix_info = {
        let first_token = text.split_whitespace().next().unwrap_or("");
        let clean_token = first_token.trim_end_matches(['.', ':', '-']);
        let is_numeric = !clean_token.is_empty()
            && clean_token
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));

        if is_numeric {
            let id = clean_token.to_string();
            let parent = clean_token.rfind('.').map(|last_dot| clean_token[..last_dot].to_string());
            Some((id, parent, first_token.len()))
        } else {
            None
        }
    };

    let (task_id, parent_id) = match prefix_info {
        Some((id, parent, prefix_len)) => {
            text = text[prefix_len..].trim().to_string();
            (id, parent)
        }
        None => (format!("{wave_num}.{counter}"), None),
    };

    (task_id, parent_id, text, refs)
}

fn reconcile_hierarchy(tasks: &mut [SpecTask]) {
    // If a subtask has higher indentation than preceding task, and no parent_id,
    // bind it to the preceding task of lower indentation
    let mut last_top_level: Option<String> = None;
    for task in tasks.iter_mut() {
        if task.indent == 0 {
            last_top_level = Some(task.id.clone());
        } else if task.parent_id.is_none() {
            task.parent_id = last_top_level.clone();
        }
    }
}
