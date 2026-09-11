//! Session-event handling: the one ordered stream from the agent —
//! text, thinking, tool lifecycle, usage, retries — projected onto App
//! state and the transcript.

use super::*;

impl App {
    /// The single session stream, in order. Turn bookkeeping hangs off it.
    pub(super) fn on_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Discarded(prompts) => {
                for text in prompts {
                    self.notice(format!(
                        "queued message discarded when the run stopped: {text}"
                    ));
                }
            }
            SessionEvent::Compacting => {
                self.compacting = true;
                self.end_thinking_burst();
                self.end_assistant_burst();
                self.notice("compacting…".into());
            }
            SessionEvent::Compacted {
                summary,
                context_tokens,
            } => {
                self.compacting = false;
                self.context_tokens = context_tokens;
                // Compaction changes model context, not the user's scrollback.
                // Keep prior tool details and their live block references valid.
                self.transcript.push(Block::new(
                    Kind::Notice,
                    "compacted — recent messages kept, the full session is under /resume",
                ));
                self.transcript.push(Block::new(Kind::Summary, summary));
            }
            SessionEvent::TurnStart => {
                self.active = Some(ActiveTurn {
                    block: None,
                    thinking_block: None,
                    turn: Turn::new(),
                    started: Instant::now(),
                    error: None,
                    error_summary: None,
                    sleep_stopped: false,
                    tool_blocks: std::collections::HashMap::new(),
                    pending_tools: 0,
                    cost_usd: self.agent.model.pricing.as_ref().map(|_| 0.0),
                });
            }
            SessionEvent::Steered(text) => {
                // A mid-turn message: show it as a user turn where it landed.
                self.transcript.push(Block::new(Kind::User, text));
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Waiting;
                }
                // The next assistant text opens a fresh block; the burst
                // that was live stays expanded where it sat.
                self.end_thinking_burst();
                self.end_assistant_burst();
            }
            SessionEvent::TextDelta(delta) => {
                // Reply text starting ends the live thinking burst — the
                // thought stays expanded above the reply; the next burst,
                // if any, opens its own block.
                self.end_thinking_burst();
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::AssistantText;
                    // Model output is untrusted: strip control sequences
                    // before it can reach the paint stream. (The raw text
                    // still goes to the model's own history in core.)
                    let idx = open_block(&mut self.transcript, &mut s.block, Kind::Assistant);
                    if let Some(block) = self.transcript.blocks.get_mut(idx) {
                        block.append_streaming(&delta);
                    }
                }
            }
            // Reasoning streams live in thinkingText while the burst runs;
            // the completed burst stays expanded. Raw provider text is
            // stripped before it can reach the paint stream, like reply text.
            SessionEvent::ReasoningDelta(delta) => {
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Thinking;
                    if self.show_thinking {
                        let idx =
                            open_block(&mut self.transcript, &mut s.thinking_block, Kind::Thinking);
                        if let Some(block) = self.transcript.blocks.get_mut(idx) {
                            block.append_streaming(&delta);
                        }
                    }
                }
            }
            SessionEvent::ToolCallAssembly { bytes: _ } => {
                // The model is streaming tool-call arguments. The tool row
                // appears only when the complete call starts executing.
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::ToolCall;
                }
            }
            SessionEvent::ToolBatchStart { calls } => {
                // End the pre-batch reasoning where it sits. A tool tree
                // continues only when no reply or expanded thinking
                // separates this batch from the previous one.
                self.end_thinking_burst();
                self.end_assistant_burst();
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Tool;
                    s.pending_tools += calls.len();
                    let children = calls
                        .iter()
                        .map(|call| {
                            crate::tui::transcript::ToolChild::pending(
                                call.id,
                                call.category.clone(),
                                call.running.clone(),
                                call.completed.clone(),
                                call.target.clone(),
                            )
                        })
                        .collect();
                    let idx = self.transcript.extend_tool_group(children);
                    self.transcript.blocks[idx].live_preview_rows = self.live_preview_rows;
                    self.transcript.blocks[idx].tool_label_rows = self.tool_label_rows;
                    for call in calls {
                        s.tool_blocks.insert(call.id, idx);
                    }
                }
            }
            SessionEvent::ToolStart { id } => {
                if let Some(s) = &mut self.active {
                    if let Some(&idx) = s.tool_blocks.get(&id) {
                        s.turn.phase = TurnPhase::Tool;
                        if let Some(block) = self.transcript.blocks.get_mut(idx) {
                            block.start_tool(id);
                        }
                    }
                }
            }
            SessionEvent::ToolOutput { id, chunk, .. } => {
                if let Some(s) = &mut self.active {
                    if let Some(&idx) = s.tool_blocks.get(&id) {
                        if let Some(block) = self.transcript.blocks.get_mut(idx) {
                            block.append_tool_output(id, &chunk);
                        }
                    }
                }
            }
            SessionEvent::Named(name) => {
                self.agent.set_session_name(name.clone());
                self.notice(format!("session: {name}"));
                set_tab_title(&tab_title(&title_path(), Some(&name)));
            }
            SessionEvent::ToolEnd {
                id,
                outcome,
                summary,
                content,
            } => {
                // Detached tools can finish after a new turn has started.
                // Their events must not change that turn or its saved outputs.
                if !self
                    .active
                    .as_ref()
                    .is_some_and(|s| s.tool_blocks.contains_key(&id))
                {
                    return;
                }
                let mut title = None;
                if let Some(s) = &mut self.active {
                    if let Some(&idx) = s.tool_blocks.get(&id) {
                        if let Some(block) = self.transcript.blocks.get_mut(idx) {
                            if let Some(child) =
                                block.tool_children.iter().find(|child| child.id == id)
                            {
                                title = Some(if child.target.is_empty() {
                                    child.completed.clone()
                                } else {
                                    format!("{} {}", child.completed, child.target)
                                });
                            }
                            block.finish_tool(id, outcome, summary, &content);
                        }
                    }
                    s.pending_tools = s.pending_tools.saturating_sub(1);
                    s.turn.phase = if s.pending_tools == 0 {
                        TurnPhase::Waiting
                    } else {
                        TurnPhase::Tool
                    };
                }
                if !content.trim().is_empty() {
                    let detail = self.remember_output(
                        title.unwrap_or_else(|| "tool output".into()),
                        crate::core::tools::sanitize_display(&content),
                    );
                    // Link the stored detail to its row for the review
                    // screen.
                    if let Some(s) = &self.active {
                        if let Some(&idx) = s.tool_blocks.get(&id) {
                            if let Some(block) = self.transcript.blocks.get_mut(idx) {
                                if let Some(child) =
                                    block.tool_children.iter_mut().find(|child| child.id == id)
                                {
                                    child.detail = Some(detail);
                                }
                            }
                        }
                    }
                }
            }
            SessionEvent::Usage {
                input,
                output,
                cache_read,
            } => {
                // `input` is the inclusive prompt total per the Usage
                // contract — adding the cached subset again would double
                // count and trigger compaction early.
                self.context_tokens = input.saturating_add(output);
                if let Some(s) = &mut self.active {
                    if let (Some(total), Some(pricing)) =
                        (&mut s.cost_usd, &self.agent.model.pricing)
                    {
                        *total += pricing.estimate(input, output, cache_read);
                    }
                    // Every step resends the whole context, so `input` is the
                    // latest request's size, not new work — summing it across
                    // steps re-counted the same tokens once per step and
                    // showed absurd totals for long tool loops. Latest wins
                    // (displacing the seed estimate); only `output` — the
                    // tokens each step actually generated — accumulates, and
                    // the live chars/4 estimate resets to cover only what the
                    // next step streams.
                    s.turn.note_usage(input, output);
                }
            }
            SessionEvent::Retry {
                attempt,
                limit,
                delay_secs,
                cause,
                reason,
            } => {
                // Replaces the activity row in place — a live status, not a
                // scrollback notice: it's transient by nature and would
                // otherwise leave one permanent line per attempt behind.
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Retrying;
                    s.turn.retry = Some(RetryStatus {
                        attempt,
                        limit,
                        delay_secs,
                        since: Instant::now(),
                        cause,
                        reason,
                    });
                    s.turn.recovered = None;
                }
                // The abandoned attempt's thinking burst ends where it sits;
                // the retry streams a fresh burst.
                self.end_thinking_burst();
            }
            SessionEvent::Recovered { attempt, limit } => {
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Waiting;
                    s.turn.retry = None;
                    s.turn.recovered = Some(RecoveredStatus {
                        attempt,
                        limit,
                        since: Instant::now(),
                    });
                }
            }
            SessionEvent::ErrorDetails(details) => {
                if let Some(s) = &mut self.active {
                    s.error_summary = Some(details.summary);
                }
            }
            SessionEvent::Error(message) => {
                if let Some(s) = &mut self.active {
                    s.error = Some(s.error_summary.take().unwrap_or(message));
                } else {
                    self.notice(format!("error: {message}"));
                }
            }
            SessionEvent::Warning(message) => {
                self.notice(format!("warning: {message}"));
            }
            SessionEvent::Slept { duration_secs } => {
                // The device slept mid-run and woke inside the window: say
                // so where the work happened, then the continuation follows
                // as its own user turn.
                self.transcript.push(Block::new(
                    Kind::System,
                    format!(
                        "the device was asleep for {} — continuing",
                        crate::core::output::format_elapsed(duration_secs)
                    ),
                ));
            }
            SessionEvent::SleepStopped { duration_secs } => {
                // Past the resume window: a stop in the cancelled family.
                // The TurnEnd row is suppressed; this line is the record.
                if let Some(s) = &mut self.active {
                    s.sleep_stopped = true;
                }
                self.transcript.push(Block::new(
                    Kind::System,
                    format!(
                        "run stopped — the device was asleep for {}",
                        crate::core::output::format_elapsed(duration_secs)
                    ),
                ));
            }
            SessionEvent::TurnEnd { aborted } => {
                self.compacting = false;
                // The queue the review was editing died with the turn.
                self.close_queue_review();
                if aborted {
                    // Every started or pending member reaches a
                    // terminal state; no ghost Running row survives Esc.
                    for block in &mut self.transcript.blocks {
                        if block.kind == Kind::ToolGroup {
                            block.cancel_unfinished_tools();
                        } else if block.kind == Kind::Tool && !block.done {
                            block.cancelled = true;
                            block.touch();
                        }
                    }
                }
                // The turn's final burst ends with it — same moment, not
                // early. Every other burst end (reply text, tools, retry,
                // steer) ended where it happened; this catches the one that
                // ran to the turn's end.
                self.end_thinking_burst();
                self.end_assistant_burst();
                let Some(s) = self.active.take() else { return };
                // The reference grammar: a completed turn ends with a dim
                // duration-and-tokens row; a cancelled one says so instead —
                // unless the sleep stop already said it its own way.
                if aborted && !s.sleep_stopped {
                    self.transcript.push(Block::new(Kind::System, "cancelled"));
                } else if !aborted {
                    let tokens = if s.turn.input == 0 && s.turn.output == 0 {
                        String::new()
                    } else {
                        format!(
                            " (↑{} ↓{})",
                            format_tokens(s.turn.input),
                            format_tokens(s.turn.output)
                        )
                    };
                    let cost = s
                        .cost_usd
                        .filter(|cost| *cost > 0.0)
                        .map(|cost| format!(" {}", crate::core::output::format_cost(cost)))
                        .unwrap_or_default();
                    self.transcript.push(Block::new(
                        Kind::Summary,
                        format!(
                            "{}{}{}",
                            format_duration(s.started.elapsed().as_millis() as u64),
                            tokens,
                            cost
                        ),
                    ));
                }
                if let Some(message) = s.error {
                    // A failed turn ends visibly: the error persists in error
                    // color below the trailer, never a vanishing status blip.
                    self.transcript.push(Block::new(Kind::Error, message));
                }
                // Release prompts held by frontend work such as shell passthrough.
                // Prompts queued in the agent are consumed by the core itself.
                for text in std::mem::take(&mut self.held_prompts) {
                    if aborted {
                        self.notice(format!("queued message discarded by Esc: {text}"));
                    } else {
                        self.prompt(text);
                    }
                }
            }
        }
    }
}

