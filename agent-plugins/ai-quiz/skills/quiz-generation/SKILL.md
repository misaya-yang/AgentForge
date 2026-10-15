---
name: quiz-generation
description: Create an interactive quiz from knowledge-base content, conversation context, or uploaded files using the assistant's built-in generate_quiz tool.
compatibility: Requires the AI Gateway assistant runtime with the generate_quiz built-in tool.
allowed-tools: generate_quiz
---

Use the `generate_quiz` tool whenever the user asks to create a quiz, test,
exam questions, or practice questions from knowledge-base content, a document,
or conversation context.

Before calling the tool:

1. Gather source material: retrieve relevant knowledge-base content with the
   retrieval tools, or use content the user uploaded or pasted. The model
   generates the quiz itself — no second LLM call happens inside the tool.
2. Pick a short `title` and one-sentence `description`.
3. Choose `difficulty` (easy / medium / hard) and question mix:
   - `mc_single` — one correct option (correct_answer is a list with ONE letter)
   - `mc_multi` — two or three correct options (correct_answer like ["A","C"])
   - `true_false` — correct_answer ["true"] or ["false"]
   - `short_answer` — free text, graded by the platform
4. Build the `questions` array with EXACTLY these field names per question:
   `question_type`, `question_text`, `options` (array of {label, text} for
   multiple choice), `correct_answer`, `explanation`. Never use `answer` or
   `question` as field names. `option.text` must contain the answer text, never
   the letter alone.

After a successful call, reply with one brief confirmation — the UI renders the
interactive quiz and grading flow automatically.

To share a quiz publicly, the console quiz card offers a share dialog; public
links resolve at /quiz/{shareCode}.
