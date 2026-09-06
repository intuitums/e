//! Provider requests, tool batches, steering, and context maintenance for one run.
//! The supervisor owns terminal events; this worker stops only at a safe boundary.

use super::*;

/// A worker failure cannot restart just because prompts arrived during shutdown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Complete,
    Cancelled,
    Failed,
}

impl Outcome {
    fn stopped(cancel: &AtomicBool) -> Self {
        if cancel.load(Ordering::SeqCst) {
            Self::Cancelled
        } else {
            Self::Failed
        }
    }
}

/// Everything a worker needs, captured once at submission. Continuations share
/// the log and cancellation token while retaining the selected model and policy.
#[derive(Clone)]
pub(super) struct Context {
    pub(super) log: TurnLog,
    pub(super) events: mpsc::Sender<SessionEvent>,
    pub(super) history: Arc<Mutex<Vec<ChatMessage>>>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) model: Model,
    pub(super) cwd: PathBuf,
    pub(super) effort: Option<String>,
    pub(super) pending: Arc<Mutex<PendingQueue>>,
    pub(super) host: Option<Arc<crate::core::extensions::ExtensionHost>>,
    pub(super) tool_seq: Arc<AtomicU64>,
    pub(super) wake: wake::Shared,
    pub(super) system: String,
    pub(super) allowed_tools: Option<Arc<Vec<String>>>,
    pub(super) compact_requested: Arc<AtomicBool>,
    pub(super) tool_runtime: Arc<tools::ToolRuntime>,
    pub(super) tool_mode: ToolMode,
}

