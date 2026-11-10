//! Answering Claude Code's question form (the `AskUserQuestion` tool) with
//! keys, the way a person at the terminal does. Nothing else is touched:
//! not the agent's files, not its process.
//!
//! # The form (checked against Claude Code 2.1.316)
//!
//! Each question is one tab (`← ☐ Pricing ☐ Rollout ✔ Submit →`). The keys
//! the form reacts to, from the bundled UI code of Claude Code 2.1.316:
//!
//! * Single choice: the list is the options, then the "Type something."
//!   text row, then (outside the list) "Chat about this". Focus starts on
//!   option 1. Down/Up move, Enter picks the focused option, records the
//!   answer and moves on to the next tab. On the text row typed characters
//!   go into the field and Enter submits it; Enter on an *empty* field
//!   cancels the whole form, so the text is typed before Enter. Digits pick
//!   an option at once (not used here: arrows are what the footer offers).
//! * Several choices (`multiSelect`): every option has a check box, then the
//!   "Type something" row, then a "Next" (last question: "Submit") button.
//!   Space (or Enter) toggles the focused option; typing on the text row
//!   fills it and ticks it. Down from the text row focuses the button and
//!   Enter on the button moves on to the next tab.
//! * After the last tab the "Review your answers" screen lists the answers
//!   with "Submit answers" focused (then "Cancel"); Enter submits.
//! * One single-choice question alone has no Submit tab: Enter on an
//!   option (or the text) submits the form at once.
//! * Single-choice options with a `preview` are drawn as a preview list
//!   without the text row ([`vca_core::Question::free_text`] is false).
//!
//! The form reads each key against its last drawn state, so keys are sent
//! one at a time with short pauses ([`KeyStep::Pause`]).
//!
//! Checked again on Claude Code 2.1.318 (2026-11-08) through a Windows
//! console and a pseudo console: the keys of a typed answer (three Downs to
//! "4. Type something.", Korean text, Enter) submit a freshly drawn
//! three-option form. The keys only fit that fresh form: another dialog
//! drawn over it (a notice such as "Teach auto mode about your
//! environment?") takes them instead, so callers check the screen first
//! ([`form_on_screen`]) and before submitting typed text
//! ([`text_on_screen`]) where they can read it.

use vca_core::{ControlKey, Question, QuestionAnswer};

pub use crate::terminal::KeyStep;

/// Pause after every key, so the form draws the new focus first.
pub const KEY_PAUSE_MS: u64 = 120;
/// Pause after typed text before Enter: a burst of text and Enter is taken
/// as a paste (the Enter would become a newline).
pub const TEXT_PAUSE_MS: u64 = 300;
/// Pause after a question is answered, while the next tab draws.
pub const NEXT_PAUSE_MS: u64 = 450;
/// Longest free-text answer.
pub const MAX_TEXT_CHARS: usize = 2_000;

/// A key and the pause after it.
fn key(key: ControlKey) -> [KeyStep; 2] {
    [KeyStep::Key { key }, KeyStep::Pause { ms: KEY_PAUSE_MS }]
}

/// Why answers cannot be typed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AnswerError(pub String);

fn invalid(message: impl Into<String>) -> AnswerError {
    AnswerError(message.into())
}

/// Checks a free-text answer: trimmed, not empty, one line, not too long.
fn clean_text(
    text: Option<&str>,
    question: &Question,
    n: usize,
) -> Result<Option<String>, AnswerError> {
    let Some(text) = text.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if !question.free_text {
        return Err(invalid(format!("question {n} takes no typed answer")));
    }
    if text.chars().any(char::is_control) {
        return Err(invalid("a typed answer must be one line"));
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(invalid(format!(
            "a typed answer has at most {MAX_TEXT_CHARS} characters"
        )));
    }
    Ok(Some(text.to_owned()))
}

fn downs(out: &mut Vec<KeyStep>, count: usize) {
    for _ in 0..count {
        out.extend(key(ControlKey::Down));
    }
}

