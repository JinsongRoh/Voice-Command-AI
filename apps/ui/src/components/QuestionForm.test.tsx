import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import type { Decision } from '../protocol/types';
import { questionScreenText } from '../speech/handsFree';
import { localSession } from '../test/fixtures';
import { FakeTransport, defaultResponse } from '../test/fakeTransport';
import { renderWithApp } from '../test/render';
import { DecisionSheet } from './DecisionSheet';
import { QuestionForm } from './QuestionForm';
import { splitRecommended, toAnswer } from './questionDraft';

/** The form from the report: a single choice, then a multiple choice. */
const decision: Decision = {
  id: 'claude:abc:toolu_q',
  prompt: '[가격 기준] 어떤 기준으로 가격을 정할까요? (+1)',
  options: [],
  questions: [
    {
      header: '가격 기준',
      question: '어떤 기준으로 가격을 정할까요?',
      options: [
        { label: '선박 납품가 기준 (Recommended)', description: '가장 익숙한 기준' },
        { label: '분야별 따로', description: '' },
        { label: '고객 미팅 뒤에 정함', description: '' },
      ],
      multi_select: false,
      free_text: true,
    },
    {
      header: '비용 항목',
      question: 'Which costs go in?',
      options: [
        { label: 'Hardware', description: 'Boxes' },
        { label: 'Licenses', description: 'Seats' },
      ],
      multi_select: true,
      free_text: true,
    },
  ],
};

const session = localSession({ state: { type: 'awaiting_decision' } });

describe('QuestionForm', () => {
  it('shows a card per question and submits once every question is answered', async () => {
    const { transport } = renderWithApp(<QuestionForm session={session} decision={decision} />);
    const cards = screen.getAllByTestId('question-card');
    expect(cards).toHaveLength(2);
    expect(cards[0]).toHaveTextContent('가격 기준');
    expect(cards[0]).toHaveTextContent('어떤 기준으로 가격을 정할까요?');
    expect(cards[0]).toHaveTextContent('가장 익숙한 기준');
    // The "(Recommended)" suffix becomes a marker.
    const first = within(cards[0]!).getAllByRole('radio')[0]!;
    expect(first).toHaveTextContent('선박 납품가 기준Recommended');
    expect(within(first).getByTestId('question-recommended')).toBeInTheDocument();
    // Multiple choice: check boxes.
    expect(within(cards[1]!).getAllByRole('checkbox')).toHaveLength(2);
    expect(cards[1]).toHaveTextContent('Choose all that apply');

    expect(screen.queryByRole('button', { name: 'Submit' })).toBeNull();
    expect(screen.getByText('0 of 2 answered')).toBeInTheDocument();
    await userEvent.click(within(cards[0]!).getByRole('radio', { name: /분야별 따로/ }));
    expect(screen.queryByRole('button', { name: 'Submit' })).toBeNull();
    await userEvent.click(within(cards[1]!).getByRole('checkbox', { name: /Licenses/ }));
    await userEvent.click(within(cards[1]!).getByRole('checkbox', { name: /Hardware/ }));
    await userEvent.type(within(cards[1]!).getByRole('textbox'), 'Travel');
    expect(screen.getByText('2 of 2 answered')).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Submit' }));
    await waitFor(() =>
      expect(transport.bodies('send_input')).toEqual([
        {
          type: 'send_input',
          session_id: 's1',
          input: {
            type: 'answer',
            decision_id: 'claude:abc:toolu_q',
            answers: [
              { options: [1], text: null },
              { options: [0, 1], text: 'Travel' },
            ],
          },
        },
      ]),
    );
  });

  it('takes typed text instead of an option for a single choice', async () => {
    renderWithApp(<QuestionForm session={session} decision={decision} />);
    const [single] = screen.getAllByTestId('question-card');
    const radio = within(single!).getAllByRole('radio')[0]!;
    await userEvent.click(radio);
    expect(radio).toHaveAttribute('aria-checked', 'true');
    // Typing replaces the picked option.
    await userEvent.type(within(single!).getByRole('textbox'), '다음 주');
    expect(radio).toHaveAttribute('aria-checked', 'false');
    const q = decision.questions![0]!;
    expect(toAnswer(q, { options: [], text: ' 다음 주 ' })).toEqual({
      options: [],
      text: '다음 주',
    });
    expect(toAnswer(q, { options: [2], text: '' })).toEqual({ options: [2], text: null });
  });

  it('sends the person to the terminal when the answers do not get through', async () => {
    const transport = new FakeTransport((msg) =>
      msg.type === 'send_input'
        ? { type: 'error', code: 'conflict', message: 'that question is no longer pending' }
        : defaultResponse(msg),
    );
    const single: Decision = { ...decision, questions: decision.questions!.slice(0, 1) };
    renderWithApp(<QuestionForm session={session} decision={single} />, { transport });
    await userEvent.click(screen.getAllByRole('radio')[0]!);
    await userEvent.click(screen.getByRole('button', { name: 'Submit' }));
    const alert = await screen.findByTestId('question-check-terminal');
    expect(alert).toHaveTextContent('Check the terminal');
    expect(alert).toHaveTextContent('that question is no longer pending');
  });

  it('keeps answers the connection lost, to send again', async () => {
    let drops = 1;
    const transport = new FakeTransport((msg) => {
      if (msg.type === 'send_input' && drops > 0) {
        drops -= 1;
        throw new Error('disconnected');
      }
      return defaultResponse(msg);
    });
    const single: Decision = { ...decision, questions: decision.questions!.slice(0, 1) };
    renderWithApp(<QuestionForm session={session} decision={single} />, { transport });
    await userEvent.type(screen.getByRole('textbox'), 'PC B,C 모두 빌드 가능');
    await userEvent.click(screen.getByRole('button', { name: 'Submit' }));
    // Not "check the terminal": it may never have reached the PC.
    const alert = await screen.findByTestId('question-unsent');
    expect(alert).toHaveTextContent('connection to the PC dropped');
    expect(alert).toHaveTextContent('disconnected');
    expect(screen.queryByTestId('question-check-terminal')).toBeNull();
    // The typed answer is still there; one tap sends it again.
    expect(screen.getByRole('textbox')).toHaveValue('PC B,C 모두 빌드 가능');
    await userEvent.click(screen.getByRole('button', { name: 'Send again' }));
    await waitFor(() => expect(transport.bodies('send_input')).toHaveLength(2));
    const [first, second] = transport.bodies('send_input');
    expect(second).toEqual(first);
    await waitFor(() => expect(screen.queryByTestId('question-unsent')).toBeNull());
  });

  it('replaces the answer buttons of the decision card', () => {
    renderWithApp(<DecisionSheet session={session} decision={decision} />);
    const region = screen.getByRole('region', { name: 'Pending decision for payments-api' });
    expect(region).toHaveTextContent('Question from the agent');
    expect(within(region).getByTestId('question-form')).toBeInTheDocument();
    expect(within(region).queryByRole('button', { name: 'Run in shell' })).toBeNull();
  });

  it('marks recommended labels and reads questions out', () => {
    expect(splitRecommended('A (Recommended)')).toEqual({ label: 'A', recommended: true });
    expect(splitRecommended('B')).toEqual({ label: 'B', recommended: false });
    expect(questionScreenText('5번 세션', '어떤 기준?', 'ko')).toBe(
      '오번 세션에서 질문이 왔습니다. 어떤 기준. 화면에서 답해 주세요.',
    );
    expect(questionScreenText('api', 'Which price?', 'en')).toBe(
      'api asks a question: Which price. Please answer on the screen.',
    );
  });
});