/// Execute requests until an answer, cancellation, or failure. Compaction
/// happens after complete tool batches and never requires frontend intervention.
pub(super) async fn run(context: Context, compact_only: bool) -> Outcome {
    let Context {
        log,
        events,
        history,
        cancel,
        model,
        cwd,
        effort,
        pending,
        host,
        tool_seq,
        wake,
        system,
        allowed_tools,
        compact_requested,
        tool_runtime,
        tool_mode,
    } = context;
    let window_secs = wake::policy::window_secs();
    let max_continuations = wake::policy::max_continuations();
    let mut sleep_continuations = 0u32;
    // One free retry for a blank success per turn: an empty stream is
    // the most transient failure there is, and ending the turn on the
    // first one traded a 1s pause for a dead turn.
    let mut empty_retried = false;
    // Steps this turn has run (one request each). The cap is a
    // runaway backstop far above real work, not a working budget.
    let mut steps = 0u32;
    let outcome = 'turn: loop {
        if cancel.load(Ordering::SeqCst) {
            break Outcome::Cancelled;
        }
        if compact_requested.swap(false, Ordering::SeqCst) {
            match compact_log(&log, &system, &cancel).await {
                Ok(true) => {}
                Ok(false) => {
                    let _ = events
                        .send(SessionEvent::Warning(
                            "recent context already fits; nothing to compact".into(),
                        ))
                        .await;
                }
                Err(error) => {
                    let _ = events.send(SessionEvent::Error(error)).await;
                    break Outcome::stopped(&cancel);
                }
            }
            if compact_only && steps == 0 {
                break Outcome::Complete;
            }
        }
        steps += 1;
        if steps > MAX_STEPS {
            let _ = events
                .send(SessionEvent::Error(format!(
                    "turn stopped after {MAX_STEPS} steps — send a message to continue"
                )))
                .await;
            break Outcome::Failed;
        }
        // Steer: fold any pending messages into this turn between
        // steps. The queue review edits these entries by key
        // concurrently; whatever the loop takes here is gone to it.
        let steered: Vec<String> = {
            let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.items.drain(..).map(|(_, text)| text).collect()
        };
        for message in steered {
            let _ = events.send(SessionEvent::Steered(message.clone())).await;
            // Harness-authored: steering echoes and continuations
            // fill the history but are not user turns.
            let mut recorded = ChatMessage::user(message);
            recorded.mark_internal();
            log.commit_async(recorded).await;
        }

        let mut messages = { history.lock().unwrap_or_else(|e| e.into_inner()).clone() };
        // Some compatible gateways omit usage entirely. Keep a local,
        // conservative fallback so the mid-turn safety guard still
        // exists there; a real Usage frame replaces it below. Sized
        // against history before the image strip below, so it stays
        // the conservative side of what's actually sent.
        let mut last_context = compact::estimate_request_tokens(&system, &messages);
        if compact::should_compact(last_context, model.context_window) {
            match compact_log(&log, &system, &cancel).await {
                Ok(true) => {
                    messages = history
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    last_context = compact::estimate_request_tokens(&system, &messages);
                }
                result => {
                    let message = result.err().unwrap_or_else(|| {
                        "context is full and cannot be reduced; history was preserved".into()
                    });
                    let _ = events.send(SessionEvent::Error(message)).await;
                    break Outcome::stopped(&cancel);
                }
            }
        }
        // A resumed session, or a mid-session model switch, can carry
        // image-bearing turns forward from an earlier, image-capable
        // model to one that isn't — this is the one place every
        // frontend's outgoing request passes through, so it's the one
        // place that needs to know. This only edits the local copy
        // just cloned from `history` above; the session's own stored
        // record keeps its images regardless of what model sends the
        // next turn.
        providers::strip_incompatible_images(&mut messages, &model);
        // The stable per-conversation id, read from the live log the
        // steering commits above just created (empty for an unsaved
        // session). Providers that opt in send it as their session
        // header — see `providers::with_attribution`.
        let session_id = log
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.id().to_string())
            .unwrap_or_default();
        let request = Request {
            model: model.clone(),
            system: system.clone(),
            messages,
            effort: effort.clone(),
            session_id,
            tools: tools::restrict_to(
                tools::filter_schemas(
                    match (&host, tool_mode, allowed_tools.is_some()) {
                        // A request allowlist names built-ins. Extension
                        // tools and overrides stay outside that contract.
                        (Some(h), ToolMode::All, false) => h.merged_tool_schemas(),
                        _ => tools::schemas(),
                    },
                    tool_mode,
                ),
                allowed_tools.as_deref().map(Vec::as_slice),
            ),
        };

        // Total provider requests made for this step, including the
        // initial request. The budget is a request budget, not a
        // retry budget, and is file-backed (`retry_max_attempts`).
        let max_attempts = retry::max_attempts();
        let mut attempt = 1u32;
        // When this attempt's stream opened. A sleep gap whose wake
        // came after this instant happened with the attempt in
        // flight, so a loss from it is attributable to the sleep.
        let mut attempt_started = Instant::now();
        let (mut rx, mut handle) = providers::stream(clone_request(&request));

        let mut text = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut reasoning_items: Vec<String> = Vec::new();
        // Set when a mid-reply loss should resume via a continuation
        // over the committed partial; when the window or the
        // continuation cap is exhausted, the stop flag is set instead.
        let mut sleep_resume: Option<Duration> = None;
        let mut sleep_stopped = false;
        // A dialect can stream thought deltas without ever
        // committing a ReasoningItem (e.g. Gemini's empty-text
        // thought chunks), so a thinking-only stream must still
        // count as produced by this flag alone: otherwise a
        // retryable error after a long thinking phase would retry
        // (replaying the thoughts on screen) and a thinking-only
        // stream ending without text would be called an empty
        // response.
        let mut reasoning_streamed = false;
        // Latest usage frame this step; emitted once after the stream
        // ends. Dialects may report usage cumulatively mid-stream
        // (Gemini sends usageMetadata per chunk), so forwarding every
        // frame would let a consumer that sums per-step usage count
        // the same tokens more than once.
        let mut step_usage: Option<(u64, u64, u64)> = None;
        // Cumulative argument bytes this attempt, for the liveness
        // row. Deliberately not part of the retry-safety check: a
        // partial call never left the dialect, so replaying the
        // request commits nothing twice.
        let mut assembly_bytes = 0u64;
        let mut errored = false;
        // True once this attempt has streamed anything at all — the
        // signal both for "recovered" (first content after a retry)
        // and for whether a fresh failure is still safe to retry.
        let mut recovered_notified = false;
        // Cancel must win even when the provider yields nothing: a
        // prior `while let Some(event) = rx.recv()` only checked Esc
        // after the next byte arrived, so a stalled SSE left the
        // spinner running and Esc inert until the socket moved.
        // Esc mid-stream falls through to the partial-commit path
        // below instead of breaking the turn here: text the user
        // watched stream must reach history, or the next turn's model
        // has no memory of words the user is replying to.
        let mut stream_cancelled = false;
        'stream: loop {
            let event = tokio::select! {
                event = rx.recv() => event,
                _ = wait_cancelled(&cancel) => {
                    handle.abort();
                    stream_cancelled = true;
                    break 'stream;
                }
            };
            let Some(event) = event else {
                break 'stream;
            };
            // The new stream (after one or more retries) just
            // produced its first non-error event — the retry
            // worked. An immediate second failure is not a recovery,
            // so this excludes Error and lets that arm decide
            // whether to retry again instead. `attempt` is 1-based:
            // 1 is the first try, so only attempt 2+ ever recovered.
            if attempt > 1 && !recovered_notified && !matches!(event, ProviderEvent::Error(_)) {
                recovered_notified = true;
                let _ = events
                    .send(SessionEvent::Recovered {
                        attempt,
                        limit: max_attempts,
                    })
                    .await;
            }
            match event {
                ProviderEvent::TextDelta(d) => {
                    text.push_str(&d);
                    let _ = events.send(SessionEvent::TextDelta(d)).await;
                }
                ProviderEvent::ReasoningDelta(d) => {
                    reasoning_streamed = true;
                    let _ = events.send(SessionEvent::ReasoningDelta(d)).await;
                }
                ProviderEvent::ToolArgumentsDelta { delta, .. } => {
                    assembly_bytes += delta.len() as u64;
                    let _ = events
                        .send(SessionEvent::ToolCallAssembly {
                            bytes: assembly_bytes,
                        })
                        .await;
                }
                ProviderEvent::ToolCallStart { .. } | ProviderEvent::ToolCallEnd { .. } => {}
                ProviderEvent::ToolCall(call) => calls.push(call),
                ProviderEvent::ReasoningItem(item) => reasoning_items.push(item),
                ProviderEvent::Usage {
                    input,
                    output,
                    cache_read,
                } => {
                    step_usage = Some((input, output, cache_read));
                }
                ProviderEvent::Error(err) => {
                    // A suspension that outlived the resume window
                    // stops the run whatever the provider call died
                    // of: partial work is committed below, and the
                    // stop is reported as a stop, not an error.
                    if let Some(gap) = wake::gap_since(&wake, attempt_started)
                        .filter(|gap| gap.duration.as_secs() >= window_secs)
                    {
                        let _ = events
                            .send(SessionEvent::SleepStopped {
                                duration_secs: gap.duration.as_secs(),
                            })
                            .await;
                        let _ = events
                            .send(SessionEvent::Warning(format!(
                        "run stopped — the device was asleep for {} (window {window_secs}s)",
                        gap.label()
                    )))
                            .await;
                        handle.abort();
                        errored = true;
                        sleep_stopped = true;
                        break 'stream;
                    }
                    let nothing_produced = text.is_empty()
                        && calls.is_empty()
                        && reasoning_items.is_empty()
                        && !reasoning_streamed;
                    // The attempt was in flight across a sleep that
                    // fits the window: the run keeps going. Nothing
                    // streamed means an immediate replay — not
                    // charged to the attempt budget, since the loss
                    // was the machine's, not the provider's.
                    let slept_through = wake::gap_since(&wake, attempt_started).is_some();
                    if nothing_produced && slept_through {
                        let duration = wake::gap_since(&wake, attempt_started)
                            .map(|gap| gap.duration.as_secs())
                            .unwrap_or(0);
                        let _ = events
                            .send(SessionEvent::Slept {
                                duration_secs: duration,
                            })
                            .await;
                        // No artificial backoff — the machine just
                        // woke; let the connect decide.
                        attempt_started = Instant::now();
                        let (nrx, nhandle) = providers::stream(clone_request(&request));
                        rx = nrx;
                        handle = nhandle;
                        assembly_bytes = 0;
                        continue 'stream;
                    }
                    // Safe to retry only when the cause itself is
                    // retryable (never Auth or Rejected) AND nothing
                    // has streamed yet this attempt: a delivered
                    // request that already produced output or ran
                    // tools cannot be replayed without risking a
                    // duplicate.
                    if err.cause.is_retryable() && nothing_produced && attempt < max_attempts {
                        let retry_number = attempt;
                        attempt += 1;
                        let delay = retry::delay_for(retry_number, err.retry_after);
                        let _ = events
                            .send(SessionEvent::Retry {
                                attempt,
                                limit: max_attempts,
                                delay_secs: delay.as_secs(),
                                cause: err.cause,
                                reason: err.short.clone(),
                            })
                            .await;
                        if !sleep_cancellable(delay, &cancel).await {
                            handle.abort();
                            break 'turn Outcome::Cancelled;
                        }
                        attempt_started = Instant::now();
                        let (nrx, nhandle) = providers::stream(clone_request(&request));
                        rx = nrx;
                        handle = nhandle;
                        // A fresh attempt streams its arguments from
                        // scratch; the liveness counter follows.
                        assembly_bytes = 0;
                        continue 'stream;
                    }
                    // A mid-reply loss under the window resumes
                    // differently: the partial reply is committed
                    // below, and a continuation request lets the
                    // model finish its own sentence — bounded by the
                    // continuation cap so lid-flapping cannot chain
                    // turns unattended.
                    if !nothing_produced {
                        if let Some(gap) = wake::gap_since(&wake, attempt_started) {
                            if sleep_continuations < max_continuations {
                                sleep_resume = Some(gap.duration);
                                errored = true;
                                break 'stream;
                            }
                            let _ = events
                                .send(SessionEvent::SleepStopped {
                                    duration_secs: gap.duration.as_secs(),
                                })
                                .await;
                            let _ = events
                        .send(SessionEvent::Warning(format!(
                            "run stopped — the device was asleep for {} (continuation cap {max_continuations} reached)",
                            gap.label()
                        )))
                        .await;
                            handle.abort();
                            // Enter the cancel-family stop path (like
                            // the past-window branch above): commit the
                            // partial once, synthesize results for any
                            // unrun calls, and end the turn. Without
                            // `errored` the stop block is skipped and
                            // the half-streamed reply is committed as a
                            // normal turn — running its tool calls after
                            // the run was already reported stopped.
                            errored = true;
                            sleep_stopped = true;
                            break 'stream;
                        }
                    }
                    // Distinguish genuine exhaustion (the cause was
                    // retryable and nothing had streamed, but the
                    // budget ran out) from a failure that simply
                    // can't be retried at all — partial content
                    // already produced this attempt, or a rejected
                    // cause. Only the former earns "gave up after
                    // N/M"; the latter would misreport why the
                    // attempt stopped.
                    let message =
                        if max_attempts > 0 && err.cause.is_retryable() && nothing_produced {
                            format!(
                                "{} — gave up after {attempt}/{} attempts: {}",
                                err.cause.label(),
                                max_attempts,
                                err.message
                            )
                        } else if err.cause == FailureCause::QuotaExhausted {
                            // Lead with the why: the raw body behind it
                            // is provider JSON the row will never show.
                            format!("{} — {}", err.cause.label(), err.message)
                        } else {
                            err.message
                        };
                    let _ = events.send(SessionEvent::Error(message)).await;
                    errored = true;
                }
                ProviderEvent::Done(end) => {
                    // The stream completed, but not necessarily with
                    // the full answer — a truncated, refused, or
                    // filtered reply arrives as an HTTP success and
                    // must not pass silently.
                    let finish_warning = match &end.finish {
                        FinishReason::Normal | FinishReason::ToolCalls => None,
                        FinishReason::Length => {
                            Some("reply truncated: the provider hit its output limit".into())
                        }
                        FinishReason::Refusal => Some("the model refused to answer".to_string()),
                        FinishReason::ContentFilter => {
                            Some("output blocked by the provider's content filter".to_string())
                        }
                        FinishReason::Other(reason) => {
                            Some(format!("turn ended abnormally: {reason}"))
                        }
                    };
                    if let Some(warning) = finish_warning {
                        let _ = events.send(SessionEvent::Warning(warning)).await;
                    }
                    if end.malformed > 0 {
                        let _ = events
                            .send(SessionEvent::Warning(format!(
                                "{} malformed stream event{} skipped",
                                end.malformed,
                                if end.malformed == 1 { "" } else { "s" }
                            )))
                            .await;
                    }
                }
            }
        }
        // One Usage per step, the stream's final frame: `input` is
        // this request's full context, `output` what this step alone
        // generated. Emitted even when the stream then errored — the
        // tokens were still consumed.
        if let Some((input, output, cache_read)) = step_usage {
            last_context = input + output;
            let _ = events
                .send(SessionEvent::Usage {
                    input,
                    output,
                    cache_read,
                })
                .await;
        } else {
            let provisional = ChatMessage::assistant(text.clone(), calls.clone());
            last_context =
                last_context.saturating_add(compact::estimate_message_tokens(&provisional));
        }
        // A cancelled or failed stream still commits what it already
        // produced: the user watched that text arrive, and a history
        // missing it would have the model contradict its own visible
        // words next turn. Calls that never ran get a synthetic
        // result — a dangling tool_use without its tool_result fails
        // the next request on every dialect.
        if stream_cancelled || errored {
            if !text.is_empty() || !calls.is_empty() {
                for item in reasoning_items.drain(..) {
                    log.commit_async(ChatMessage::reasoning(item)).await;
                }
                let (note, outcome, summary) = if stream_cancelled {
                    (
                        "not executed — the turn was cancelled before this call ran",
                        tools::ToolOutcome::Cancelled,
                        "cancelled",
                    )
                } else {
                    (
                        "not executed — the provider stream failed before this call ran",
                        tools::ToolOutcome::Failed,
                        "error",
                    )
                };
                let unrun = calls.clone();
                let mut final_message = ChatMessage::assistant(std::mem::take(&mut text), calls);
                if let Some((input, output, cache_read)) = step_usage {
                    final_message = final_message.with_usage(providers::MessageUsage {
                        input,
                        output,
                        cache_read,
                    });
                }
                log.commit_async(final_message).await;
                for call in unrun {
                    log.commit_async(ChatMessage::tool_result_with_meta(
                        call.id, note, outcome, summary,
                    ))
                    .await;
                }
            }
            if stream_cancelled {
                break Outcome::Cancelled;
            }
            // A sleep past the window (or the continuation cap) is a
            // stop in the cancel family: the stop line already went
            // out, so end aborted without a second row.
            if sleep_stopped {
                break Outcome::Cancelled;
            }
            // A sleep under the window resumes: the continuation
            // message is committed and shown, and the next step asks
            // the model to finish its own sentence.
            if let Some(duration) = sleep_resume {
                sleep_continuations += 1;
                let _ = events
                    .send(SessionEvent::Slept {
                        duration_secs: duration.as_secs(),
                    })
                    .await;
                // Harness-authored: the wake continuation fills
                // the history but is not a user turn.
                let mut recorded = ChatMessage::user(SLEEP_CONTINUATION.to_string());
                recorded.mark_internal();
                log.commit_async(recorded).await;
                let _ = events
                    .send(SessionEvent::Steered(SLEEP_CONTINUATION.to_string()))
                    .await;
                continue 'turn;
            }
            break Outcome::Failed;
        }

        // A stream that ends with no text, no calls, no reasoning,
        // and no error is a blank success — committing it would strand
        // the turn in silence. It is also the most transient failure
        // a provider produces, so it gets one quiet re-request per
        // turn when the configured attempt budget permits. Abnormal
        // finishes and skipped frames
        // were already surfaced as warnings above; past the retry we
        // guarantee the user at least sees that something went wrong.
        if text.is_empty()
            && calls.is_empty()
            && reasoning_items.is_empty()
            && !reasoning_streamed
            && pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .items
                .is_empty()
        {
            if !empty_retried && max_attempts > 1 {
                empty_retried = true;
                let _ = events
                    .send(SessionEvent::Retry {
                        attempt: 2,
                        limit: max_attempts,
                        delay_secs: 1,
                        cause: FailureCause::ProviderUnavailable,
                        reason: "empty response".into(),
                    })
                    .await;
                if !sleep_cancellable(Duration::from_secs(1), &cancel).await {
                    break 'turn Outcome::Cancelled;
                }
                continue 'turn;
            }
            let _ = events
                .send(SessionEvent::Error(
                    "the model returned an empty response".into(),
                ))
                .await;
            break Outcome::Failed;
        }

        // Reasoning items commit first — the dialect that produced
        // them must replay them ahead of the assistant turn.
        for item in reasoning_items.drain(..) {
            log.commit_async(ChatMessage::reasoning(item)).await;
        }
        // Commit the assistant turn (text + any calls), with the
        // step's real usage attached when the stream reported it —
        // the session file then carries the token accounting.
        let mut final_message = ChatMessage::assistant(text, calls.clone());
        if let Some((input, output, cache_read)) = step_usage {
            final_message = final_message.with_usage(providers::MessageUsage {
                input,
                output,
                cache_read,
            });
        }
        log.commit_async(final_message).await;

        if calls.is_empty() {
            // A plain reply would end the turn — but a message that
            // landed mid-reply must still be delivered, so continue
            // the turn to pick it up rather than stranding it.
            if !pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .items
                .is_empty()
            {
                continue 'turn;
            }
            break 'turn Outcome::Complete;
        }

        // Resolve the complete batch before concurrent execution so the
        // transcript has one stable group from the first call.
        let mut batch = Vec::with_capacity(calls.len());
        for call in &calls {
            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
            let presentation = tools::present(&call.name, &args);
            let id = tool_seq.fetch_add(1, Ordering::SeqCst) + 1;
            batch.push((
                id,
                call.clone(),
                ToolCallPresentation {
                    id,
                    category: presentation.category,
                    running: presentation.running,
                    completed: presentation.completed,
                    target: presentation.target,
                },
            ));
        }
        let _ = events
            .send(SessionEvent::ToolBatchStart {
                calls: batch.iter().map(|(_, _, shown)| shown.clone()).collect(),
            })
            .await;

        // Run the batch concurrently: every
        // child streams its own lifecycle on the shared channel as it
        // progresses. Results commit in assistant source order.
        let mut handles = Vec::with_capacity(batch.len());
        for (id, call, _) in batch {
            let host = host.clone();
            let cancel = cancel.clone();
            let events = events.clone();
            let cwd = cwd.clone();
            let allowed_tools = allowed_tools.clone();
            let tool_runtime = tool_runtime.clone();
            handles.push((
                call.clone(),
                tokio::spawn(async move {
                    let _ = events.send(SessionEvent::ToolStart { id }).await;
                    let output = run_tool(
                        ToolRunContext {
                            tools: tool_runtime,
                            host,
                            tool_mode,
                            allowed_tools,
                            cwd,
                            cancel,
                            id,
                            events: events.clone(),
                        },
                        &call.name,
                        &call.arguments,
                    )
                    .await;
                    let _ = events
                        .send(SessionEvent::ToolEnd {
                            id,
                            outcome: output.outcome,
                            summary: output.summary.clone(),
                            // The viewer gets the rich detail (full
                            // diffs); history keeps the lean content.
                            content: output.display_text().to_string(),
                        })
                        .await;
                    output
                }),
            ));
        }

        for (call, handle) in handles {
            // A blocked filesystem operation cannot be interrupted in
            // place; on Esc, stop waiting, record the call as
            // cancelled, and detach the task — the turn must end
            // promptly even over a stalled FIFO or NFS mount.
            let output = tokio::select! {
                biased;
                joined = handle => match joined {
                    Ok(output) => output,
                    Err(_) => tools::ToolOutput {
                        content: "tool panicked".into(),
                        outcome: tools::ToolOutcome::Failed,
                        summary: "error".into(),
                        display: None,
                    },
                },
                _ = wait_cancelled(&cancel) => tools::ToolOutput {
                    // Honest record: the blocked operation is only
                    // detached, so it may still complete after this.
                    content: "tool cancelled — the underlying operation \
                              may still complete in the background"
                        .into(),
                    outcome: tools::ToolOutcome::Cancelled,
                    summary: "cancelled".into(),
                    display: None,
                },
            };
            last_context =
                last_context.saturating_add((output.content.chars().count() as u64).div_ceil(4));
            log.commit_async(ChatMessage::tool_result_with_meta(
                call.id,
                output.content,
                output.outcome,
                output.summary,
            ))
            .await;
        }
        if cancel.load(Ordering::SeqCst) {
            break 'turn Outcome::Cancelled;
        }
        // Compact after committing the complete tool batch. The next
        // request continues this run through the same event stream.
        if last_context > 0 && compact::should_compact(last_context, model.context_window) {
            let _ = events
                .send(SessionEvent::Warning(
                    "context nearly full — compacting before continuing".into(),
                ))
                .await;
            match compact_log(&log, &system, &cancel).await {
                Ok(true) => {}
                Ok(false) => {
                    let _ = events
                        .send(SessionEvent::Error(
                            "context is full and cannot be reduced; history was preserved".into(),
                        ))
                        .await;
                    break Outcome::Failed;
                }
                Err(error) => {
                    let _ = events.send(SessionEvent::Error(error)).await;
                    break Outcome::stopped(&cancel);
                }
            }
        }
    };
    outcome
}