/// The keys that answer `questions` with `answers` (one per question, in
/// order) in a freshly drawn form: focus on option 1 of the first tab.
pub fn answer_keys(
    questions: &[Question],
    answers: &[QuestionAnswer],
) -> Result<Vec<KeyStep>, AnswerError> {
    if questions.is_empty() {
        return Err(invalid("there is no question to answer"));
    }
    if answers.len() != questions.len() {
        return Err(invalid("answer every question"));
    }
    let mut out = Vec::new();
    for (i, (q, a)) in questions.iter().zip(answers).enumerate() {
        let n = i + 1;
        let count = q.options.len();
        let text = clean_text(a.text.as_deref(), q, n)?;
        let mut picked: Vec<usize> = Vec::with_capacity(a.options.len());
        for &o in &a.options {
            let o = usize::try_from(o).unwrap_or(usize::MAX);
            if o >= count {
                return Err(invalid(format!(
                    "question {n} has no option {}",
                    o.saturating_add(1)
                )));
            }
            if !picked.contains(&o) {
                picked.push(o);
            }
        }
        picked.sort_unstable();
        if q.multi_select {
            if picked.is_empty() && text.is_none() {
                return Err(invalid(format!(
                    "pick at least one answer for question {n}"
                )));
            }
            let mut pos = 0;
            for o in picked {
                downs(&mut out, o - pos);
                out.push(KeyStep::Text { text: " ".into() });
                out.push(KeyStep::Pause { ms: KEY_PAUSE_MS });
                pos = o;
            }
            if let Some(text) = text {
                downs(&mut out, count - pos);
                out.push(KeyStep::Text { text });
                out.push(KeyStep::Pause { ms: TEXT_PAUSE_MS });
                pos = count;
            }
            // Past the text row to the Next / Submit button.
            downs(&mut out, count + 1 - pos);
            out.push(KeyStep::Key {
                key: ControlKey::Enter,
            });
        } else {
            match (picked.as_slice(), text) {
                ([o], None) => {
                    downs(&mut out, *o);
                    out.push(KeyStep::Key {
                        key: ControlKey::Enter,
                    });
                }
                ([], Some(text)) => {
                    downs(&mut out, count);
                    out.push(KeyStep::Text { text });
                    out.push(KeyStep::Pause { ms: TEXT_PAUSE_MS });
                    out.push(KeyStep::Key {
                        key: ControlKey::Enter,
                    });
                }
                _ => {
                    return Err(invalid(format!(
                        "question {n} takes exactly one answer: one option or typed text"
                    )));
                }
            }
        }
        out.push(KeyStep::Pause { ms: NEXT_PAUSE_MS });
    }
    if !submits_at_once(questions) {
        // The review screen, "Submit answers" focused.
        out.push(KeyStep::Key {
            key: ControlKey::Enter,
        });
    }
    Ok(out)
}

/// One single-choice question alone: the form submits on its answer.
fn submits_at_once(questions: &[Question]) -> bool {
    matches!(questions, [q] if !q.multi_select)
}

/// One line describing the form, for alerts and spoken announcements:
/// the first question (with its tab label) and how many follow.
pub fn question_prompt(questions: &[Question]) -> String {
    let Some(first) = questions.first() else {
        return String::new();
    };
    let header = first.header.trim();
    let mut out = if header.is_empty() {
        first.question.clone()
    } else {
        format!("[{header}] {}", first.question)
    };
    if questions.len() > 1 {
        out.push_str(&format!(" (+{})", questions.len() - 1));
    }
    out
}

/// The form's footer while a question tab has focus.
const FOOTER: &str = "Enter to select";
/// How much of a label or typed text is compared with the screen: long
/// lines are cut at the screen edge, the start is enough.
const MATCH_CHARS: usize = 12;