/// The currently open block for `index`, or a freshly started one — the
/// same shape `TextDelta`'s assistant block and `ReasoningDelta`'s thinking
/// block both need, each with a different `kind`.
fn open_block(transcript: &mut Transcript, index: &mut Option<usize>, kind: Kind) -> usize {
    match *index {
        Some(idx) => idx,
        None => {
            let idx = transcript.push(Block::new(kind, ""));
            *index = Some(idx);
            idx
        }
    }
}

/// End the live thinking burst: detach it so the next reasoning opens a
/// fresh block. The streamed thought stays where it sits, expanded, in
/// `thinkingText` — there is no collapse to a dim `Thought for Ns` summary.
/// That collapse shrank the frame mid-turn, and because the paint window
/// tracks the transcript's tail, a shrink after the burst had scrolled into
/// scrollback read as the whole screen jumping. A no-op with no burst open —
/// in particular when `show_thinking` is off, which never opens one.
impl App {
    pub(super) fn end_thinking_burst(&mut self) {
        let Some(index) = self
            .active
            .as_mut()
            .and_then(|turn| turn.thinking_block.take())
        else {
            return;
        };
        if let Some(block) = self.transcript.blocks.get_mut(index) {
            block.finish_streaming();
        }
    }

    /// Seal the current reply segment so its final delta renders immediately.
    fn end_assistant_burst(&mut self) {
        let Some(index) = self.active.as_mut().and_then(|turn| turn.block.take()) else {
            return;
        };
        if let Some(block) = self.transcript.blocks.get_mut(index) {
            block.finish_streaming();
        }
    }
}
