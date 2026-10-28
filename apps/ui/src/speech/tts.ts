/** Spoken alerts (NTF-04) through the Web Speech API, when available. */
export function speak(text: string, lang: string): void {
  if (typeof window === 'undefined' || !('speechSynthesis' in window)) return;
  try {
    const utterance = new SpeechSynthesisUtterance(text);
    utterance.lang = lang;
    window.speechSynthesis.speak(utterance);
  } catch {
    // Speech synthesis can be blocked (no voices, autoplay policy); alerts are still shown.
  }
}