fn squeeze(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn prefix(s: &str) -> String {
    squeeze(s).chars().take(MATCH_CHARS).collect()
}

/// A numbered row of a list (`❯ 3. label` or `  3. label`), squeezed and
/// without the pointer and the number: `Some((pointed, rest))`.
fn numbered_row(line: &str) -> Option<(bool, String)> {
    let line = squeeze(line);
    let (pointed, rest) = match line.strip_prefix('❯') {
        Some(rest) => (true, rest),
        None => (false, line.as_str()),
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = rest[digits..].strip_prefix('.')?;
    Some((pointed, rest.to_owned()))
}

/// Whether a terminal screen shows the form freshly drawn, as
/// [`answer_keys`] expects: the pointer on option 1 of the first question,
/// with the form's footer below it as the last thing drawn (no other
/// dialog, such as a notice or another prompt, has the keys).
pub fn form_on_screen(screen: &str, questions: &[Question]) -> bool {
    let Some(first) = questions.first().and_then(|q| q.options.first()) else {
        return false;
    };
    let label = prefix(&first.label);
    let lines: Vec<&str> = screen.lines().collect();
    let Some(footer) = lines.iter().rposition(|l| l.contains(FOOTER)) else {
        return false;
    };
    // Nothing that takes keys below the footer.
    if lines[footer + 1..]
        .iter()
        .any(|l| l.contains('❯') || l.contains("Enter to "))
    {
        return false;
    }
    // The form's pointer (the last numbered row with one above the
    // footer; the transcript above may echo a prompt with a pointer too)
    // is on option 1 of the first question.
    lines[..footer]
        .iter()
        .rev()
        .find(|l| numbered_row(l).is_some_and(|(pointed, _)| pointed))
        .is_some_and(|l| {
            squeeze(l).starts_with("❯1.")
                && numbered_row(l).is_some_and(|(_, rest)| rest.contains(&label))
        })
}

/// Whether the screen shows typed `text` in the focused row of the form
/// (the "Type something" row), right before Enter submits it.
pub fn text_on_screen(screen: &str, text: &str) -> bool {
    let text = prefix(text);
    !text.is_empty()
        && screen
            .lines()
            .filter_map(numbered_row)
            .any(|(pointed, rest)| pointed && rest.contains(&text))
}

/// Whether the screen still shows the form: its review screen, or its
/// footer and a numbered row with one of its options. Once the agent took
/// the answers the form is gone from the screen.
pub fn form_still_open(screen: &str, questions: &[Question]) -> bool {
    if screen.contains("Review your answers") {
        return true;
    }
    if !screen.contains(FOOTER) {
        return false;
    }
    let labels: Vec<String> = questions
        .iter()
        .flat_map(|q| q.options.iter().map(|o| prefix(&o.label)))
        .filter(|l| !l.is_empty())
        .collect();
    screen
        .lines()
        .filter_map(numbered_row)
        .any(|(_, rest)| labels.iter().any(|l| rest.contains(l.as_str())))
}

#[cfg(test)]
mod tests {
    use vca_core::QuestionOption;

    use super::*;

    fn question(options: &[&str], multi: bool) -> Question {
        Question {
            header: "H".into(),
            question: "Q?".into(),
            options: options
                .iter()
                .map(|l| QuestionOption {
                    label: (*l).into(),
                    description: String::new(),
                })
                .collect(),
            multi_select: multi,
            free_text: true,
        }
    }

    fn pick(options: &[u32]) -> QuestionAnswer {
        QuestionAnswer {
            options: options.to_vec(),
            text: None,
        }
    }

    fn typed(text: &str) -> QuestionAnswer {
        QuestionAnswer {
            options: Vec::new(),
            text: Some(text.into()),
        }
    }

    /// The keys without pauses, as short names.
    fn names(steps: &[KeyStep]) -> Vec<String> {
        steps
            .iter()
            .filter_map(|s| match s {
                KeyStep::Key { key } => Some(format!("{key:?}")),
                KeyStep::Text { text } if text == " " => Some("Space".into()),
                KeyStep::Text { text } => Some(format!("'{text}'")),
                KeyStep::Pause { .. } => None,
            })
            .collect()
    }

    #[test]
    fn one_single_choice_question_submits_on_enter() {
        let qs = [question(&["a", "b", "c"], false)];
        let keys = answer_keys(&qs, &[pick(&[2])]).unwrap();
        assert_eq!(names(&keys), ["Down", "Down", "Enter"]);
        // Every key is followed by a pause.
        assert!(matches!(keys[1], KeyStep::Pause { ms: KEY_PAUSE_MS }));
        let first = answer_keys(&qs, &[pick(&[0])]).unwrap();
        assert_eq!(names(&first), ["Enter"]);
    }

    #[test]
    fn several_questions_end_on_the_review_screen() {
        // The form from the report: four tabs, then Submit.
        let qs = [
            question(&["ship price", "per field", "after meeting"], false),
            question(&["yes", "no"], false),
            question(&["mon", "tue", "wed"], false),
            question(&["x", "y"], false),
        ];
        let keys = answer_keys(
            &qs,
            &[pick(&[0]), pick(&[1]), typed("next week"), pick(&[1])],
        )
        .unwrap();
        assert_eq!(
            names(&keys),
            [
                "Enter", // tab 1: option 1
                "Down",
                "Enter", // tab 2: option 2
                "Down",
                "Down",
                "Down",
                "'next week'",
                "Enter", // tab 3: text row
                "Down",
                "Enter", // tab 4: option 2
                "Enter", // review: Submit answers
            ]
        );
        // Text waits before its Enter; every tab waits for the next to draw.
        let text_at = keys
            .iter()
            .position(|k| matches!(k, KeyStep::Text { .. }))
            .unwrap();
        assert_eq!(keys[text_at + 1], KeyStep::Pause { ms: TEXT_PAUSE_MS });
        assert_eq!(
            keys.iter()
                .filter(|k| **k == KeyStep::Pause { ms: NEXT_PAUSE_MS })
                .count(),
            4
        );
    }

    #[test]
    fn multi_select_toggles_with_space_and_leaves_through_the_button() {
        let qs = [question(&["a", "b", "c", "d"], true)];
        let keys = answer_keys(&qs, &[pick(&[3, 1, 1])]).unwrap();
        assert_eq!(
            names(&keys),
            [
                "Down", "Space", // b
                "Down", "Down", "Space", // d
                "Down", "Down",  // text row, then the button
                "Enter", // Submit (moves to review)
                "Enter", // Submit answers
            ]
        );
        let with_text = answer_keys(
            &qs,
            &[QuestionAnswer {
                options: vec![0],
                text: Some(" other ".into()),
            }],
        )
        .unwrap();
        assert_eq!(
            names(&with_text),
            [
                "Space", // a
                "Down", "Down", "Down", "Down", "'other'", // typing ticks the row
                "Down", "Enter", // button
                "Enter",
            ]
        );
        let only_text = answer_keys(&qs, &[typed("z")]).unwrap();
        assert_eq!(
            names(&only_text),
            [
                "Down", "Down", "Down", "Down", "'z'", "Down", "Enter", "Enter"
            ]
        );
    }

    #[test]
    fn invalid_answers_are_refused() {
        let single = [question(&["a", "b"], false)];
        assert!(answer_keys(&single, &[]).is_err());
        assert!(answer_keys(&single, &[pick(&[2])]).is_err());
        assert!(answer_keys(&single, &[pick(&[0, 1])]).is_err());
        assert!(answer_keys(&single, &[pick(&[])]).is_err());
        assert!(answer_keys(&single, &[typed("   ")]).is_err());
        assert!(answer_keys(&single, &[typed("two\nlines")]).is_err());
        assert!(answer_keys(&single, &[typed(&"x".repeat(MAX_TEXT_CHARS + 1))]).is_err());
        let both = QuestionAnswer {
            options: vec![0],
            text: Some("x".into()),
        };
        assert!(answer_keys(&single, &[both]).is_err());
        let mut preview = question(&["a", "b"], false);
        preview.free_text = false;
        assert!(answer_keys(&[preview], &[typed("x")]).is_err());
        let multi = [question(&["a"], true)];
        assert!(answer_keys(&multi, &[pick(&[])]).is_err());
        assert!(answer_keys(&[], &[]).is_err());
    }

    #[test]
    fn prompt_names_the_first_question() {
        let mut q = question(&["a"], false);
        q.header = "가격 기준".into();
        q.question = "어떤 기준으로 가격을 정할까요?".into();
        assert_eq!(
            question_prompt(&[q.clone(), q.clone()]),
            "[가격 기준] 어떤 기준으로 가격을 정할까요? (+1)"
        );
        q.header.clear();
        assert_eq!(question_prompt(&[q]), "어떤 기준으로 가격을 정할까요?");
    }

    #[test]
    fn the_screen_must_show_a_fresh_form() {
        let qs = [question(
            &["선박 납품가 기준 (Recommended)", "분야별 따로"],
            false,
        )];
        let screen = "← ☐ 가격 기준 ☐ 외부 배포 ✔ Submit →\n\n어떤 기준?\n\n\
                      ❯ 1. 선박 납품가 기준 (Recommended)\n  2. 분야별 따로\n  3. Type something.\n\n\
                      Enter to select · Tab/Arrow keys to navigate · Esc to cancel\n";
        assert!(form_on_screen(screen, &qs));
        // The pointer moved, or the form is gone.
        let moved = screen
            .replace("❯ 1.", "  1.")
            .replace("  2. 분야", "❯ 2. 분야");
        assert!(!form_on_screen(&moved, &qs));
        assert!(!form_on_screen("$ ", &qs));
        // Multi-select rows draw a check box after the number.
        let multi = [question(&["a long option label here"], true)];
        assert!(form_on_screen(
            "❯ 1. [ ] a long option label here\nEnter to select",
            &multi
        ));
    }

    /// The form of the 2026-11-08 field report as a Windows console showed
    /// it (Claude Code 2.1.318): the transcript above echoes the prompt
    /// with a pointer, "Chat about this" sits below the list.
    const FIELD_FORM: &str = "\
❯ Read q.json in the current folder and call the AskUserQuestion tool once
  Read 1 file (ctrl+o to expand)
────────────────────────────────────────
 ☐ 빌드 장소

│ HD 현장 시험실 시험판(7b 2e57d2fa) 한 벌을 어디서 빌드할까요?

❯ 1. PC B 예외 한 벌 (Recommended)
     이번 시험실판 한 벌만 PC B 줄 3에서 빌드.
  2. PC C에서 빌드
     블루스크린(CPU·BIOS 의심) 위험을 안고 빌드.
  3. 다른 PC 마련
     Unity가 설치된 다른 PC를 대표님이 정해 주심.
  4. Type something.
────────────────────────────────────────
  5. Chat about this

Enter to select · ↑/↓ to navigate · Esc to cancel";

    fn field_question() -> Vec<Question> {
        vec![question(
            &[
                "PC B 예외 한 벌 (Recommended)",
                "PC C에서 빌드",
                "다른 PC 마련",
            ],
            false,
        )]
    }

    #[test]
    fn a_fresh_form_is_found_under_an_echoed_prompt() {
        let qs = field_question();
        assert!(form_on_screen(FIELD_FORM, &qs));
        assert!(form_still_open(FIELD_FORM, &qs));
        // The typed answer on the text row, the pointer on it.
        let typed = FIELD_FORM.replace("❯ 1. PC B", "  1. PC B").replace(
            "  4. Type something.",
            "❯ 4. PC B,C모두 신출물 빌드 가능하잖아 왜 뭐가금지시킨거야?",
        );
        assert!(!form_on_screen(&typed, &qs));
        assert!(text_on_screen(
            &typed,
            "PC B,C모두 신출물 빌드 가능하잖아 왜 뭐가금지시킨거야?"
        ));
        // Not on the fresh form, nor on a row without the pointer.
        assert!(!text_on_screen(FIELD_FORM, "PC B,C모두 신출물"));
        assert!(!text_on_screen(
            &typed.replace("❯ 4.", "  4."),
            "PC B,C모두 신출물"
        ));
        assert!(!text_on_screen(&typed, ""));
    }

    #[test]
    fn a_dialog_over_the_form_takes_the_keys() {
        let qs = field_question();
        // Another prompt drawn below the form's footer.
        let notice = format!(
            "{FIELD_FORM}\n\n  Teach auto mode about your environment?\n\n  ❯ 1. Yes\n    2. Not now\n\n  Enter to confirm · Esc to cancel"
        );
        assert!(!form_on_screen(&notice, &qs));
        // A dialog instead of the form.
        let other = "  Teach auto mode about your environment?\n  ❯ 1. Yes\n    2. Not now\n  Enter to confirm · Esc to cancel";
        assert!(!form_on_screen(other, &qs));
        assert!(!form_still_open(other, &qs));
    }

    #[test]
    fn an_answered_form_is_gone_from_the_screen() {
        let qs = field_question();
        let answered = "\
● User answered Claude's questions:
  ⎿  · HD 현장 시험실 시험판 → PC B 예외 한 벌 (Recommended)

* Forging… (5s · thinking)
────────────────────────
❯
────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle)";
        assert!(!form_still_open(answered, &qs));
        assert!(!form_on_screen(answered, &qs));
        // The review screen of a longer form is still the form.
        let review = "Review your answers\n  · Q? → x\nReady to submit your answers?\n❯ 1. Submit answers\n  2. Cancel";
        assert!(form_still_open(review, &qs));
    }
}
