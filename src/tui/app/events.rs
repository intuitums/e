//! Session-event handling: the one ordered stream from the agent —
//! text, thinking, tool lifecycle, usage, retries — projected onto App
//! state and the transcript.

use super::*;

impl App {
    /// The single session stream, in order. Turn bookkeeping hangs off it.
    pub(super) fn on_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::TurnStart => {
                self.active = Some(ActiveTurn {
                    block: None,
                    text: String::new(),
                    thinking_block: None,
                    thinking: String::new(),
                    thinking_started: None,
                    turn: Turn::new(),
                    started: Instant::now(),
                    error: None,
                    sleep_stopped: false,
                    tool_blocks: std::collections::HashMap::new(),
                    pending_tools: 0,
                    cost_usd: self.agent.model.pricing.as_ref().map(|_| 0.0),
                });
                // Seed the token counters from request size so the activity
                // row shows ↑ from the first second, like the reference.
                if let Some(s) = &mut self.active {
                    let estimate = crate::core::agent::compact::estimate_request_tokens(
                        &system_prompt(),
                        &self.agent.history_snapshot(),
                    );
                    s.turn.seed_input(estimate);
                }
            }
            SessionEvent::Steered(text) => {
                // A mid-turn message: show it as a user turn where it landed.
                self.transcript.push(Block::new(Kind::User, text));
                // The next assistant text opens a fresh block; the burst
                // that was live collapses where it sat.
                self.end_thinking_burst();
                if let Some(s) = &mut self.active {
                    s.block = None;
                    s.text.clear();
                }
            }
            SessionEvent::TextDelta(delta) => {
                // Reply text starting ends the live thinking burst — it
                // collapses to its one-line summary above the reply.
                self.end_thinking_burst();
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::AssistantText;
                    s.turn.note_text(&delta);
                    // Model output is untrusted: strip control sequences
                    // before it can reach the paint stream. (The raw text
                    // still goes to the model's own history in core.)
                    s.text
                        .push_str(&crate::core::tools::sanitize_display(&delta));
                    let idx = open_block(&mut self.transcript, &mut s.block, Kind::Assistant);
                    let text = s.text.clone();
                    if let Some(b) = self.transcript.blocks.get_mut(idx) {
                        b.text = text;
                        b.touch();
                    }
                }
            }
            // Reasoning streams live in thinkingText while the burst runs;
            // when the burst ends — reply text, tools, retry, steer, turn
            // commit — it collapses to a single dim row. Raw provider text
            // is stripped before it can reach the paint stream, like
            // assistant text.
            SessionEvent::ReasoningDelta(delta) => {
                if let Some(s) = &mut self.active {
                    s.turn.note_text(&delta);
                    if self.show_thinking {
                        if s.thinking_block.is_none() {
                            s.thinking_started = Some(Instant::now());
                        }
                        s.thinking
                            .push_str(&crate::core::tools::sanitize_display(&delta));
                        let idx =
                            open_block(&mut self.transcript, &mut s.thinking_block, Kind::Thinking);
                        let text = s.thinking.clone();
                        if let Some(b) = self.transcript.blocks.get_mut(idx) {
                            b.text = text;
                            b.touch();
                        }
                    }
                }
            }
            SessionEvent::ToolCallAssembly { bytes } => {
                // The model is streaming tool-call arguments. No phase of
                // its own — the footer stays on Thinking (the model is
                // still generating), the bytes count toward the token
                // estimate, and the tool row appears in the tree when the
                // call actually starts.
                if let Some(s) = &mut self.active {
                    s.turn.note_assembly(bytes);
                }
            }
            SessionEvent::ToolBatchStart { calls } => {
                // The pre-batch reasoning collapses where it sits; the tree
                // then continues if the agent has not spoken since the last
                // batch — one tree per working stretch, not one per batch.
                self.end_thinking_burst();
                if let Some(s) = &mut self.active {
                    s.block = None;
                    s.text.clear();
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
                    for call in calls {
                        s.tool_blocks.insert(call.id, idx);
                    }
                }
            }
            SessionEvent::ToolStart { id } => {
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Tool;
                    if let Some(&idx) = s.tool_blocks.get(&id) {
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
                        TurnPhase::Thinking
                    } else {
                        TurnPhase::Tool
                    };
                }
                if !content.trim().is_empty() {
                    self.remember_output(
                        title.unwrap_or_else(|| "tool output".into()),
                        crate::core::tools::sanitize_display(&content),
                    );
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
                self.context_tokens = input + output;
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
                // Replaces the Thinking row in place — a live status, not a
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
                    // The abandoned attempt's argument bytes die with it —
                    // the fresh attempt streams from scratch.
                    s.turn.note_assembly(0);
                }
                // The abandoned attempt's thinking collapses with its
                // duration; the retry streams a fresh burst.
                self.end_thinking_burst();
            }
            SessionEvent::Recovered { attempt, limit } => {
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Thinking;
                    s.turn.retry = None;
                    s.turn.recovered = Some(RecoveredStatus {
                        attempt,
                        limit,
                        since: Instant::now(),
                    });
                }
            }
            SessionEvent::Error(message) => {
                if let Some(s) = &mut self.active {
                    s.error = Some(message);
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
                let stranded = self.agent.on_turn_end();
                if aborted {
                    // Every started or serially pending member reaches a
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
                // The turn's final burst collapses with it — same moment,
                // not early. Every other burst end (reply text, tools,
                // retry, steer) collapsed where it happened; this catches
                // the one that ran to the turn's end.
                self.end_thinking_burst();
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
                        let estimate = if s.turn.input_estimated { "~" } else { "" };
                        format!(
                            " (↑{estimate}{} ↓{})",
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
                    self.transcript
                        .push(Block::new(Kind::Error, format!("error: {message}")));
                }
                // Compaction runs between turns, never during one: a deferred
                // /compact fires here, and so does the auto threshold check
                // against real usage (window minus reserve).
                let over = crate::core::agent::compact::should_compact(
                    self.context_tokens,
                    self.agent.model.context_window,
                );
                if !aborted && (self.compact_requested || over) {
                    let auto = !self.compact_requested;
                    self.compact_requested = false;
                    self.start_compaction(auto);
                }
                // A prompt submitted in the gap between the worker's final
                // pending check and this handler had no worker left to drain
                // it. Resubmit in order; after Esc, dropping it visibly
                // beats silently starting a turn the user just stopped.
                for text in stranded {
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

/// End the live thinking burst: collapse its block (or blocks — the walk
/// covers everything still live, not just the open index) to a single dim
/// `Thought for Ns` row, using when the burst started. A no-op with no
/// burst open — in particular when `show_thinking` is off, which never
/// opens one.
impl App {
    pub(super) fn end_thinking_burst(&mut self) {
        let Some(s) = &mut self.active else { return };
        if s.thinking_block.is_none() {
            return;
        }
        let secs = s
            .thinking_started
            .map(|started| started.elapsed().as_secs())
            .unwrap_or(0);
        for block in &mut self.transcript.blocks {
            if block.kind == Kind::Thinking && !block.done {
                block.text = format!("Thought for {}", format_elapsed(secs));
                block.done = true;
                block.touch();
            }
        }
        s.thinking_block = None;
        s.thinking.clear();
        s.thinking_started = None;
    }
}
