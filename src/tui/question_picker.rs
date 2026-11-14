//! Interactive structured questions from the model.
use super::{picker, render::Frame};
use crate::{
    agent::UserQuestionRequest,
    theme::{ColorMode, Theme},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

pub struct Pending {
    request: UserQuestionRequest,
    index: usize,
    selected: usize,
    checked: Vec<bool>,
    custom: String,
    answers: Vec<String>,
    chat_mode: bool,
}
impl Pending {
    pub fn new(request: UserQuestionRequest) -> Self {
        let count = request.prompt.questions[0].options.len();
        Self {
            request,
            index: 0,
            selected: 0,
            checked: vec![false; count],
            custom: String::new(),
            answers: Vec::new(),
            chat_mode: false,
        }
    }
    pub fn frame(&self, width: usize, height: usize, mode: ColorMode) -> Frame {
        let theme = Theme::default();
        let q = &self.request.prompt.questions[self.index];
        let mut rows = vec![
            picker::heading(
                &format!(
                    "  Вопрос {} из {} · {}",
                    self.index + 1,
                    self.request.prompt.questions.len(),
                    super::plain(&q.question)
                ),
                width,
                &theme,
                mode,
            ),
            String::new(),
        ];
        for (i, opt) in q.options.iter().enumerate() {
            let mark = if q.multi_select {
                if self.checked[i] {
                    "[x] "
                } else {
                    "[ ] "
                }
            } else {
                ""
            };
            rows.push(picker::choice(
                self.selected == i,
                &format!("{}. {}{}", i + 1, mark, super::plain(&opt.label)),
                &format!("  {}", super::plain(&opt.description)),
                width,
                &theme,
                mode,
            ));
        }
        rows.push(picker::choice(
            self.selected == q.options.len(),
            &format!(
                "{}. Свой ответ: {}",
                q.options.len() + 1,
                super::plain(&self.custom)
            ),
            "",
            width,
            &theme,
            mode,
        ));
        if !q.multi_select {
            rows.push(picker::choice(
                self.selected == q.options.len() + 1,
                "Обсудить в чате",
                "",
                width,
                &theme,
                mode,
            ));
        }
        rows.push(String::new());
        rows.push(picker::text(
            if q.multi_select {
                "  ↑↓ выбор   пробел отметить   enter далее   esc отмена"
            } else {
                "  ↑↓ выбор   enter далее   печатай свой ответ   esc отмена"
            },
            width,
            theme.placeholder,
            mode,
        ));
        if rows.len() > height && height >= 3 {
            let heading = rows.remove(0);
            let footer = rows.pop().unwrap_or_default();
            rows.retain(|row| !super::plain(row).trim().is_empty());
            let capacity = height - 2;
            let start = (self.selected + 1)
                .saturating_sub(capacity)
                .min(rows.len().saturating_sub(capacity));
            rows = rows.into_iter().skip(start).take(capacity).collect();
            rows.insert(0, heading);
            rows.push(footer);
        }
        Frame {
            lines: picker::constrain(rows, height),
            cursor: None,
        }
    }
    /// Returns Some(true) to submit, Some(false) to cancel.
    pub fn answer(&mut self, event: &Event) -> Option<bool> {
        if let Event::Paste(text) = event {
            self.custom.push_str(text);
            self.selected = self.request.prompt.questions[self.index].options.len();
            return None;
        }
        let Event::Key(key) = event else {
            return None;
        };
        if key.kind == KeyEventKind::Release {
            return None;
        }
        let q = &self.request.prompt.questions[self.index];
        let count = q.options.len();
        let choices = count + 1 + usize::from(!q.multi_select);
        match key.code {
            KeyCode::Esc => return Some(false),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Some(false)
            }
            KeyCode::Up => self.selected = (self.selected + choices - 1) % choices,
            KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % choices,
            KeyCode::Char(' ') if q.multi_select && self.selected < count => {
                self.checked[self.selected] = !self.checked[self.selected]
            }
            KeyCode::Backspace if self.selected == count => {
                self.custom.pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.selected = count;
                self.custom.push(c);
            }
            KeyCode::Enter => {
                if !q.multi_select && self.selected == count + 1 {
                    self.chat_mode = true;
                    return None;
                }
                let answer = if q.multi_select {
                    let mut choices: Vec<String> = q
                        .options
                        .iter()
                        .zip(&self.checked)
                        .filter_map(|(o, checked)| checked.then_some(o.label.clone()))
                        .collect();
                    if !self.custom.trim().is_empty() {
                        choices.push(self.custom.trim().to_string());
                    }
                    if choices.is_empty() {
                        return None;
                    }
                    choices.join(", ")
                } else if self.selected == count {
                    if self.custom.trim().is_empty() {
                        return None;
                    }
                    self.custom.trim().to_string()
                } else {
                    q.options[self.selected].label.clone()
                };
                self.answers.push(answer);
                self.index += 1;
                if self.index == self.request.prompt.questions.len() {
                    return Some(true);
                }
                self.selected = 0;
                self.custom.clear();
                self.checked = vec![false; self.request.prompt.questions[self.index].options.len()];
            }
            _ => {}
        }
        None
    }
    pub fn in_chat(&self) -> bool {
        self.chat_mode
    }
    pub fn finish_chat(mut self, response: String) {
        self.answers
            .push(format!("User replied in chat: {response}"));
        while self.answers.len() < self.request.prompt.questions.len() {
            self.answers
                .push("Not answered; user replied in chat".into());
        }
        let _ = self.request.reply.send(Some(self.answers));
    }
    pub fn finish(self, submitted: bool) {
        let _ = self.request.reply.send(submitted.then_some(self.answers));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use serde_json::json;

    #[test]
    fn questions_collect_choices_and_custom_text() {
        let prompt = crate::agent::agency::QuestionPrompt::from_value(&json!({"questions":[
            {"question":"Первый?","multiSelect":true,"options":[{"label":"A"},{"label":"B"}]},
            {"question":"Второй?","options":[{"label":"Да"},{"label":"Нет"}]}
        ]}))
        .unwrap();
        let (reply, mut receive) = tokio::sync::oneshot::channel();
        let mut pending = Pending::new(UserQuestionRequest { prompt, reply });
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        pending.answer(&key(KeyCode::Char(' ')));
        pending.answer(&key(KeyCode::Down));
        pending.answer(&key(KeyCode::Char(' ')));
        assert_eq!(pending.answer(&key(KeyCode::Enter)), None);
        pending.answer(&Event::Paste("свой ответ".into()));
        assert_eq!(pending.answer(&key(KeyCode::Enter)), Some(true));
        pending.finish(true);
        assert_eq!(
            receive.try_recv().unwrap(),
            Some(vec!["A, B".to_string(), "свой ответ".to_string()])
        );
    }
    #[test]
    fn single_question_can_be_answered_in_chat() {
        let prompt = crate::agent::agency::QuestionPrompt::from_value(
            &json!({"question":"Как?","options":[{"label":"Да"}]}),
        )
        .unwrap();
        let (reply, mut receive) = tokio::sync::oneshot::channel();
        let mut pending = Pending::new(UserQuestionRequest { prompt, reply });
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        pending.answer(&key(KeyCode::Up));
        pending.answer(&key(KeyCode::Enter));
        assert!(pending.in_chat());
        pending.finish_chat("Объясню подробнее".into());
        assert_eq!(
            receive.try_recv().unwrap().unwrap(),
            vec!["User replied in chat: Объясню подробнее"]
        );
    }
}
