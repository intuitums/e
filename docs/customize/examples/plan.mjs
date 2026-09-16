#!/usr/bin/env node
/** plan — a plan mode on the extension surface, built on the scaffold.
 *
 * Copy scaffold.mjs + plan.mjs into ~/.e/extensions/ (chmod +x) and
 * restart e:
 *
 *   /plan            — toggle plan mode: the model can only read and grep,
 *                      a status slot says so, and every turn gets a
 *                      planning paragraph in its system prompt
 *   ctrl+alt+p       — the same toggle as a shortcut
 *   /plan pick       — choose the mode from a picker instead
 *   /plan show       — the plan steps in a side pane; ↑/↓ move, Enter or x
 *                      checks a step off, Esc closes (ctrl+t moves focus)
 *
 * Every surface here is data e paints: the picker is e's picker, the pane
 * sits where ~/.e/layout.json says, the status slot sits on e's status row.
 * Nothing in this file touches the terminal.
 */

import { connect } from "./scaffold.mjs";

let planning = false;
let cursor = 0;
const steps = [
  { text: "Read the code paths involved", done: false },
  { text: "Write the plan as a numbered list", done: false },
  { text: "Ask before editing anything", done: false },
];

const ext = connect({
  manifest: {
    name: "plan",
    version: "1.0",
    description: "plan mode: read-only tools, a planning prompt, a step panel",
    commands: [
      {
        name: "plan",
        description: "toggle plan mode",
        arguments: "[pick|show]",
        completions: true,
      },
    ],
    shortcuts: [{ key: "ctrl+alt+p", description: "toggle plan mode" }],
    hooks: ["before_turn"],
    events: ["session_start"],
  },
  async command({ args }) {
    const word = (args || "").trim();
    if (word === "pick") {
      const picked = await ext.ui.select("Mode", [
        { label: "Plan", description: "read and grep only", value: "plan" },
        { label: "Build", description: "every tool", value: "build" },
      ]);
      if (picked.cancelled) return {};
      await setPlanning(picked.value === "plan");
      return {};
    }
    if (word === "show") {
      await drawPane();
      return {};
    }
    await setPlanning(!planning);
    return {};
  },
  async shortcut() {
    await setPlanning(!planning);
    return {};
  },
  complete({ prefix }) {
    const items = ["pick", "show"]
      .filter((word) => word.startsWith(prefix))
      .map((value) => ({ value, description: value === "pick" ? "choose the mode" : "the step pane" }));
    return { items };
  },
  beforeTurn() {
    if (!planning) return {};
    return {
      system_suffix:
        "Plan mode: you may only read and search. Produce a numbered plan; do not edit files.",
    };
  },
  event({ name }) {
    // A new or resumed session starts in build mode; e resets the toolset
    // itself, this keeps the status slot honest.
    if (name === "session_start" && planning) setPlanning(false);
  },
  // e moves the cursor and tells us; Enter (or x) checks the step off.
  paneSelect({ id }) {
    cursor = Number(id) || 0;
  },
  paneActivate({ id }) {
    toggle(Number(id) || 0);
  },
  paneKey({ key }) {
    if (key === "x" || key === "space") toggle(cursor);
  },
});

function toggle(index) {
  if (!steps[index]) return;
  steps[index].done = !steps[index].done;
  drawPane();
}

async function setPlanning(on) {
  planning = on;
  await ext.session.tools(on ? ["read", "grep"] : null);
  await ext.ui.status(on ? "plan mode" : null);
  await ext.ui.notify(on ? "plan mode: read and grep only" : "build mode: every tool");
}

function drawPane() {
  const done = steps.filter((step) => step.done).length;
  return ext.ui.pane({
    id: "plan",
    title: "Plan",
    side: "left",
    sections: [
      {
        kind: "list",
        id: "steps",
        title: `${done} of ${steps.length} done`,
        selected: String(cursor),
        items: steps.map((step, i) => ({
          id: String(i),
          label: `${step.done ? "[x]" : "[ ]"} ${step.text}`,
          token: step.done ? "success" : undefined,
        })),
      },
    ],
  });
}

ext.run();
